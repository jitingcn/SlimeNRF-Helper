use super::super::native::Transport;
use super::super::{
    CHUNK_BYTES, IO_TIMEOUT, MAX_IDENTITY_LINE, QUEUE_CAPACITY, SessionManager, port_key,
};
use super::{completed, connected, stopped, until};
use crate::commands::{CommandContext, presets};
use crate::domain::DeviceRole;
use crate::rssi_scan::Sample;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const HEADER: &str = "RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)\r\nCurrent channel (effective): 84\r\nRecommended: channel 86 (-69 dBm)\r\nAll channels sorted by RSSI (lower is better):\r\n";
const FIRST_ROWS: &str = " 86 (-69 dBm)   88 (-66 dBm)\r\n 84 (-61 dBm)\r\n";

struct ReadBlock {
    bytes: Vec<u8>,
    consumed: mpsc::Sender<()>,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}

struct Script {
    reads: mpsc::Sender<ReadBlock>,
    writes: mpsc::Receiver<Vec<u8>>,
}

impl Script {
    fn feed(&self, bytes: impl AsRef<[u8]>) {
        let (consumed, done) = mpsc::channel();
        self.reads
            .send(ReadBlock {
                bytes: bytes.as_ref().to_vec(),
                consumed,
                gate: None,
            })
            .unwrap();
        // Acknowledged on the NEXT read, after framing, mailbox replacement and
        // lossy display handling have all executed in the actual serial worker.
        done.recv_timeout(Duration::from_secs(3)).unwrap();
    }

    fn gate(&self, bytes: impl AsRef<[u8]>) -> mpsc::Sender<()> {
        let (ready, entered) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (consumed, _done) = mpsc::channel();
        self.reads
            .send(ReadBlock {
                bytes: bytes.as_ref().to_vec(),
                consumed,
                gate: Some((ready, wait)),
            })
            .unwrap();
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
        release
    }

    fn command(&self, expected: &[u8]) {
        assert_eq!(
            self.writes.recv_timeout(Duration::from_secs(3)).unwrap(),
            expected
        );
    }
}

struct ScriptTransport {
    reads: mpsc::Receiver<ReadBlock>,
    writes: mpsc::Sender<Vec<u8>>,
    bytes: io::Cursor<Vec<u8>>,
    consumed: Option<mpsc::Sender<()>>,
    fragment: usize,
}

impl Read for ScriptTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.bytes.position() == self.bytes.get_ref().len() as u64 {
            if let Some(consumed) = self.consumed.take() {
                let _ = consumed.send(());
            }
            let block = match self.reads.recv_timeout(IO_TIMEOUT) {
                Ok(block) => block,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            };
            if let Some((ready, wait)) = block.gate {
                ready.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            self.bytes = io::Cursor::new(block.bytes);
            self.consumed = Some(block.consumed);
        }
        let length = bytes.len().min(self.fragment);
        self.bytes.read(&mut bytes[..length])
    }
}

impl Write for ScriptTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn scripted(fragments: &[usize]) -> (SessionManager, Vec<Script>) {
    let mut transports = VecDeque::new();
    let mut scripts = Vec::new();
    for &fragment in fragments {
        let (reads, read_rx) = mpsc::channel();
        let (write_tx, writes) = mpsc::channel();
        scripts.push(Script { reads, writes });
        transports.push_back(Box::new(ScriptTransport {
            reads: read_rx,
            writes: write_tx,
            bytes: io::Cursor::new(Vec::new()),
            consumed: None,
            fragment,
        }) as Box<dyn Transport>);
    }
    let transports = Mutex::new(transports);
    let manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(transports
            .lock()
            .pop_front()
            .expect("unexpected serial open"))
    }));
    (manager, scripts)
}

fn mailbox(
    manager: &SessionManager,
    port: &str,
) -> Arc<Mutex<Option<crate::rssi_scan::ScanSnapshot>>> {
    Arc::clone(
        &manager.entries[&port_key(port)]
            .worker
            .as_ref()
            .unwrap()
            .rssi_scan,
    )
}

fn wait_finished(manager: &SessionManager, port: &str) {
    until(|| {
        manager.entries[&port_key(port)]
            .worker
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
    });
}

