use super::native::{Factory, Transport};
use super::thermal_dump::DumpCollector;
use super::transcript::ConsoleLines;
use super::{CHUNK_BYTES, Command, DisplayQueue, Lifecycle, ThermalDumpOutcome, WRITE_DEADLINE};
use crate::i18n::tr;
use parking_lot::Mutex;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

pub(super) fn run_worker(
    port: &str,
    factory: Factory,
    cancel: Arc<AtomicBool>,
    commands: mpsc::Receiver<Command>,
    display: Arc<Mutex<DisplayQueue>>,
    lifecycle: mpsc::Sender<Lifecycle>,
    mut parser: ConsoleLines,
) {
    let mut read_buffer = [0; CHUNK_BYTES];
    let mut dump: Option<DumpCollector> = None;
    let result = (|| -> io::Result<()> {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut transport = factory(port)?;
        // A cancelled blocking open must never publish a connected session.
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let _ = lifecycle.send(Lifecycle::Opened);
        loop {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            if dump.as_ref().is_some_and(|collector| !collector.active()) {
                dump = None;
            }
            for _ in 0..8 {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                match commands.try_recv() {
                    Ok(mut command) => {
                        if let Some(request) = command.thermal_dump.take() {
                            let Some(collector) = DumpCollector::arm(request) else {
                                let _ = command.completion.try_send(Err(
                                    "Temperature dump cancelled before sending".to_owned(),
                                ));
                                continue;
                            };
                            dump = Some(collector);
                        }
                        let result = write_command(&mut *transport, &command.bytes, &cancel);
                        if let Err(error) = &result
                            && let Some(collector) = &dump
                        {
                            collector.finish(ThermalDumpOutcome::WriteFailed(error.to_string()));
                        }
                        let _ = command
                            .completion
                            .try_send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
                        result?;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            if cancel.load(Ordering::Acquire) {
                break;
            }
            match transport.read(&mut read_buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        tr("serial.transport.error.stream_closed"),
                    ));
                }
                Ok(length) => {
                    // Shared console observers consume lossless input before
                    // the bounded display queue can discard received bytes.
                    parser.push(&read_buffer[..length]);
                    // Acquisition consumes the same lossless input before display.
                    if let Some(collector) = &mut dump {
                        collector.feed(&read_buffer[..length]);
                    }
                    display.lock().push(&read_buffer[..length]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        rust_i18n::t!("serial.transport.error.read", error = error).to_string(),
                    ));
                }
            }
        }
        Ok(())
    })();
    // The transport has already been dropped when the closure returns.
    parser.finish();
    if let Some(collector) = &dump {
        collector.finish(ThermalDumpOutcome::Disconnected);
    }
    let cancelled = cancel.load(Ordering::Acquire);
    let error = result.err().map(|error| error.to_string());
    let pending_error = if cancelled {
        tr("serial.session.error.disconnecting")
    } else {
        error
            .clone()
            .unwrap_or_else(|| tr("serial.session.error.stopped"))
    };
    // Fail already queued work before publishing closure. A concurrent enqueue
    // after this drain still resolves through the disconnected receipt channel.
    while let Ok(command) = commands.try_recv() {
        if let Some(dump) = &command.thermal_dump {
            dump.requests
                .lock()
                .finish(&dump.request, ThermalDumpOutcome::Disconnected);
        }
        let _ = command.completion.try_send(Err(pending_error.clone()));
    }
    // Transfer the final gap exactly once. Closed owns it; the consumer must
    // drain all retained blocks before appending this trailing notice.
    let dropped = display.lock().take_dropped();
    let _ = lifecycle.send(Lifecycle::Closed {
        error: if cancelled { None } else { error },
        dropped,
    });
}

pub(super) fn write_command(
    transport: &mut dyn Transport,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_DEADLINE;
    let mut offset = 0;
    while offset < bytes.len() {
        if cancel.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                tr("serial.session.error.send_cancelled"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                tr("serial.transport.error.send_timeout"),
            ));
        }
        match transport.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    tr("serial.transport.error.write_zero"),
                ));
            }
            Ok(length) => offset += length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // A native write error can follow an unreported partial transfer.
            // Never replay timeout/WouldBlock errors: commands may be non-idempotent.
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    rust_i18n::t!("serial.transport.error.send", error = error).to_string(),
                ));
            }
        }
    }
    Ok(())
}
