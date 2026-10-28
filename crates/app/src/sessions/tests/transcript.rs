use super::super::test_support::{display_manager, display_test_session};
use super::super::{
    CHUNK_BYTES, ConnectionState, Lifecycle, QUEUE_CAPACITY, SessionChanges, SessionManager,
    port_key,
};
use super::{connected, fixture, peer, stopped, until};
use parking_lot::Mutex;
use slimenrf_terminal_core::AnsiColor;
use std::io::{self, Read, Write};
use std::sync::Arc;

#[test]
fn polling_classifies_rendered_bytes_identity_and_lifecycle_separately() {
    let mut manager = display_manager();
    let display = display_test_session(&mut manager, "display");
    let key = port_key("display");
    manager.entries.get_mut(&key).unwrap().view.state = ConnectionState::Connecting;
    display.lifecycle.send(Lifecycle::Opened).unwrap();
    assert_eq!(
        manager.poll(),
        SessionChanges {
            state: true,
            display: false,
            refresh_inventory: false,
            identities: vec![],
        },
    );
    assert_eq!(
        manager.session("display").unwrap().state,
        ConnectionState::Connected,
    );

    display.push(b"\x1b[31m");
    assert_eq!(manager.poll(), SessionChanges::default());
    display.push(b"colored\n");
    assert_eq!(
        manager.poll(),
        SessionChanges {
            state: false,
            display: true,
            refresh_inventory: false,
            identities: vec![],
        },
    );
    assert_eq!(
        manager.session("display").unwrap().terminal.text(),
        "colored\n"
    );

    display.set_name("Named tracker");
    assert_eq!(
        manager.poll(),
        SessionChanges {
            state: true,
            display: false,
            refresh_inventory: false,
            identities: vec![],
        },
    );
    assert_eq!(
        manager.session("display").unwrap().identity.name.as_deref(),
        Some("Named tracker"),
    );
    display.close(Some("transport disconnected".into()), 0);
    assert_eq!(
        manager.poll(),
        SessionChanges {
            state: true,
            display: false,
            refresh_inventory: false,
            identities: vec![],
        },
    );
    let view = manager.session("display").unwrap();
    assert_eq!(view.state, ConnectionState::Faulted);
    assert_eq!(view.last_error.as_deref(), Some("transport disconnected"));
    assert_eq!(manager.poll(), SessionChanges::default());
}

#[test]
fn closing_reports_display_changes_from_final_utf8_and_loss_notices() {
    for dropped in [0, 7] {
        let mut manager = display_manager();
        let display = display_test_session(&mut manager, "display");
        display.push(b"\xf0\x9f");
        assert_eq!(manager.poll(), SessionChanges::default());
        display.close(None, dropped);
        assert_eq!(
            manager.poll(),
            SessionChanges {
                state: true,
                display: true,
                refresh_inventory: false,
                identities: vec![],
            },
        );
        let text = manager.session("display").unwrap().terminal.text();
        assert!(text.starts_with('\u{fffd}'));
        if dropped != 0 {
            assert!(text.contains("7"));
        } else {
            assert_eq!(text, "\u{fffd}");
        }
        assert_eq!(manager.poll(), SessionChanges::default());
    }
}

struct FragmentedDisplayInput {
    bytes: io::Cursor<Vec<u8>>,
    fragment: usize,
}

impl Read for FragmentedDisplayInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes.len().min(self.fragment);
        self.bytes.read(&mut bytes[..count])
    }
}

impl Write for FragmentedDisplayInput {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("display fixture must never send a serial command")
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn completed_display_input(bytes: Vec<u8>, fragment: usize) -> SessionManager {
    let input = Mutex::new(Some(bytes));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(FragmentedDisplayInput {
            bytes: io::Cursor::new(input.lock().take().unwrap()),
            fragment,
        }))
    }));
    manager.connect("display").unwrap();
    // No consumer polling until every scripted read and Closed are queued.
    manager
        .entries
        .get_mut(&port_key("display"))
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle
        .take()
        .unwrap()
        .join()
        .unwrap();
    manager
}

