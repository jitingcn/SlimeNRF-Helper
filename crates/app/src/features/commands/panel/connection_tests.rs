use super::CommandConnection;
use crate::domain::DeviceRole;
use crate::features::commands::policy::{CommandContext, CommandSnapshot, RouteEvidence, presets};
use crate::sessions::test_support::{
    completed, connected, fixture, peer, serial_open_error, socket_transport, stopped,
    with_socket_factory,
};
use crate::sessions::{ConnectionState, SessionManager, WriteReceipt};
use parking_lot::Mutex;
use std::io::{self, Read};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

// Keep the selected command in the consumer across asynchronous opens and
// explicit retries; CommandConnection owns only transport readiness.
struct CommandConsumer {
    connection: CommandConnection,
    draft: String,
    authorized: usize,
    receipt: Option<WriteReceipt>,
}

impl CommandConsumer {
    fn new(port: &str, draft: &str) -> Self {
        Self {
            connection: CommandConnection {
                port: port.into(),
                generation: None,
            },
            draft: draft.into(),
            authorized: 0,
            receipt: None,
        }
    }

    fn submit_once(&mut self, manager: &mut SessionManager) -> Result<bool, String> {
        if self.receipt.is_some() || !self.connection.ready(manager)? {
            return Ok(false);
        }
        let session = manager.session(&self.connection.port).unwrap();
        let evidence = RouteEvidence::Direct {
            port: &self.connection.port,
            generation: session.generation,
            connected: session.state == ConnectionState::Connected,
            role: DeviceRole::Unknown,
        };
        let preset = presets(CommandContext::Direct(DeviceRole::Unknown))
            .find(|preset| preset.command == self.draft)
            .expect("consumer draft selects a safe generic command");
        let snapshot = CommandSnapshot::capture(preset, "", evidence)?;
        snapshot.authorize(evidence, false)?;
        self.authorized += 1;
        self.receipt = Some(manager.send_line(snapshot.port(), snapshot.command())?);
        Ok(true)
    }
}

#[test]
fn command_connection_waits_for_opened_and_submits_retained_draft_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let mut manager = with_socket_factory(move |_| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        let stream = socket_transport(address)?;
        opened_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        Ok(stream)
    });
    let mut consumer = CommandConsumer::new("command", "help");
    assert!(consumer.connection.ready(&manager).is_err());
    assert!(consumer.submit_once(&mut manager).is_err());
    consumer.connection.start(&mut manager).unwrap();
    let generation = manager.session("command").unwrap().generation;
    assert_eq!(consumer.connection.generation, Some(generation));
    let mut stream = peer(&listener);
    opened_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = manager.poll();
    assert!(!consumer.connection.ready(&manager).unwrap());
    assert!(!consumer.submit_once(&mut manager).unwrap());
    assert_eq!(consumer.authorized, 0);
    assert!(consumer.receipt.is_none());
    assert_eq!(consumer.draft, "help");

    // Repeated requests attach to the opening generation instead of opening
    // another worker, including a second independent command intent.
    consumer.connection.start(&mut manager).unwrap();
    let mut attached = CommandConsumer::new("command", "info");
    attached.connection.start(&mut manager).unwrap();
    assert_eq!(attached.connection.generation, Some(generation));
    assert!(!attached.connection.ready(&manager).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stream.set_nonblocking(true).unwrap();
    assert_eq!(
        stream.read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    stream.set_nonblocking(false).unwrap();

    release_tx.send(()).unwrap();
    connected(&mut manager, "command");
    assert!(consumer.connection.ready(&manager).unwrap());
    consumer.connection.start(&mut manager).unwrap();
    assert_eq!(consumer.connection.generation, Some(generation));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(consumer.submit_once(&mut manager).unwrap());
    assert!(!consumer.submit_once(&mut manager).unwrap());
    completed(consumer.receipt.as_mut().unwrap()).unwrap();
    assert!(!consumer.submit_once(&mut manager).unwrap());
    assert_eq!(consumer.authorized, 1);
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"help\n");
}

