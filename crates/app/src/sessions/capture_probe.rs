use super::lines::Lines;
use super::native::Transport;
use super::{
    CHUNK_BYTES, Command, IO_TIMEOUT, PendingWrite, ReservedConsoleSource, WRITE_DEADLINE,
};
use parking_lot::Mutex;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DRAIN_LIMIT: Duration = Duration::from_millis(250);
const RESPONSE_LIMIT: Duration = Duration::from_secs(5);
const RETRY_INTERVAL: Duration = Duration::from_millis(400);
const BYTE_LIMIT: usize = 65_536;
const LINE_LIMIT: usize = 1_024;
static NEXT_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct ProbeOwnerState {
    pub(super) alive: bool,
    pub(super) active: Option<Arc<AtomicBool>>,
}

impl Default for ProbeOwnerState {
    fn default() -> Self {
        Self {
            alive: true,
            active: None,
        }
    }
}

pub(super) struct ProbeOwnerGuard(pub(super) Arc<Mutex<ProbeOwnerState>>);

impl Drop for ProbeOwnerGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.alive = false;
        if let Some(cancel) = &state.active {
            cancel.store(true, Ordering::Release);
        }
    }
}

#[derive(Debug)]
struct ProbeLease {
    cancel: Arc<AtomicBool>,
    owner: Arc<Mutex<ProbeOwnerState>>,
    _pending: PendingWrite,
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        let mut owner = self.owner.lock();
        if owner
            .active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.cancel))
        {
            owner.active = None;
        }
    }
}

/// A deferred probe pins the connected serial generation until consumed or dropped.
#[derive(Debug)]
pub(crate) struct CaptureConsoleProbe {
    lease: Arc<ProbeLease>,
    owner_cancel: Arc<AtomicBool>,
    commands: mpsc::SyncSender<Command>,
}

impl CaptureConsoleProbe {
    pub(super) fn new(
        owner: &Arc<Mutex<ProbeOwnerState>>,
        owner_cancel: &Arc<AtomicBool>,
        commands: &mpsc::SyncSender<Command>,
        pending: &Arc<AtomicUsize>,
    ) -> Result<Self, String> {
        let mut state = owner.lock();
        if !state.alive || owner_cancel.load(Ordering::Acquire) {
            return Err("Console owner is no longer connected".into());
        }
        if state.active.is_some() {
            return Err("Console readiness probe is already active".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.active = Some(Arc::clone(&cancel));
        let lease = Arc::new(ProbeLease {
            cancel,
            owner: Arc::clone(owner),
            _pending: PendingWrite::new(pending),
        });
        Ok(Self {
            lease,
            owner_cancel: Arc::clone(owner_cancel),
            commands: commands.clone(),
        })
    }

    pub(crate) fn cancel_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.lease.cancel)
    }

    fn check_current(&self, cancel: &AtomicBool) -> Result<(), String> {
        let owner = self.lease.owner.lock();
        if cancel.load(Ordering::Acquire)
            || self.lease.cancel.load(Ordering::Acquire)
            || self.owner_cancel.load(Ordering::Acquire)
            || !owner.alive
            || !owner
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &self.lease.cancel))
        {
            return Err("Console readiness probe cancelled or owner changed".into());
        }
        Ok(())
    }

    pub(super) fn enqueue(
        &self,
        cancel: &AtomicBool,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        if cancel.load(Ordering::Acquire) {
            self.lease.cancel.store(true, Ordering::Release);
        }
        self.check_current(cancel)?;
        let (completion, receipt) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command {
                bytes: Vec::new(),
                completion,
                thermal_dump: None,
                capture_probe: Some(ProbeRequest {
                    lease: Arc::clone(&self.lease),
                }),
                _pending: None,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "Serial command queue is full".to_owned(),
                mpsc::TrySendError::Disconnected(_) => {
                    "Console owner stopped before probe submission".to_owned()
                }
            })?;
        Ok(receipt)
    }

    pub(super) fn finish_result(
        &self,
        result: Result<(), String>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        result?;
        self.check_current(cancel)
    }

    pub(crate) fn wait(self, cancel: &AtomicBool) -> Result<(), String> {
        let completion = self.enqueue(cancel)?;
        loop {
            if cancel.load(Ordering::Acquire) {
                self.lease.cancel.store(true, Ordering::Release);
            }
            match completion.recv_timeout(IO_TIMEOUT) {
                Ok(result) => return self.finish_result(result, cancel),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Console owner stopped before readiness probe completed".into());
                }
            }
        }
    }
}

impl Drop for CaptureConsoleProbe {
    fn drop(&mut self) {
        self.lease.cancel.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
pub(super) struct ProbeRequest {
    lease: Arc<ProbeLease>,
}

impl ProbeRequest {
    pub(super) fn cancel(&self) -> &AtomicBool {
        &self.lease.cancel
    }
}

#[derive(Debug)]
pub(super) enum ProbeError {
    Invalid(String),
    Transport(io::Error),
}

impl ProbeError {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Invalid(message) => message.clone(),
            Self::Transport(error) => error.to_string(),
        }
    }
}

fn cancelled(cancel: &AtomicBool, owner_cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Acquire) || owner_cancel.load(Ordering::Acquire)
}

fn check_cancel(cancel: &AtomicBool, owner_cancel: &AtomicBool) -> Result<(), ProbeError> {
    if cancelled(cancel, owner_cancel) {
        Err(ProbeError::Invalid(
            "Console readiness probe cancelled".into(),
        ))
    } else {
        Ok(())
    }
}