#[test]
fn typed_and_preset_scan_observe_fragmented_raw_reads_despite_display_loss() {
    for fragment in [1, 7, CHUNK_BYTES] {
        let (mut manager, scripts) = scripted(&[fragment]);
        let script = &scripts[0];
        manager.connect("receiver").unwrap();
        connected(&mut manager, "receiver");
        let preset = presets(CommandContext::Direct(DeviceRole::Receiver))
            .find(|preset| preset.id == "rssi-scan")
            .unwrap();
        for line in ["  rssi_scan  \r\n".to_owned(), preset.build("").unwrap()] {
            let display = Arc::clone(
                &manager.entries[&port_key("receiver")]
                    .worker
                    .as_ref()
                    .unwrap()
                    .display,
            );
            display
                .lock()
                .push(&vec![b'x'; CHUNK_BYTES * QUEUE_CAPACITY]);
            let mut receipt = manager.send_line("receiver", &line).unwrap();
            script.command(format!("{}\n", line.trim_end_matches(['\r', '\n'])).as_bytes());
            assert!(completed(&mut receipt).is_ok());
            // Transport receipt is neither a scan observation nor completion.
            assert!(manager.take_rssi_scans().is_empty());
            script.feed(format!("rssi_scan\r\n{HEADER}{FIRST_ROWS}"));
            let mut updates = manager.take_rssi_scans();
            assert_eq!(updates.len(), 1);
            let update = updates.pop().unwrap();
            assert_eq!(update.port, "receiver");
            assert_eq!(
                update.generation,
                manager.session("receiver").unwrap().generation
            );
            assert!(update.first_update);
            assert_eq!(
                update.snapshot.report.samples,
                vec![
                    Sample {
                        channel: 84,
                        rssi_dbm: -61
                    },
                    Sample {
                        channel: 86,
                        rssi_dbm: -69
                    },
                    Sample {
                        channel: 88,
                        rssi_dbm: -66
                    },
                ]
            );
            assert!(display.lock().take_dropped() > 0);
            assert!(manager.take_rssi_scans().is_empty());
            // Observation owns no command transaction or quarantine.
            let mut info = manager.send_line("receiver", "info").unwrap();
            script.command(b"info\n");
            assert!(completed(&mut info).is_ok());
        }
    }
}

#[test]
fn coalescing_uses_last_delivered_scan_not_first_received_row() {
    let (mut manager, scripts) = scripted(&[1]);
    manager.connect("receiver").unwrap();
    connected(&mut manager, "receiver");
    scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
    let first = manager.take_rssi_scans().pop().unwrap();
    assert!(first.first_update);
    assert_eq!(first.snapshot.report.samples.len(), 3);
    scripts[0].feed(" 78 (-47 dBm)\r\n");
    let continued = manager.take_rssi_scans().pop().unwrap();
    assert_eq!(continued.snapshot.scan_id, first.snapshot.scan_id);
    assert!(!continued.first_update);
    assert_eq!(continued.snapshot.report.samples.len(), 4);
    scripts[0].feed(format!("{HEADER}{FIRST_ROWS}{HEADER} 2 (-22 dBm)\r\n"));
    let latest = manager.take_rssi_scans().pop().unwrap();
    assert_eq!(latest.snapshot.scan_id, first.snapshot.scan_id + 2);
    assert!(latest.first_update);
    assert_eq!(
        latest.snapshot.report.samples,
        vec![Sample {
            channel: 2,
            rssi_dbm: -22
        }]
    );
    scripts[0].feed(" 2 (-22 dBm)\r\n[00:00:20.018,035] <inf> esb_event: Total TPS: 0\r\n");
    assert!(manager.take_rssi_scans().is_empty());
}

