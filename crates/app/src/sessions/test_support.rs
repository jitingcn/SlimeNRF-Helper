use super::native::Transport;
use super::{
    ConnectionState, DisplayQueue, Entry, IO_TIMEOUT, IdentitySnapshot, Lifecycle, QUEUE_CAPACITY,
    SessionManager, SessionView, Worker, WriteReceipt, port_key,
};
use crate::domain::DeviceIdentity;
use parking_lot::Mutex;
use slimenrf_terminal_core::TerminalBuffer;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) fn write_receipt() -> (WriteReceipt, mpsc::SyncSender<Result<(), String>>) {
    let (completion, receiver) = mpsc::sync_channel(1);
    (
        WriteReceipt {
            completion: Some(receiver),
        },
        completion,
    )
}

pub(crate) struct DisplaySession {
    pub(super) display: Arc<Mutex<DisplayQueue>>,
    pub(super) lifecycle: mpsc::Sender<Lifecycle>,
    identity: Arc<Mutex<IdentitySnapshot>>,
}

impl DisplaySession {
    pub(crate) fn push(&self, bytes: &[u8]) {
        self.display.lock().push(bytes);
    }

    pub(crate) fn set_name(&self, name: &str) {
        let mut snapshot = self.identity.lock();
        snapshot.identity.name = Some(name.into());
        snapshot.revision += 1;
    }

    pub(crate) fn close(&self, error: Option<String>, dropped: u64) {
        self.lifecycle
            .send(Lifecycle::Closed { error, dropped })
            .unwrap();
    }
}

pub(crate) fn display_manager() -> SessionManager {
    SessionManager::with_factory(Arc::new(|_| {
        panic!("queue fixture must not open a transport")
    }))
}

pub(crate) fn display_test_session(manager: &mut SessionManager, port: &str) -> DisplaySession {
    let display = Arc::new(Mutex::new(DisplayQueue::new()));
    let (commands, _) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (lifecycle_tx, lifecycle) = mpsc::channel();
    let identity = Arc::new(Mutex::new(IdentitySnapshot {
        identity: DeviceIdentity::default(),
        revision: 0,
        pairing_changed: false,
    }));
    manager.entries.insert(
        port_key(port),
        Entry {
            view: SessionView {
                port: port.into(),
                generation: 1,
                state: ConnectionState::Connected,
                last_error: None,
                identity: DeviceIdentity::default(),
                terminal: TerminalBuffer::new(),
            },
            worker: Some(Worker {
                generation: 1,
                cancel: Arc::new(AtomicBool::new(false)),
                commands,
                display: Arc::clone(&display),
                lifecycle,
                identity: Arc::clone(&identity),
                identity_revision: 0,
                rssi_scan: Arc::new(Mutex::new(None)),
                delivered_rssi_scan: None,
                opened: true,
                handle: None,
                closed: false,
            }),
            close_requested: false,
            pending_writes: Arc::new(AtomicUsize::new(0)),
            opening_identity: None,
            captured_usb_identity: None,
            reconnect: None,
        },
    );
    DisplaySession {
        display,
        lifecycle: lifecycle_tx,
        identity,
    }
}

pub(crate) fn held_worker_session(
    manager: &mut SessionManager,
    port: &str,
) -> (DisplaySession, mpsc::Sender<()>) {
    let display = display_test_session(manager, port);
    let (release, gate) = mpsc::channel();
    manager
        .entries
        .get_mut(&port_key(port))
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle = Some(thread::spawn(move || {
        gate.recv_timeout(Duration::from_secs(3)).unwrap()
    }));
    (display, release)
}

pub(crate) fn display_session_state(
    manager: &mut SessionManager,
    port: &str,
    state: ConnectionState,
) {
    manager.entries.get_mut(&port_key(port)).unwrap().view.state = state;
}

pub(crate) fn socket_transport(address: std::net::SocketAddr) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

pub(crate) fn fixture() -> (SessionManager, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(socket_transport(address)?) as Box<dyn Transport>)
    }));
    (manager, listener)
}

pub(crate) fn peer(listener: &TcpListener) -> TcpStream {
    let stream = listener.accept().unwrap().0;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

pub(crate) fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "worker condition timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

pub(crate) fn connected(manager: &mut SessionManager, port: &str) {
    until(|| {
        let _ = manager.poll();
        manager.session(port).unwrap().state == ConnectionState::Connected
    });
}

pub(crate) fn stopped(manager: &mut SessionManager, port: &str) {
    until(|| {
        let _ = manager.poll();
        manager.entries[&port_key(port)].worker.is_none()
    });
}

pub(crate) fn completed(receipt: &mut WriteReceipt) -> Result<(), String> {
    let mut result = None;
    until(|| {
        result = receipt.poll();
        result.is_some()
    });
    let result = result.unwrap();
    assert!(receipt.poll().is_none(), "receipt delivered more than once");
    result
}

pub(crate) fn with_socket_factory(
    factory: impl Fn(&str) -> io::Result<TcpStream> + Send + Sync + 'static,
) -> SessionManager {
    with_transport_factory(factory)
}

pub(crate) fn with_transport_factory<T: io::Read + io::Write + Send + 'static>(
    factory: impl Fn(&str) -> io::Result<T> + Send + Sync + 'static,
) -> SessionManager {
    SessionManager::with_factory(Arc::new(move |port| {
        Ok(Box::new(factory(port)?) as Box<dyn Transport>)
    }))
}

pub(crate) fn serial_open_error(port: &str, error: &serialport::Error) -> io::Error {
    super::native::serial_open_error(port, error)
}