fn send_challenge(
    transport: &mut dyn Transport,
    bytes: &[u8],
    cancel: &AtomicBool,
    owner_cancel: &AtomicBool,
) -> Result<(), ProbeError> {
    let deadline = Instant::now() + WRITE_DEADLINE;
    let mut offset = 0;
    while offset < bytes.len() {
        // Once a prefix was accepted, finish this harmless frame before the
        // terminal can accept another command. Owner shutdown may abort it.
        if owner_cancel.load(Ordering::Acquire) || (offset == 0 && cancel.load(Ordering::Acquire)) {
            return Err(ProbeError::Invalid(
                "Console readiness probe cancelled".into(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(ProbeError::Transport(io::Error::new(
                io::ErrorKind::TimedOut,
                "Console readiness command write timed out",
            )));
        }
        match transport.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(ProbeError::Transport(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Console readiness command write returned zero",
                )));
            }
            Ok(length) => offset += length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProbeError::Transport(error)),
        }
    }
    check_cancel(cancel, owner_cancel)
}

fn challenge() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let serial = NEXT_NONCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "help h{:x}{:08x}{:08x}{:x}\n",
        time.as_secs(),
        time.subsec_nanos(),
        std::process::id(),
        serial
    )
}

fn read_chunk(
    transport: &mut dyn Transport,
    bytes: &mut [u8],
    forward: &mut impl FnMut(&[u8]),
) -> Result<Option<usize>, ProbeError> {
    match transport.read(bytes) {
        Ok(0) => Err(ProbeError::Transport(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Console stream closed during readiness probe",
        ))),
        Ok(length) => {
            forward(&bytes[..length]);
            Ok(Some(length))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(None)
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(Some(0)),
        Err(error) => Err(ProbeError::Transport(error)),
    }
}

pub(super) fn run(
    transport: &mut dyn Transport,
    cancel: &AtomicBool,
    owner_cancel: &AtomicBool,
    prior_partial: bool,
    forward: impl FnMut(&[u8]),
) -> Result<(), ProbeError> {
    run_with_clock(
        transport,
        cancel,
        owner_cancel,
        prior_partial,
        forward,
        Instant::now,
    )
}

fn run_with_clock(
    transport: &mut dyn Transport,
    cancel: &AtomicBool,
    owner_cancel: &AtomicBool,
    prior_partial: bool,
    mut forward: impl FnMut(&[u8]),
    now: impl Fn() -> Instant,
) -> Result<(), ProbeError> {
    check_cancel(cancel, owner_cancel)?;
    let mut lines = Lines::default();
    let mut bytes = [0; CHUNK_BYTES];
    let mut total = 0;
    let mut discard_partial = prior_partial;
    let drain_deadline = now() + DRAIN_LIMIT;
    loop {
        check_cancel(cancel, owner_cancel)?;
        if now() >= drain_deadline {
            return Err(ProbeError::Invalid(
                "Console input barrier remained saturated".into(),
            ));
        }
        let length = read_chunk(transport, &mut bytes, &mut forward)?;
        check_cancel(cancel, owner_cancel)?;
        let Some(length) = length else {
            if discard_partial || !lines.clean() {
                return Err(ProbeError::Invalid(
                    "Incomplete prior console response".into(),
                ));
            }
            break;
        };
        total += length;
        if total > BYTE_LIMIT {
            return Err(ProbeError::Invalid(
                "Console input barrier exceeded its bound".into(),
            ));
        }
        for &byte in &bytes[..length] {
            if discard_partial {
                if byte == b'\n' {
                    discard_partial = false;
                }
                continue;
            }
            let _ = lines.byte(byte).map_err(ProbeError::Invalid)?;
        }
    }

    check_cancel(cancel, owner_cancel)?;
    let wire = challenge();
    let command = wire.trim_end_matches('\n');
    let deadline = now() + RESPONSE_LIMIT;
    send_challenge(transport, wire.as_bytes(), cancel, owner_cancel)?;
    // The receiver accepts input only after the console initialization attempt.
    // This fresh echo is the barrier; it does not establish CDC initialization success.
    let mut retry_at = now() + RETRY_INTERVAL;
    let mut total = 0;
    let mut count = 0;
    loop {
        check_cancel(cancel, owner_cancel)?;
        if now() >= deadline {
            return Err(ProbeError::Invalid(
                "Incomplete console readiness response timed out".into(),
            ));
        }
        if lines.clean() && now() >= retry_at {
            check_cancel(cancel, owner_cancel)?;
            send_challenge(transport, wire.as_bytes(), cancel, owner_cancel)?;
            retry_at = now() + RETRY_INTERVAL;
        }
        let length = read_chunk(transport, &mut bytes, &mut forward)?;
        check_cancel(cancel, owner_cancel)?;
        if now() >= deadline {
            return Err(ProbeError::Invalid(
                "Incomplete console readiness response timed out".into(),
            ));
        }
        if let Some(length) = length {
            total += length;
            if total > BYTE_LIMIT {
                return Err(ProbeError::Invalid(
                    "Console readiness response exceeded its byte bound".into(),
                ));
            }
            for &byte in &bytes[..length] {
                if let Some(line) = lines.byte(byte).map_err(ProbeError::Invalid)? {
                    count += 1;
                    if count > LINE_LIMIT {
                        return Err(ProbeError::Invalid(
                            "Console readiness response exceeded its line bound".into(),
                        ));
                    }
                    if line == command {
                        check_cancel(cancel, owner_cancel)?;
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Consumes the exclusive source and closes its transport before returning.
pub(crate) fn probe_reserved_capture_console(
    source: ReservedConsoleSource,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("Console readiness probe cancelled".into());
    }
    let mut console = source.open()?;
    let owner_cancel = AtomicBool::new(false);
    run(
        &mut *console.transport,
        cancel,
        &owner_cancel,
        false,
        |_| {},
    )
    .map_err(|error| error.message())
}

#[cfg(test)]
#[path = "tests/capture_probe_protocol.rs"]
mod tests;
