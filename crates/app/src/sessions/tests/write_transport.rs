use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub(super) struct PartialErrorWriter {
    pub(super) stream: TcpStream,
    pub(super) ready: mpsc::Sender<()>,
    pub(super) release: mpsc::Receiver<()>,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) confirmed_prefix: usize,
    pub(super) unreported_prefix: usize,
    pub(super) error_kind: io::ErrorKind,
    pub(super) failed: bool,
}

impl Read for PartialErrorWriter {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for PartialErrorWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.confirmed_prefix != 0 {
            let length = self.confirmed_prefix;
            self.confirmed_prefix = 0;
            self.stream.write_all(&bytes[..length])?;
            return Ok(length);
        }
        if !self.failed {
            self.failed = true;
            self.ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            // Model a driver that transmits bytes but returns no count on
            // error, as serialport's failed Windows WriteFile path does.
            self.stream.write_all(&bytes[..self.unreported_prefix])?;
            return Err(io::Error::new(self.error_kind, "injected write failure"));
        }
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