#[test]
fn command_connection_failed_open_preserves_draft_for_explicit_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let available = Arc::new(AtomicBool::new(false));
    let factory_available = Arc::clone(&available);
    let mut manager = with_socket_factory(move |port| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        let stream = socket_transport(address)?;
        if !factory_available.load(Ordering::SeqCst) {
            return Err(serial_open_error(
                port,
                &serialport::Error::new(serialport::ErrorKind::NoDevice, "拒绝访问"),
            ));
        }
        Ok(stream)
    });
    let mut consumer = CommandConsumer::new("retry", "help");
    consumer.connection.start(&mut manager).unwrap();
    let failed_generation = consumer.connection.generation.unwrap();
    let mut failed_stream = peer(&listener);
    stopped(&mut manager, "retry");
    assert_eq!(
        manager.session("retry").unwrap().state,
        ConnectionState::Faulted
    );
    let failure = manager
        .session("retry")
        .unwrap()
        .last_error
        .as_ref()
        .unwrap();
    assert!(failure.contains("retry"));
    assert!(failure.contains("拒绝访问"));
    assert_eq!(consumer.connection.generation, Some(failed_generation));
    assert!(consumer.connection.ready(&manager).is_err());
    assert!(consumer.submit_once(&mut manager).is_err());
    // Releasing the port does not retry a failed intent by itself.
    available.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        let _ = manager.poll();
        assert!(consumer.connection.ready(&manager).is_err());
        assert!(consumer.submit_once(&mut manager).is_err());
    }
    assert_eq!(consumer.authorized, 0);
    assert!(consumer.receipt.is_none());
    assert_eq!(consumer.draft, "help");
    assert_eq!(failed_stream.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    consumer.connection.start(&mut manager).unwrap();
    assert!(consumer.connection.generation.unwrap() > failed_generation);
    let mut retry_stream = peer(&listener);
    connected(&mut manager, "retry");
    assert!(consumer.connection.ready(&manager).unwrap());
    assert!(consumer.submit_once(&mut manager).unwrap());
    assert!(!consumer.submit_once(&mut manager).unwrap());
    completed(consumer.receipt.as_mut().unwrap()).unwrap();
    assert_eq!(consumer.authorized, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    retry_stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"help\n");
}

#[test]
fn command_connection_reopen_rejects_old_intent_before_authorization() {
    let (mut manager, listener) = fixture();
    let mut old = CommandConsumer::new("reopen", "info");
    old.connection.start(&mut manager).unwrap();
    let mut old_stream = peer(&listener);
    connected(&mut manager, "reopen");
    let generation = old.connection.generation.unwrap();
    assert!(old.connection.ready(&manager).unwrap());
    manager.disconnect("reopen").unwrap();
    assert!(old.connection.ready(&manager).is_err());
    assert!(old.submit_once(&mut manager).is_err());
    stopped(&mut manager, "reopen");
    assert!(old.connection.ready(&manager).is_err());
    assert!(old.submit_once(&mut manager).is_err());
    assert_eq!(old_stream.read(&mut [0; 1]).unwrap(), 0);

    manager.connect("reopen").unwrap();
    let mut new_stream = peer(&listener);
    assert!(manager.session("reopen").unwrap().generation > generation);
    assert!(old.connection.ready(&manager).is_err());
    assert!(old.connection.start(&mut manager).is_err());
    connected(&mut manager, "reopen");
    assert!(old.connection.ready(&manager).is_err());
    assert!(old.connection.start(&mut manager).is_err());
    assert!(old.submit_once(&mut manager).is_err());
    assert_eq!(old.connection.generation, Some(generation));
    assert_eq!(old.draft, "info");
    assert_eq!(old.authorized, 0);
    assert!(old.receipt.is_none());

    let mut current = CommandConsumer::new("reopen", "help");
    current.connection.start(&mut manager).unwrap();
    assert_eq!(
        current.connection.generation,
        Some(manager.session("reopen").unwrap().generation)
    );
    assert!(current.submit_once(&mut manager).unwrap());
    completed(current.receipt.as_mut().unwrap()).unwrap();
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    new_stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"help\n");
}

#[test]
fn command_connection_closing_rejects_start_without_background_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let mut manager = with_socket_factory(move |_| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        let stream = socket_transport(address)?;
        opened_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        Ok(stream)
    });
    manager.connect("closing").unwrap();
    let mut stream = peer(&listener);
    opened_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let generation = manager.session("closing").unwrap().generation;
    manager.disconnect("closing").unwrap();
    let mut consumer = CommandConsumer::new("closing", "help");
    assert!(consumer.connection.start(&mut manager).is_err());
    assert_eq!(consumer.connection.generation, Some(generation));
    assert!(consumer.connection.ready(&manager).is_err());
    assert!(consumer.submit_once(&mut manager).is_err());
    assert_eq!(
        manager.session("closing").unwrap().state,
        ConnectionState::Closing
    );
    release_tx.send(()).unwrap();
    stopped(&mut manager, "closing");
    assert_eq!(
        manager.session("closing").unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(consumer.connection.ready(&manager).is_err());
    assert!(consumer.submit_once(&mut manager).is_err());
    assert_eq!(consumer.authorized, 0);
    assert!(consumer.receipt.is_none());
    assert_eq!(consumer.draft, "help");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("closing").unwrap().generation, generation);
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert!(manager.shutdown().is_none());
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
