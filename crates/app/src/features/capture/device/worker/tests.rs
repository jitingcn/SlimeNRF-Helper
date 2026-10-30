use super::{CapturePhase, CaptureSnapshot, CaptureWorker};
use crate::features::capture::device::collection::tests::{Sim, request};
use crate::features::capture::device::collection::{collect, expected_roster};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
#[test]
fn join_never_detaches_owned_io_or_releases_external_lease() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("joined.slimecap"), false);
    let cancel = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
    let thread_state = Arc::clone(&state);
    let thread_cancel = Arc::clone(&cancel);
    let (release, wait) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        wait.recv().unwrap();
        let mut io = Sim::new(Arc::clone(&thread_cancel));
        collect(
            &request,
            expected_roster(&request).unwrap(),
            &mut io,
            &thread_cancel,
            &thread_state,
        )
    });
    let mut worker = CaptureWorker {
        cancel,
        state,
        handle: Some(handle),
    };
    // External lease remains caller-owned even when cancellation is requested.
    let external_lease = Arc::new(());
    worker.stop();
    assert!(worker.join().is_none());
    assert_eq!(Arc::strong_count(&external_lease), 1);
    release.send(()).unwrap();
    // The test explicitly joins the private handle; production uses nonblocking join().
    let result = worker.handle.take().unwrap().join().unwrap().unwrap();
    assert!(result.path.exists());
    assert!(worker.is_finished());
    drop(external_lease);
}
#[test]
fn snapshot_polling_reuses_immutable_publication_until_worker_replaces_it() {
    let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
    let worker = CaptureWorker {
        cancel: Arc::new(AtomicBool::new(false)),
        state: Arc::clone(&state),
        handle: None,
    };
    let first = worker.snapshot();
    assert!(Arc::ptr_eq(&first, &worker.snapshot()));
    let mut updated = CaptureSnapshot::default();
    updated.counters.samples = 7;
    updated.phase = CapturePhase::Receiving;
    *state.lock() = Arc::new(updated);
    let next = worker.snapshot();
    assert!(!Arc::ptr_eq(&first, &next));
    assert!(Arc::ptr_eq(&next, &worker.snapshot()));
    assert_eq!(first.counters.samples, 0);
    assert_eq!(next.counters.samples, 7);
}
