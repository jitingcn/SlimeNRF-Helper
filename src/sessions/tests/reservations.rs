use super::super::{ConnectionState, SessionManager};
use super::{connected, fixture, peer, socket_transport, until};
use parking_lot::Mutex;
use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn serial_reservation_guards_aliases_generations_and_explicit_reconnect() {
    let (mut manager, listener) = fixture();
    manager.connect("COM37").unwrap();
    let mut reserved_peer = peer(&listener);
    connected(&mut manager, "COM37");
    manager.connect("other").unwrap();
    let mut other_peer = peer(&listener);
    connected(&mut manager, "other");
    let generation = manager.session("COM37").unwrap().generation;
    assert!(manager.reserve("COM37", Some(generation + 1)).is_err());
    let alias = if cfg!(windows) { r"\\.\com37" } else { "COM37" };
    let token = manager.reserve(alias, Some(generation)).unwrap();
    assert!(manager.connect("COM37").is_err());
    assert!(manager.reserve("COM37", None).is_err());
    assert!(manager.send_line(alias, "info").is_err());
    let mut receipt = manager.send_line("other", "help").unwrap();
    let mut bytes = [0; 5];
    other_peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"help\n");
    until(|| receipt.poll().is_some_and(|result| result.is_ok()));
    until(|| {
        let _ = manager.poll();
        manager.reservation_ready(&token).unwrap()
    });
    assert_eq!(reserved_peer.read(&mut [0; 1]).unwrap(), 0);
    assert!(manager.connect(alias).is_err());
    manager.release(&token).unwrap();
    assert_eq!(
        manager.session("COM37").unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(manager.reservation_ready(&token).is_err());
    let replacement = manager.reserve("COM37", Some(generation)).unwrap();
    assert!(manager.release(&token).is_err());
    assert!(manager.reservation_ready(&replacement).unwrap());
    manager.release(&replacement).unwrap();
    manager.connect("COM37").unwrap();
    let _new_peer = peer(&listener);
    connected(&mut manager, "COM37");
    assert!(manager.session("COM37").unwrap().generation > generation);
    assert!(manager.reserve("COM37", Some(generation)).is_err());
    if let Some(reaper) = manager.shutdown() {
        reaper.join().unwrap();
    }
}

#[test]
fn cancelling_pending_serial_reservation_never_overlaps_a_late_open() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        opened_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        Ok(Box::new(stream))
    }));
    manager.connect("pending").unwrap();
    let mut stream = peer(&listener);
    opened_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let token = manager.reserve("pending", None).unwrap();
    assert!(!manager.reservation_ready(&token).unwrap());
    manager.release(&token).unwrap();
    assert!(manager.connect("pending").is_err());
    let token = manager.reserve("pending", None).unwrap();
    assert!(!manager.reservation_ready(&token).unwrap());
    release_tx.send(()).unwrap();
    until(|| {
        let _ = manager.poll();
        manager.reservation_ready(&token).unwrap()
    });
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert!(manager.shutdown().is_none());
    assert!(manager.reservation_ready(&token).is_err());
    assert!(manager.release(&token).is_err());
    assert!(manager.reserve("pending", None).is_err());
    assert!(manager.connect("pending").is_err());
}

#[test]
fn serial_tokens_are_manager_scoped_even_for_unconnected_ports() {
    let (mut first, _) = fixture();
    let (mut second, _) = fixture();
    let first_token = first.reserve("same", None).unwrap();
    let second_token = second.reserve("same", None).unwrap();
    assert!(first.reservation_ready(&first_token).unwrap());
    assert!(second.reservation_ready(&second_token).unwrap());
    assert!(second.release(&first_token).is_err());
    assert!(first.reservation_ready(&second_token).is_err());
    first.release(&first_token).unwrap();
    second.release(&second_token).unwrap();
}
