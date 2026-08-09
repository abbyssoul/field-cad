//! `HeadlessServer::replace_source` swaps the session behind the same
//! `Arc<Mutex<HeadlessServer>>` every attached transport shares. Two hazards
//! this must not have: a waiter left hanging forever, and a suppressed
//! "nothing changed" publish because the new session's state coincides with
//! the old one's (both commonly start at sequence 0 / tick 0).

use fieldcad_server::HeadlessServer;
use fieldcad_simulation::CommandPayload;
use tokio::sync::oneshot::error::TryRecvError;

#[test]
fn replace_source_disconnects_a_pending_waiter_instead_of_hanging_forever() {
    let mut server = HeadlessServer::new(fieldcad_server::default_session().unwrap());

    // `AsyncLocalDataSource::execute` always answers `Submitted` immediately;
    // nothing resolves the waiter until something calls `advance`/`poll`,
    // which this test deliberately never does — so the waiter is guaranteed
    // still pending when `replace_source` runs below.
    let (receipt, receiver) = server
        .submit_and_await(CommandPayload::Play)
        .expect("command is accepted");
    assert_eq!(
        receipt.disposition,
        fieldcad_simulation::CommandDisposition::Submitted
    );
    let mut receiver = receiver.expect("a Submitted disposition always registers a waiter");
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

    server.replace_source(fieldcad_server::default_session().unwrap());

    assert_eq!(
        receiver.try_recv(),
        Err(TryRecvError::Closed),
        "an awaiting caller must see a clean disconnect, not hang forever"
    );
}

#[test]
fn replace_source_forces_a_fresh_publish_even_when_new_state_coincides_with_old() {
    let mut server = HeadlessServer::new(fieldcad_server::default_session().unwrap());
    let mut watcher = server.subscribe_events();
    // Drain whatever the initial construction already published so the
    // assertion below is specifically about the *replace*, not leftover
    // startup events.
    watcher.drain();

    // A second `default_session()` starts at the same tick/sequence/status
    // as the first — the exact coincidence that would let a stale
    // change-detection cache suppress this publish if `EventHub::reset`
    // were not called.
    server.replace_source(fieldcad_server::default_session().unwrap());

    let events = watcher.drain();
    assert!(
        !events.is_empty(),
        "a session replace must always publish fresh state, even if it numerically matches the old session's last-published values"
    );
}