#[test]
fn thousands_of_one_byte_reads_preserve_text_utf8_and_ansi() {
    let input = "\x1b[31m界\x1b[0m plain\n".repeat(600);
    let mut manager = completed_display_input(input.into_bytes(), 1);
    let _ = manager.poll();
    let view = manager.session("display").unwrap();
    assert_eq!(view.terminal.text(), "界 plain\n".repeat(600));
    for (_, _, text, styles, _) in view.terminal.rows().take(600) {
        assert_eq!(text, "界 plain");
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].range, 0..3);
        assert_eq!(styles[0].style.foreground, AnsiColor::Indexed(1));
    }
    let text = view.terminal.text();
    let _ = manager.poll();
    assert_eq!(manager.session("display").unwrap().terminal.text(), text);
}

#[test]
fn short_reads_are_immediately_visible_and_reused_blocks_keep_ports_separate() {
    let mut manager = display_manager();
    let first = display_test_session(&mut manager, "first");
    let second = display_test_session(&mut manager, "second");
    first.push(b"!");
    let _ = manager.poll();
    assert_eq!(manager.session("first").unwrap().terminal.text(), "!");
    assert_eq!(manager.session("second").unwrap().terminal.text(), "");
    for _ in 0..150 {
        for byte in b"\x1b[31mfirst\n" {
            first.push(std::slice::from_ref(byte));
        }
        for byte in b"second\n" {
            second.push(std::slice::from_ref(byte));
        }
        let _ = manager.poll();
    }
    let first = &manager.session("first").unwrap().terminal;
    let second = &manager.session("second").unwrap().terminal;
    assert_eq!(first.text(), format!("!{}", "first\n".repeat(150)));
    assert_eq!(second.text(), "second\n".repeat(150));
    assert_eq!(first.rows().next().unwrap().3.len(), 1);
    assert!(second.rows().all(|row| row.3.is_empty()));
}

#[test]
fn partial_read_overflow_places_one_gap_before_resumed_utf8_and_ansi() {
    for (partial, resumed, before_gap, after_gap) in [
        (
            &b"\xf0\x9f"[..],
            &b"\x98\x80AFTER"[..],
            "BEFORE\u{fffd}",
            "\u{fffd}\u{fffd}AFTER",
        ),
        (&b"\x1b[3"[..], &b"2mAFTER"[..], "BEFORE", "2mAFTER"),
    ] {
        let mut manager = display_manager();
        let display = display_test_session(&mut manager, "gap");
        let prefix = b"\x1b[31mBEFORE";
        let filler = vec![0; CHUNK_BYTES * QUEUE_CAPACITY - prefix.len() - partial.len()];
        display.push(&filler);
        display.push(prefix);
        // Accept the read's prefix and account for only its lost suffix.
        let mut overflow = partial.to_vec();
        overflow.extend_from_slice(b"lost");
        display.push(&overflow);
        display.push(b"more");
        let _ = manager.poll();
        assert_eq!(manager.session("gap").unwrap().terminal.text(), "BEFORE");

        for byte in resumed {
            display.push(std::slice::from_ref(byte));
        }
        display.push(b"\x1b[32mGREEN");
        display.close(None, display.display.lock().take_dropped());
        let _ = manager.poll();
        let terminal = &manager.session("gap").unwrap().terminal;
        let rows: Vec<_> = terminal.rows().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].2, before_gap);
        assert_eq!(
            rows[1]
                .2
                .split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>(),
            ["8"],
        );
        assert_eq!(rows[2].2, format!("{after_gap}GREEN"));
        assert!(
            rows[1].3.is_empty(),
            "Loss notices must not inherit stream color",
        );
        assert_eq!(rows[2].3.len(), 1);
        assert_eq!(rows[2].3[0].range, after_gap.len()..after_gap.len() + 5);
        assert_eq!(rows[2].3[0].style.foreground, AnsiColor::Indexed(2));
        let text = terminal.text();
        let _ = manager.poll();
        assert_eq!(manager.session("gap").unwrap().terminal.text(), text);
    }
}