#[test]
fn unsolicited_scans_are_independently_attributed_to_each_serial_owner() {
    let (mut manager, scripts) = scripted(&[1, 3]);
    for port in ["first", "second"] {
        manager.connect(port).unwrap();
        connected(&mut manager, port);
    }
    scripts[0].feed(format!("{HEADER} 86 (-69 dBm)\r\n"));
    scripts[1].feed(format!("{HEADER} 8 (-43 dBm)\r\n"));
    let updates = manager.take_rssi_scans();
    assert_eq!(updates.len(), 2);
    for (port, channel) in [("first", 86), ("second", 8)] {
        let update = updates.iter().find(|update| update.port == port).unwrap();
        assert_eq!(update.generation, manager.session(port).unwrap().generation);
        assert_eq!(update.snapshot.scan_id, 1);
        assert!(update.first_update);
        assert_eq!(update.snapshot.report.samples[0].channel, channel);
    }
    scripts[0].feed(" 88 (-66 dBm)\r\n");
    let updates = manager.take_rssi_scans();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].port, "first");
    assert!(!updates[0].first_update);
}

#[test]
fn cancelled_reserved_and_old_generations_discard_pending_and_late_read_results() {
    for transition in ["disconnect", "reserve", "generation", "cancel", "shutdown"] {
        let (mut manager, scripts) = scripted(&[CHUNK_BYTES]);
        manager.connect("receiver").unwrap();
        connected(&mut manager, "receiver");
        let slot = mailbox(&manager, "receiver");
        scripts[0].feed(format!("{HEADER} 86 (-69 dBm)\r\n"));
        assert!(slot.lock().is_some());
        // The next native read is already in flight when ownership is revoked.
        let release = scripts[0].gate(" 88 (-66 dBm)\r\n");
        let mut reaper = None;
        match transition {
            "disconnect" => manager.disconnect("receiver").unwrap(),
            "reserve" => {
                let generation = manager.session("receiver").unwrap().generation;
                manager.reserve("receiver", Some(generation)).unwrap();
            }
            "generation" => {
                manager
                    .entries
                    .get_mut(&port_key("receiver"))
                    .unwrap()
                    .view
                    .generation += 1;
            }
            "cancel" => {
                manager.entries[&port_key("receiver")]
                    .worker
                    .as_ref()
                    .unwrap()
                    .cancel
                    .store(true, Ordering::Release);
            }
            "shutdown" => reaper = manager.shutdown(),
            _ => unreachable!(),
        }
        assert!(manager.take_rssi_scans().is_empty(), "{transition}");
        assert!(slot.lock().is_none() || transition == "shutdown");
        // Revoking an old generation through poll also requests its cancellation.
        if transition == "generation" {
            let _ = manager.poll();
        }
        release.send(()).unwrap();
        if let Some(reaper) = reaper {
            reaper.join().unwrap();
        } else {
            stopped(&mut manager, "receiver");
        }
        assert!(slot.lock().is_none(), "late read survived {transition}");
        assert!(manager.take_rssi_scans().is_empty());
    }
}

#[test]
fn closed_worker_discards_pending_results_even_before_lifecycle_poll() {
    let (mut manager, mut scripts) = scripted(&[CHUNK_BYTES]);
    manager.connect("receiver").unwrap();
    connected(&mut manager, "receiver");
    scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
    let slot = mailbox(&manager, "receiver");
    drop(scripts.remove(0));
    wait_finished(&manager, "receiver");
    assert!(slot.lock().is_none());
    assert!(manager.take_rssi_scans().is_empty());
    stopped(&mut manager, "receiver");
}

#[test]
fn reconnect_has_fresh_mailbox_and_first_delivery_even_when_scan_ids_repeat() {
    let (mut manager, mut scripts) = scripted(&[CHUNK_BYTES, 1]);
    manager.connect("receiver").unwrap();
    connected(&mut manager, "receiver");
    scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
    let old_slot = mailbox(&manager, "receiver");
    let old = manager.take_rssi_scans().pop().unwrap();
    manager.disconnect("receiver").unwrap();
    stopped(&mut manager, "receiver");
    drop(scripts.remove(0));
    manager.connect("receiver").unwrap();
    connected(&mut manager, "receiver");
    let new_slot = mailbox(&manager, "receiver");
    assert!(!Arc::ptr_eq(&old_slot, &new_slot));
    // Retaining an old producer's slot cannot write into the replacement owner.
    *old_slot.lock() = Some(old.snapshot.clone());
    assert!(manager.take_rssi_scans().is_empty());
    scripts[0].feed(format!("{HEADER} 2 (-22 dBm)\r\n"));
    let new = manager.take_rssi_scans().pop().unwrap();
    assert_ne!(old.generation, new.generation);
    assert_eq!(old.snapshot.scan_id, new.snapshot.scan_id);
    assert!(new.first_update);
    assert_eq!(
        new.snapshot.report.samples,
        vec![Sample {
            channel: 2,
            rssi_dbm: -22
        }]
    );
}

