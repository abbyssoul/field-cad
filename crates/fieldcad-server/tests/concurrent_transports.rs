//! Proves the bug this plan exists to fix stays fixed: once two transports
//! share one `HeadlessServer` behind `Arc<std::sync::Mutex<_>>` — an
//! embedded UI's per-frame pump and an MCP tool call's own wait loop, say —
//! neither can silently steal the other's command-completion event out of
//! `AsyncLocalDataSource`'s single destructively-drained queue, and neither
//! can hang forever waiting for an event the other side already consumed.
//!
//! Without `HeadlessServer::submit_and_await`'s waiter registration (i.e.
//! against the older design that scanned each `drain_events()` call's
//! returned `Vec` for a matching `CommandId`), this test is exactly the
//! scenario that hangs or cross-delivers: a tight "desktop" pumper draining
//! concurrently with a task waiting on its own submission.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use fieldcad_core::{ObjectShape, ObjectSpec, Transform, WorldCommand};
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{CommandDisposition, CommandEvent, CommandPayload};
use glam::DVec3;

/// Stands in for the desktop app's per-frame `poll` + `drain_command_events`
/// — a tight, independent pumper that knows nothing about anyone else's
/// in-flight commands and drains unconditionally.
fn spawn_pumper(model: Arc<Mutex<HeadlessServer>>, stop: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let mut server = model.lock().unwrap();
            let _ = server.advance(Duration::ZERO);
            server.drain_events();
            drop(server);
            std::thread::yield_now();
        }
    });
}

async fn submit_and_await(
    model: &Arc<Mutex<HeadlessServer>>,
    payload: CommandPayload,
) -> CommandEvent {
    let (receipt, waiter) = {
        let mut server = model.lock().unwrap();
        server
            .submit_and_await(payload)
            .expect("submission accepted")
    };
    let Some(waiter) = waiter else {
        panic!("expected a non-blocking submission with a waiter, got {receipt:?}");
    };
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("command timed out — the waiter was never fulfilled")
        .expect("the waiter's sender was dropped without resolving")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pumpers_do_not_hang_or_cross_deliver_completions() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let model = Arc::new(Mutex::new(HeadlessServer::new(source)));

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Two independent pumpers — desktop's per-frame drain plus whatever a
    // second transport's own loop might also do — racing each other and
    // every `submit_and_await` call below.
    spawn_pumper(model.clone(), stop.clone());
    spawn_pumper(model.clone(), stop.clone());

    // Many commands in flight at once, from what is effectively "two
    // sources" sharing one `HeadlessServer` — the exact setup where a
    // private-per-source `CommandSequencer` and a per-caller drain used to
    // let one call see a completely different command's receipt.
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..50u32 {
        let model = model.clone();
        tasks.spawn(async move {
            let payload = CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(
                ObjectSpec::new(format!("object-{index}"))
                    .with_transform(Transform::at(DVec3::new(f64::from(index), 0.0, 0.0)).unwrap())
                    .with_shape(ObjectShape::point(0.05).unwrap()),
            )]);
            let event = submit_and_await(&model, payload).await;
            (index, event)
        });
    }

    let mut completed = 0;
    while let Some(result) = tasks.join_next().await {
        let (index, event) = result.expect("task did not panic");
        match event {
            CommandEvent::Completed(_) => completed += 1,
            CommandEvent::Failed { error, .. } => {
                panic!("object-{index} was rejected unexpectedly: {error}")
            }
            CommandEvent::Cancelled(_) => {
                panic!("object-{index} was cancelled unexpectedly")
            }
        }
    }
    assert_eq!(
        completed, 50,
        "every concurrently submitted command resolved"
    );

    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    let world = model.lock().unwrap().world();
    assert_eq!(
        world.objects().len(),
        50,
        "every command actually landed in the world, not just resolved its waiter"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_receiver_is_pruned_on_the_next_publish() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let (_receipt, waiter) = server
        .submit_and_await(CommandPayload::Play)
        .expect("play submits");
    let waiter = waiter.expect("play on a paused session is non-blocking");
    assert_eq!(server.waiter_count(), 1, "waiter is registered");

    // Drop the receiver as an MCP timeout would.
    drop(waiter);

    // The next advance triggers publish(), which should prune the closed
    // sender from the waiters map.
    let _ = server.advance(Duration::ZERO);
    assert_eq!(
        server.waiter_count(),
        0,
        "the orphaned waiter was pruned by publish()"
    );

    // Also verify: a second submission still resolves normally when the
    // receiver is actually awaited, to prove the prune didn't break the
    // normal path.
    let (receipt, waiter) = server
        .submit_and_await(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(
                ObjectSpec::new("prune-safety-check")
                    .with_transform(Transform::at(DVec3::ZERO).unwrap())
                    .with_shape(ObjectShape::point(0.1).unwrap()),
            ),
        ]))
        .expect("submission accepted");
    assert_eq!(receipt.disposition, CommandDisposition::Submitted);
    let mut waiter = waiter.expect("non-blocking submission registers a waiter");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        server
            .advance(Duration::from_millis(100))
            .expect("advance succeeds");
        server.drain_events();
        match waiter.try_recv() {
            Ok(event) => {
                assert!(matches!(event, CommandEvent::Completed(_)));
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "normal waiter never resolved"
                );
                std::thread::yield_now();
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                panic!("normal waiter closed unexpectedly");
            }
        }
    }
    assert_eq!(
        server.waiter_count(),
        0,
        "the resolved waiter was also properly removed"
    );
}