#[test]
fn retained_backlog_keeps_multiple_loss_regions_in_order() {
    let mut manager = display_manager();
    let display = display_test_session(&mut manager, "gaps");
    display.push(&vec![0; CHUNK_BYTES * QUEUE_CAPACITY - 1]);
    display.push(b"Ax");
    // Consume only an old control-only block, leaving the retained tail in
    // place when the producer resumes. Repeat through the ring's wrap.
    let mut bytes = [0; CHUNK_BYTES];
    for (retained, lost) in [(b'B', &b"yy"[..]), (b'C', &b""[..])] {
        let (len, dropped_before) = display.display.lock().pop_into(&mut bytes).unwrap();
        assert_eq!(dropped_before, 0);
        manager
            .entries
            .get_mut(&port_key("gaps"))
            .unwrap()
            .view
            .terminal
            .push(&bytes[..len]);
        display.push(&[retained]);
        display.push(&vec![0; CHUNK_BYTES - 1]);
        display.push(lost);
    }
    let _ = manager.poll();
    let terminal = &manager.session("gaps").unwrap().terminal;
    let rows: Vec<_> = terminal.rows().collect();
    assert_eq!(rows.len(), 5);
    assert_eq!([rows[0].2, rows[2].2, rows[4].2], ["A", "B", "C"]);
    for (row, count) in [(1, "1"), (3, "2")] {
        assert_eq!(
            rows[row]
                .2
                .split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>(),
            [count],
        );
        assert!(rows[row].3.is_empty());
    }
    let text = terminal.text();
    let _ = manager.poll();
    assert_eq!(manager.session("gaps").unwrap().terminal.text(), text);
}

#[test]
fn closed_drains_retained_bytes_then_reports_final_loss_exactly_once() {
    let prefix = b"\x1b[31mLAST\xf0\x9f";
    let mut input = vec![0; CHUNK_BYTES * QUEUE_CAPACITY - prefix.len()];
    input.extend_from_slice(prefix);
    let lost = b"lost bytes\nDevice address: AABBCCDDEEFF";
    input.extend_from_slice(lost);
    // A non-dividing read size exercises the partially accepted final read.
    let mut manager = completed_display_input(input, 13);
    let _ = manager.poll();
    let view = manager.session("display").unwrap();
    assert_eq!(view.identity.address.as_deref(), Some("AABBCCDDEEFF"));
    let rows: Vec<_> = view.terminal.rows().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].2, "LAST\u{fffd}");
    assert_eq!(
        rows[1]
            .2
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>(),
        [lost.len().to_string()],
    );
    assert_eq!(rows[2].2, "");
    assert!(rows[1].3.is_empty());
    let expected = view.terminal.text();
    let _ = manager.poll();
    assert_eq!(
        manager.session("display").unwrap().terminal.text(),
        expected
    );
}

#[test]
fn display_overflow_preserves_identity_and_does_not_block_close() {
    let (mut manager, listener) = fixture();
    manager.connect("flood").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "flood");
    // No UI polling until the identity beyond the saturated queue is parsed.
    stream
        .write_all(&vec![b'\n'; CHUNK_BYTES * QUEUE_CAPACITY * 3])
        .unwrap();
    stream.write_all(b"Device address: AABBCCDDEEFF\n").unwrap();
    until(|| {
        manager.entries[&port_key("flood")]
            .worker
            .as_ref()
            .unwrap()
            .identity
            .lock()
            .identity
            .address
            .as_deref()
            == Some("AABBCCDDEEFF")
    });
    manager.disconnect("flood").unwrap();
    stopped(&mut manager, "flood");
    let view = manager.session("flood").unwrap();
    assert_eq!(view.state, ConnectionState::Disconnected);
    assert_eq!(view.identity.address.as_deref(), Some("AABBCCDDEEFF"));
    let retained: Vec<_> = view
        .terminal
        .rows()
        .filter(|row| !row.2.is_empty())
        .collect();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0]
            .2
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>(),
        [
            (CHUNK_BYTES * QUEUE_CAPACITY * 2 + b"Device address: AABBCCDDEEFF\n".len())
                .to_string()
        ],
    );
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}