#[test]
fn unterminated_rssi_tokens_never_publish_during_identity_eof_flush() {
    let (mut manager, mut scripts) = scripted(&[1]);
    manager.connect("receiver").unwrap();
    connected(&mut manager, "receiver");
    scripts[0].feed(format!("{HEADER} 86 (-69 dBm)"));
    assert!(manager.take_rssi_scans().is_empty());
    let slot = mailbox(&manager, "receiver");
    drop(scripts.remove(0));
    wait_finished(&manager, "receiver");
    assert!(slot.lock().is_none());
    assert!(manager.take_rssi_scans().is_empty());
    stopped(&mut manager, "receiver");
}

#[test]
fn discarded_lines_invalidate_table_and_preserve_hidden_control_string_state() {
    let mut cases = vec![
        (b"bad UTF-8 \xff\r\n".to_vec(), false),
        (b"\x1b]title;\xff\r\n".to_vec(), true),
    ];
    for length in [8192, MAX_IDENTITY_LINE + 64] {
        let mut plain = vec![b'x'; length];
        plain.extend_from_slice(b"\r\n");
        cases.push((plain, false));
        let mut hidden = b"\x1b]title;".to_vec();
        hidden.resize(length, b'x');
        hidden.extend_from_slice(b"\r\n");
        cases.push((hidden, true));
    }
    // Split a real Unicode codepoint precisely across buffered-prefix/overflow
    // discard calls. Continuation bytes 9D/9C are not independent OSC/ST codes.
    for (character, hidden) in [("Н", false), ("М", true)] {
        let mut split = if hidden {
            b"\x1b]title;".to_vec()
        } else {
            Vec::new()
        };
        split.resize(MAX_IDENTITY_LINE - 1, b'x');
        split.extend_from_slice(character.as_bytes());
        split.extend_from_slice(b"\r\n");
        cases.push((split, hidden));
    }
    for (discarded, hidden) in cases {
        let (mut manager, scripts) = scripted(&[3]);
        manager.connect("receiver").unwrap();
        connected(&mut manager, "receiver");
        scripts[0].feed(HEADER);
        scripts[0].feed(discarded);
        scripts[0].feed(" 86 (-69 dBm)\r\n");
        assert!(manager.take_rssi_scans().is_empty());
        if hidden {
            // The dropped bytes contained an OSC opener: apparent protocol
            // lines remain hidden until its real terminator reaches the framer.
            scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
            assert!(manager.take_rssi_scans().is_empty());
            scripts[0].feed("\x07\r\n");
        }
        scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
        let update = manager.take_rssi_scans().pop().unwrap();
        assert_eq!(update.snapshot.scan_id, 2);
        assert!(update.first_update);
        assert_eq!(update.snapshot.report.samples.len(), 3);
    }
}

#[test]
fn startup_scan_is_not_lost_before_opened_lifecycle_is_polled() {
    let (mut manager, scripts) = scripted(&[1]);
    manager.connect("receiver").unwrap();
    // No manager poll: the worker has opened and read data, but the main-thread
    // view still says Connecting until it consumes the reliable Opened event.
    scripts[0].feed(format!("{HEADER}{FIRST_ROWS}"));
    let update = manager.take_rssi_scans().pop().unwrap();
    assert!(update.first_update);
    assert_eq!(update.port, "receiver");
    assert_eq!(
        update.generation,
        manager.session("receiver").unwrap().generation
    );
    connected(&mut manager, "receiver");
    assert!(manager.take_rssi_scans().is_empty());
    // A healthy admitted connection is not revoked by stale inventory alone.
    manager.invalidate_inventory();
    scripts[0].feed(" 78 (-47 dBm)\r\n");
    let continued = manager.take_rssi_scans().pop().unwrap();
    assert!(!continued.first_update);
    assert_eq!(continued.snapshot.scan_id, update.snapshot.scan_id);
}
