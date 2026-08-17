//! Proves the broadcast hub's three acceptance criteria from
//! `docs/tasks/session-events-and-queue-control.md`: two independent
//! watchers both observe the same transitions without draining each other,
//! a watcher that falls behind gets a resync marker rather than an
//! unbounded backlog or silence, and `submit_and_await`'s waiter still
//! resolves correctly once a command's completion is folded through
//! `EventHub` rather than the old single destructive drain.

use std::time::{Duration, Instant};

use fieldcad_core::{ObjectShape, ObjectSpec, Transform, WorldCommand};
use fieldcad_server::{HeadlessServer, SessionEvent, WatchEvent};
use fieldcad_simulation::{CommandDisposition, CommandEvent, CommandPayload};

fn create_object(name: &str) -> CommandPayload {
    CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(
        ObjectSpec::new(name)
            .with_transform(Transform::default())
            .with_shape(ObjectShape::default()),
    )])
}

/// Mirrors `headless_session.rs`'s helper: submit and poll/drain until the
/// worker responds.
fn submit_and_wait(server: &mut HeadlessServer, payload: CommandPayload) -> CommandEvent {
    server.submit(payload).expect("command is accepted");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        server.advance(Duration::ZERO).expect("advance succeeds");
        if let Some(event) = server.drain_events().into_iter().next() {
            return event;
        }
        assert!(Instant::now() < deadline, "worker did not respond");
        std::thread::yield_now();
    }
}

#[test]
fn two_independent_watchers_both_observe_the_same_command_terminal_event() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let mut first = server.subscribe_events();
    let mut second = server.subscribe_events();

    submit_and_wait(&mut server, create_object("point charge"));

    // Neither watcher drains the other's view of the same transition.
    for watcher in [&mut first, &mut second] {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match watcher.try_next() {
                Some(WatchEvent::Session(SessionEvent::CommandTerminal(_))) => break,
                Some(_) => {}
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "watcher never observed the command-terminal transition"
                    );
                    std::thread::yield_now();
                }
            }
        }
    }
}

#[test]
fn a_lagging_watcher_receives_a_resync_marker_then_reads_cleanly() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let mut watcher = server.subscribe_events();

    // Overflow the hub's bounded capacity for this one watcher, without
    // ever draining it, by producing far more transitions than it retains.
    for index in 0..400u32 {
        submit_and_wait(&mut server, create_object(&format!("object {index}")));
    }

    let saw_lagged = watcher
        .drain()
        .iter()
        .any(|event| matches!(event, WatchEvent::Lagged));
    assert!(
        saw_lagged,
        "a watcher idle across 400 transitions must observe a resync marker"
    );

    // Recovery: every authoritative resource still reads cleanly after a
    // lag, with no backlog to work through and nothing left unbounded.
    assert_eq!(server.world().objects().len(), 400);
    assert!(server.get_queue().pending.is_empty());
}

#[test]
fn submit_and_await_resolves_a_queued_then_later_applied_command() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let play = submit_and_wait(&mut server, CommandPayload::Play);
    assert!(matches!(play, CommandEvent::Completed(_)));

    let (receipt, waiter) = server
        .submit_and_await(create_object("queued object"))
        .expect("submission accepted");
    assert_eq!(receipt.disposition, CommandDisposition::Submitted);
    let mut waiter = waiter.expect("a non-blocking submission registers a waiter");

    // Drive ticks until the waiter resolves -- proving the
    // `async_source.rs` fix (a `Queued` disposition is not terminal) is
    // visible end-to-end through `HeadlessServer`'s waiter path, now folded
    // through `EventHub::publish_command_event` rather than the old
    // per-`drain_events`-caller resolution.
    let deadline = Instant::now() + Duration::from_secs(2);
    let event = loop {
        server
            .advance(Duration::from_millis(100))
            .expect("advance succeeds");
        server.drain_events();
        match waiter.try_recv() {
            Ok(event) => break event,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "the queued command's waiter never resolved"
                );
                std::thread::yield_now();
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                panic!("the waiter's sender was dropped without resolving");
            }
        }
    };
    assert!(matches!(
        event,
        CommandEvent::Completed(ref receipt) if receipt.disposition == CommandDisposition::Applied
    ));
}

#[test]
fn a_closed_hub_yields_closed_on_try_next_and_drain() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let server = HeadlessServer::new(source);
    let mut watcher = server.subscribe_events();

    // Drop the server, which drops the EventHub and its broadcast sender.
    drop(server);

    // try_next on a closed hub must yield Some(Closed), not None
    // (indistinguishable from Empty as it was before BE-22).
    assert!(
        matches!(watcher.try_next(), Some(WatchEvent::Closed)),
        "try_next must report Closed"
    );

    // Subsequent calls continue to yield Closed (the broadcast receiver's
    // own behaviour), not loop forever.
    assert!(
        matches!(watcher.try_next(), Some(WatchEvent::Closed)),
        "subsequent try_next still reports Closed"
    );

    // drain must stop at Closed without looping.
    let events = watcher.drain();
    assert!(
        events.iter().any(|e| matches!(e, WatchEvent::Closed)),
        "drain must include the Closed event"
    );
    assert_eq!(
        events.len(),
        1,
        "drain returns exactly one Closed after the hub is gone"
    );
}
