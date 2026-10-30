use super::{ProbeOwnerGuard, ProbeOwnerState, RESPONSE_LIMIT, RETRY_INTERVAL, run_with_clock};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Reply {
    Echo,
    WrongEcho,
    PartialEcho,
    StaleHelp,
    StartsLate,
    NeverStarts,
    WriteError,
}

struct Script {
    clock: Arc<Mutex<Instant>>,
    before: Vec<u8>,
    before_at: usize,
    after: Vec<u8>,
    after_at: usize,
    reply: Reply,
    commands: Vec<Vec<u8>>,
}

impl Script {
    fn new(reply: Reply, before: &[u8]) -> Self {
        Self {
            clock: Arc::new(Mutex::new(Instant::now())),
            before: before.to_vec(),
            before_at: 0,
            after: Vec::new(),
            after_at: 0,
            reply,
            commands: Vec::new(),
        }
    }

    fn run(&mut self, prior_partial: bool) -> (Result<(), String>, Vec<u8>) {
        let clock = Arc::clone(&self.clock);
        let mut forwarded = Vec::new();
        let result = run_with_clock(
            self,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            prior_partial,
            |bytes| forwarded.extend_from_slice(bytes),
            || *clock.lock(),
        )
        .map_err(|error| error.message());
        (result, forwarded)
    }
}

impl Read for Script {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let (input, position) = if self.commands.is_empty() {
            (&self.before, &mut self.before_at)
        } else {
            (&self.after, &mut self.after_at)
        };
        if *position == input.len() {
            if !self.commands.is_empty() {
                let mut clock = self.clock.lock();
                *clock += if matches!(self.reply, Reply::StartsLate | Reply::NeverStarts) {
                    RETRY_INTERVAL
                } else {
                    RESPONSE_LIMIT + Duration::from_millis(1)
                };
            }
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = bytes.len().min(13).min(input.len() - *position);
        bytes[..count].copy_from_slice(&input[*position..*position + count]);
        *position += count;
        Ok(count)
    }
}

impl Write for Script {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(bytes.starts_with(b"help h"));
        self.commands.push(bytes.to_vec());
        match self.reply {
            Reply::Echo | Reply::StartsLate => {
                if matches!(self.reply, Reply::StartsLate) && self.commands.len() == 1 {
                    return Ok(bytes.len());
                }
                self.after.extend_from_slice(b"\x1b[32m");
                self.after.extend_from_slice(&bytes[..bytes.len() - 1]);
                self.after.extend_from_slice(b"\x1b[0m\r\n");
            }
            Reply::WrongEcho => self.after.extend_from_slice(b"help hdeadbeef\n"),
            Reply::PartialEcho => self.after.extend_from_slice(&bytes[..bytes.len() - 1]),
            Reply::StaleHelp => self.after.extend_from_slice(
                b"\n=== Available Commands ===\nOther:\n  help Show this help\n\n",
            ),
            Reply::NeverStarts => {}
            Reply::WriteError => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected write failure",
                ));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn fresh_fragmented_ansi_echo_is_the_only_bootstrap_evidence() {
    let mut console = Script::new(Reply::Echo, b"* SlimeVR Receiver *\r\n");
    let (result, forwarded) = console.run(false);
    result.unwrap();
    assert_eq!(console.commands.len(), 1);
    assert!(forwarded.starts_with(b"* SlimeVR Receiver *\r\n\x1b[32mhelp h"));
    assert!(forwarded.ends_with(b"\x1b[0m\r\n"));
}

#[test]
fn discarded_early_help_is_retried_with_the_same_fresh_nonce() {
    let mut console = Script::new(Reply::StartsLate, b"");
    let (result, _) = console.run(false);
    result.unwrap();
    assert_eq!(console.commands.len(), 2);
    assert_eq!(console.commands[0], console.commands[1]);
}

#[test]
fn retries_end_at_one_deadline_and_write_errors_are_not_replayed() {
    let mut never = Script::new(Reply::NeverStarts, b"");
    let (result, _) = never.run(false);
    assert!(result.unwrap_err().contains("timed out"));
    assert!(never.commands.len() > 1 && never.commands.len() <= 13);
    assert!(
        never
            .commands
            .iter()
            .all(|command| command == &never.commands[0])
    );

    let mut failed_write = Script::new(Reply::WriteError, b"");
    let (result, _) = failed_write.run(false);
    assert!(result.unwrap_err().contains("injected write failure"));
    assert_eq!(failed_write.commands.len(), 1);
}

#[test]
fn banner_old_help_wrong_nonce_and_unterminated_echo_do_not_succeed() {
    for reply in [Reply::WrongEcho, Reply::PartialEcho, Reply::StaleHelp] {
        let mut console = Script::new(reply, b"* SlimeVR Receiver *\n");
        let (result, forwarded) = console.run(false);
        assert!(result.is_err());
        assert!(forwarded.starts_with(b"* SlimeVR Receiver *\n"));
        assert_eq!(console.commands.len(), 1);
    }
    let mut console = Script::new(Reply::StaleHelp, b"\n=== Available Commands ===\n");
    let (result, forwarded) = console.run(false);
    assert!(result.is_err());
    assert!(forwarded.starts_with(b"\n=== Available Commands ===\n"));
    assert_eq!(console.commands.len(), 1);
}

#[test]
fn prior_fragment_can_finish_but_an_incomplete_or_saturated_barrier_cannot_send() {
    let mut finished = Script::new(Reply::Echo, b" tail\n");
    let (result, forwarded) = finished.run(true);
    result.unwrap();
    assert!(forwarded.starts_with(b" tail\n"));
    assert_eq!(finished.commands.len(), 1);

    let mut partial = Script::new(Reply::Echo, b"partial stale response");
    let (result, forwarded) = partial.run(false);
    assert!(result.unwrap_err().contains("Incomplete prior"));
    assert_eq!(forwarded, b"partial stale response");
    assert!(partial.commands.is_empty());

    let mut unfinished = Script::new(Reply::Echo, b"still partial");
    let (result, _) = unfinished.run(true);
    assert!(result.unwrap_err().contains("Incomplete prior"));
    assert!(unfinished.commands.is_empty());

    let input = b"bounded line\n".repeat(5_100);
    let mut saturated = Script::new(Reply::Echo, &input);
    let (result, forwarded) = saturated.run(false);
    assert!(result.unwrap_err().contains("exceeded its bound"));
    assert!(forwarded.len() > 65_536);
    assert!(saturated.commands.is_empty());
}

#[test]
fn owner_guard_revokes_pending_probe_when_the_thread_unwinds() {
    let owner = Arc::new(Mutex::new(ProbeOwnerState::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    owner.lock().active = Some(Arc::clone(&cancel));
    let worker_owner = Arc::clone(&owner);
    let worker = std::thread::spawn(move || {
        let _guard = ProbeOwnerGuard(worker_owner);
        panic!("synthetic owner failure");
    });
    assert!(worker.join().is_err());
    assert!(!owner.lock().alive);
    assert!(cancel.load(Ordering::Acquire));
}
