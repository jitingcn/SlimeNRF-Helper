use super::{ShutdownHost, ShutdownPayload};
use crate::ui::test_support::background_executor;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

fn gated_shutdown_payload() -> (ShutdownPayload, mpsc::Sender<()>, Arc<AtomicUsize>) {
    let (release, gate) = mpsc::channel();
    let joined = Arc::new(AtomicUsize::new(0));
    let completed = joined.clone();
    let worker = std::thread::spawn(move || {
        gate.recv().unwrap();
        completed.fetch_add(1, Ordering::SeqCst);
    });
    (
        ShutdownPayload {
            replay: vec![worker],
            ..ShutdownPayload::default()
        },
        release,
        joined,
    )
}

#[test]
fn shutdown_spawn_failure_defers_payload_without_blocking_and_host_joins_once() {
    let host = Arc::new(ShutdownHost::default());
    use futures_util::FutureExt as _;
    let (mut payload, release, joined) = gated_shutdown_payload();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("accepted.txt");
    let target = path.clone();
    let (release_file, file_gate) = mpsc::channel();
    let executor = background_executor();
    let file = executor
        .spawn(async move {
            file_gate.recv().unwrap();
            std::fs::write(target, b"accepted work completed").unwrap();
        })
        .shared();
    payload.files.push(file.clone());
    payload._background = Some(executor.clone());
    let completion_task = executor.spawn(file);
    drop(completion_task);
    let mut permission = host.start_with(payload, |_, _| {
        Err(std::io::Error::other("fixture spawn failure"))
    });
    assert!(matches!(
        permission.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert_eq!(joined.load(Ordering::SeqCst), 0);
    let (finished, completion) = mpsc::channel();
    let draining = host.clone();
    let waiter = std::thread::spawn(move || {
        draining.finish();
        finished.send(()).unwrap();
    });
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_file.send(()).unwrap();
    completion.recv_timeout(Duration::from_secs(2)).unwrap();
    waiter.join().unwrap();
    host.finish();
    assert_eq!(joined.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(path).unwrap(), b"accepted work completed");
}

#[test]
fn shutdown_worker_and_host_share_exactly_one_cleanup_payload() {
    let host = Arc::new(ShutdownHost::default());
    let (payload, release, joined) = gated_shutdown_payload();
    let mut permission = host.start(payload);
    assert!(matches!(
        permission.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    host.finish();
    permission.try_recv().unwrap();
    host.finish();
    assert_eq!(joined.load(Ordering::SeqCst), 1);
}
