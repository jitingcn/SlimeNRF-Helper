use super::{HidLogUiState, OTHER_ROWS, SENSOR_ROWS};
use crate::{
    domain::ReceiverEndpoint,
    hid_log::{
        self as trace, Direction, Record, RecordData, Recorder, ReportBytes, Source,
        decode::Category,
    },
};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

fn source(key: &str) -> Arc<Source> {
    Arc::new(Source::main(&ReceiverEndpoint {
        key: key.into(),
        label: key.into(),
        path: key.as_bytes().to_vec(),
        serial_number: None,
    }))
}

fn report(sequence: u64, source: Arc<Source>, bytes: &[u8]) -> Arc<Record> {
    Arc::new(Record {
        sequence,
        elapsed_us: sequence * 1000,
        source,
        data: RecordData::Report {
            direction: Direction::In,
            bytes: ReportBytes::new(bytes).expect("native-size fixture"),
            transferred: Some(bytes.len()),
            error: None,
        },
    })
}

fn event(tracker: u8) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[0] = 251;
    bytes[2] = 225;
    bytes[3] = 1;
    bytes[4] = tracker;
    bytes[5..9].copy_from_slice(&0x12345678_u32.to_le_bytes());
    bytes[9..11].copy_from_slice(&9_u16.to_le_bytes());
    bytes[11..13].copy_from_slice(&11_u16.to_le_bytes());
    bytes[13] = 1;
    bytes
}

fn sensor(tracker: u8) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[0] = 3;
    bytes[1] = tracker;
    bytes[2] = 1;
    bytes[15] = (-45_i8) as u8;
    bytes
}

#[test]
fn tracker_filter_keeps_identical_slots_on_different_receivers_isolated() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    state.retain(report(1, source("receiver-a"), &event(4)));
    state.retain(report(2, source("receiver-b"), &event(4)));
    state.tracker_filter.extend([4, 254]);
    state.kind_filter.insert(1);
    state.receiver_filter.insert("receiver-a".into());
    state.rebuild();
    assert_eq!(state.visible.len(), 1);
    assert_eq!(state.visible[0].record.source.receiver_key, "receiver-a");
    assert_eq!(state.visible[0].record.sequence, 1);

    state.receiver_filter = ["receiver-b".into()].into_iter().collect();
    state.rebuild();
    assert_eq!(state.visible.len(), 1);
    assert_eq!(state.visible[0].record.source.receiver_key, "receiver-b");
    assert_eq!(state.visible[0].record.sequence, 2);

    state.receiver_filter.insert("receiver-a".into());
    state.rebuild();
    assert_eq!(state.visible.len(), 2);
}

#[test]
fn multiselect_filters_union_within_each_group_and_intersect_across_groups() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    let receiver_a = source("receiver-a");
    let receiver_b = source("receiver-b");
    let mut event_a7 = event(7);
    event_a7[13] = 2;
    let mut event_b4 = event(4);
    event_b4[13] = 2;
    state.retain(report(1, receiver_a.clone(), &event(4)));
    state.retain(report(2, receiver_a.clone(), &event_a7));
    state.retain(report(3, receiver_b.clone(), &event_b4));
    state.retain(report(4, receiver_b.clone(), &event(8)));
    state.retain(report(5, source("receiver-c"), &event(4)));
    state.retain(report(6, receiver_a, &sensor(4)));
    state.retain(report(7, receiver_b, &sensor(7)));
    state
        .receiver_filter
        .extend(["receiver-a".into(), "receiver-b".into()]);
    state.tracker_filter.extend([4, 7]);
    state.category_filter = vec![Category::Event, Category::Sensor];
    state.kind_filter.extend([1, 2]);
    state.rebuild();
    let sequences = |state: &HidLogUiState| {
        state
            .visible
            .iter()
            .map(|row| row.record.sequence)
            .collect::<Vec<_>>()
    };
    assert_eq!(sequences(&state), vec![1, 2, 3]);

    state.kind_filter.remove(&2);
    state.rebuild();
    assert_eq!(sequences(&state), vec![1]);
    state.receiver_filter = ["receiver-b".into()].into_iter().collect();
    state.rebuild();
    assert!(state.visible.is_empty());

    state.kind_filter.clear();
    state.rebuild();
    assert_eq!(sequences(&state), vec![3, 7]);
    state.category_filter.clear();
    state.tracker_filter.clear();
    state.receiver_filter.clear();
    state.rebuild();
    assert_eq!(sequences(&state), vec![1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn kind_selection_includes_events_and_excluding_events_clears_kind_selection() {
    let mut state = HidLogUiState {
        category_filter: vec![Category::Sensor],
        ..Default::default()
    };
    state.toggle_kind(1);
    state.toggle_kind(2);
    assert_eq!(
        state.kind_filter.iter().copied().collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        state.category_filter,
        vec![Category::Sensor, Category::Event]
    );
    state.toggle_kind(1);
    assert_eq!(
        state.kind_filter.iter().copied().collect::<Vec<_>>(),
        vec![2]
    );
    state.toggle_category(Category::Event);
    assert_eq!(state.category_filter, vec![Category::Sensor]);
    assert!(state.kind_filter.is_empty());
}

#[test]
fn rare_event_survives_sensor_identity_and_ack_history_eviction() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    let receiver = source("receiver-a");
    let rare = report(1, receiver.clone(), &event(4));
    state.retain(rare.clone());
    let bytes = sensor(4);
    for index in 0..SENSOR_ROWS + 64 {
        state.retain(report(index as u64 + 2, receiver.clone(), &bytes));
    }
    state.rebuild();

    assert_eq!(state.sensors.len(), SENSOR_ROWS);
    assert_eq!(state.evicted, 64);
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.visible.len(), 1);
    assert_eq!(state.visible[0].decoded.category, Category::Event);
    assert!(Arc::ptr_eq(&state.visible[0].record, &rare));
    assert!(
        state
            .snapshot()
            .iter()
            .any(|record| Arc::ptr_eq(record, &rare))
    );
    let retained_sensor = state
        .sensors
        .front()
        .expect("retained sensor row")
        .record
        .clone();
    let mut identity = [0; 64];
    identity[0] = 255;
    identity[1] = 4;
    identity[2..8].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
    let mut ack = [0; 64];
    ack[0] = 251;
    ack[2] = 2;
    ack[3] = 1;
    let start = SENSOR_ROWS as u64 + 66;
    for index in 0..OTHER_ROWS + 64 {
        let sequence = start + 2 * index as u64;
        state.retain(report(sequence, receiver.clone(), &identity));
        state.retain(report(sequence + 1, receiver.clone(), &ack));
    }
    state.rebuild();
    assert_eq!(state.others.len(), OTHER_ROWS);
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.visible.len(), 1);
    assert!(Arc::ptr_eq(&state.visible[0].record, &rare));
    let snapshot = state.snapshot();
    assert!(snapshot.iter().any(|record| Arc::ptr_eq(record, &rare)));
    assert!(
        snapshot
            .iter()
            .any(|record| Arc::ptr_eq(record, &retained_sensor))
    );
}

#[test]
fn snapshot_preserves_whole_reports_independently_of_visible_filters() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    let receiver = source("receiver-a");
    let mut mixed = event(4);
    mixed[16..32].copy_from_slice(&sensor(8)[..16]);
    mixed[32..48].copy_from_slice(&[0x90, 9, 8, 7, 6, 5, 4, 3, 2, 1, 10, 11, 12, 13, 14, 15]);
    let first = report(1, receiver.clone(), &mixed);
    let mut attempted = [0x98; 65];
    attempted[0] = 0;
    let output = Arc::new(Record {
        sequence: 2,
        elapsed_us: 2000,
        source: receiver,
        data: RecordData::Report {
            direction: Direction::Out,
            bytes: ReportBytes::new(&attempted).expect("report-ID-prefixed fixture"),
            transferred: Some(3),
            error: None,
        },
    });
    let other = report(3, source("receiver-b"), &event(4));
    state.retain(first.clone());
    state.retain(output.clone());
    state.retain(other.clone());
    state.receiver_filter.insert("receiver-a".into());
    state.tracker_filter.insert(4);
    state.category_filter = vec![Category::Event];
    state.kind_filter.insert(1);
    state.rebuild();
    assert_eq!(state.visible.len(), 1);
    assert_eq!(state.visible[0].decoded.byte_offset, 0);
    assert_eq!(state.visible[0].decoded.byte_len, 16);

    let snapshot = state.snapshot();
    assert_eq!(snapshot.len(), 3);
    assert!(Arc::ptr_eq(&snapshot[0], &first));
    assert!(Arc::ptr_eq(&snapshot[1], &output));
    assert!(Arc::ptr_eq(&snapshot[2], &other));
    match &snapshot[0].data {
        RecordData::Report { bytes, .. } => assert_eq!(bytes.as_slice(), &mixed),
        _ => panic!("mixed original report must remain a report"),
    }
    match &snapshot[1].data {
        RecordData::Report {
            direction,
            bytes,
            transferred,
            error,
        } => {
            assert_eq!(*direction, Direction::Out);
            assert_eq!(bytes.as_slice(), &attempted);
            assert_eq!(*transferred, Some(3));
            assert!(error.is_none());
        }
        _ => panic!("attempted original write must remain a report"),
    }
}

fn wait_for_file_results(state: &mut HidLogUiState, recording: bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while if recording {
        state.recorder.is_some()
    } else {
        state.file_worker.is_some()
    } {
        assert!(Instant::now() < deadline, "HID file job did not finish");
        state.poll_file_results();
        thread::yield_now();
    }
}

fn file_rows(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn publish_and_observe(
    state: &mut HidLogUiState,
    receiver: &Arc<Source>,
    bytes: &[u8],
) -> Arc<Record> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        trace::publish_report(receiver, Direction::In, bytes, Some(bytes.len()), None).unwrap();
        if let Some(trace::TraceItem::Record(record)) =
            state.subscription.as_mut().unwrap().try_recv()
            && record.source.connection_id == receiver.connection_id
            && let RecordData::Report {
                bytes: observed, ..
            } = &record.data
            && observed.as_slice() == bytes
        {
            return record;
        }
        assert!(Instant::now() < deadline, "live UI subscriber was stopped");
        thread::yield_now();
    }
}

#[test]
fn recording_stop_finalizes_during_pending_snapshot_and_can_restart() {
    let directory = tempfile::tempdir().unwrap();
    let recording_path = directory.path().join("recording.hidlog");
    let snapshot_path = directory.path().join("snapshot.hidlog");
    let restart_path = directory.path().join("restart.hidlog");
    let receiver = source("ui-record-stop-independent-snapshot");
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    state.retain(report(1, receiver.clone(), &event(4)));
    state.receiver_filter.insert(receiver.receiver_key.clone());
    state.watch_receiver = Some(receiver.receiver_key.clone());
    state.rebuild();
    let retained = state.snapshot();
    let selected = state.visible[0].clone();
    state.selected = Some(selected.clone());
    state.subscription = Some(trace::subscribe());
    assert!(state.can_start_recording());
    assert!(!state.can_stop_recording());
    state.recorder = Some(Recorder::start(recording_path.clone()).unwrap());
    state.recording_path = Some(recording_path.clone());

    // A real snapshot worker remains pending independently of the recorder.
    let (release, gate) = mpsc::channel();
    let snapshot = state.snapshot();
    state.file_worker = Some(thread::spawn(move || {
        gate.recv_timeout(Duration::from_secs(5)).unwrap();
        trace::save_snapshot(&snapshot_path, &snapshot, 0).map(|summary| (summary, snapshot_path))
    }));
    assert!(!state.can_start_recording());
    assert!(state.can_stop_recording());
    publish_and_observe(&mut state, &receiver, &event(4));
    assert!(state.stop_recording());
    assert!(state.stopping);
    assert!(!state.can_stop_recording());
    assert!(!state.stop_recording());
    assert!(
        state.recorder.is_some(),
        "retain the recorder until its worker finishes"
    );
    wait_for_file_results(&mut state, true);
    assert!(!state.stopping);
    assert!(state.recording_path.is_none());
    assert!(state.error.is_none(), "{:?}", state.error);
    assert!(
        state
            .notice
            .as_ref()
            .unwrap()
            .contains(&recording_path.display().to_string())
    );
    assert!(!state.file_worker.as_ref().unwrap().is_finished());
    assert!(
        !state.can_start_recording(),
        "snapshot still owns the start/file gate"
    );
    let saved = file_rows(&recording_path);
    assert_eq!(saved.last().unwrap()["type"], "end");
    assert!(
        saved
            .iter()
            .any(|row| row["connection_id"] == receiver.connection_id
                && row["data"]["bytes"] == serde_json::json!(event(4).to_vec()))
    );
    assert_eq!(state.snapshot().len(), retained.len());
    assert!(Arc::ptr_eq(&state.snapshot()[0], &retained[0]));
    assert!(Arc::ptr_eq(state.selected.as_ref().unwrap(), &selected));
    assert!(state.receiver_filter.contains(&receiver.receiver_key));
    assert_eq!(
        state.watch_receiver.as_deref(),
        Some(receiver.receiver_key.as_str())
    );
    assert!(state.subscription.is_some());

    // Stopping the file consumer leaves live display collection attached.
    let live = publish_and_observe(&mut state, &receiver, &event(7));
    state.retain(live);
    release.send(()).unwrap();
    wait_for_file_results(&mut state, false);
    assert!(state.error.is_none(), "{:?}", state.error);
    assert!(state.can_start_recording());
    state.recorder = Some(Recorder::start(restart_path.clone()).unwrap());
    state.recording_path = Some(restart_path.clone());
    assert!(state.can_stop_recording());
    publish_and_observe(&mut state, &receiver, &event(8));
    assert!(state.stop_recording());
    wait_for_file_results(&mut state, true);
    let restarted = file_rows(&restart_path);
    assert_eq!(restarted.last().unwrap()["type"], "end");
    assert!(
        restarted
            .iter()
            .any(|row| row["connection_id"] == receiver.connection_id
                && row["data"]["bytes"] == serde_json::json!(event(8).to_vec()))
    );
    assert!(state.can_start_recording());
}

#[test]
fn keyboard_navigation_uses_filtered_row_identity_and_stays_bounded() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    for sequence in 1..=6 {
        let receiver = if sequence % 2 == 0 {
            source("receiver-b")
        } else {
            source("receiver-a")
        };
        state.retain(report(sequence, receiver, &event(4)));
    }
    state.rebuild();
    state.selected = Some(state.visible[3].clone());
    state.receiver_filter.insert("receiver-b".into());
    state.rebuild();
    assert_eq!(state.selected_visible_index(), Some(1));
    assert!(state.navigate_rows("down", 2));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 6);
    assert!(state.navigate_rows("down", usize::MAX));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 6);
    assert!(state.navigate_rows("pageup", usize::MAX));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 2);
    assert!(state.navigate_rows("up", 0));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 2);

    state.receiver_filter = ["receiver-a".into()].into_iter().collect();
    state.rebuild();
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 2);
    assert_eq!(state.selected_visible_index(), None);
    assert!(state.navigate_rows("enter", 1));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 1);
    assert!(state.navigate_rows("end", 1));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 5);
    let selected = state.selected.clone();
    assert!(!state.navigate_rows("tab", 1));
    assert!(Arc::ptr_eq(
        state.selected.as_ref().unwrap(),
        selected.as_ref().unwrap()
    ));
    state.receiver_filter = ["absent".into()].into_iter().collect();
    state.rebuild();
    assert!(!state.navigate_rows("enter", 1));
    assert!(Arc::ptr_eq(
        state.selected.as_ref().unwrap(),
        selected.as_ref().unwrap()
    ));
}

#[test]
fn keyboard_navigation_distinguishes_decoded_rows_from_the_same_report() {
    let mut state = HidLogUiState {
        follow: false,
        ..Default::default()
    };
    let mut mixed = event(4);
    mixed[16..32].copy_from_slice(&event(7)[..16]);
    state.retain(report(42, source("receiver-a"), &mixed));
    state.rebuild();
    assert_eq!(state.visible.len(), 2);
    assert!(state.navigate_rows("enter", 1));
    assert_eq!(state.selected.as_ref().unwrap().decoded.byte_offset, 0);
    assert!(state.navigate_rows("down", 1));
    assert_eq!(state.selected.as_ref().unwrap().record.sequence, 42);
    assert_eq!(state.selected.as_ref().unwrap().decoded.byte_offset, 16);
    assert_eq!(state.selected_visible_index(), Some(1));
    state.tracker_filter.insert(7);
    state.rebuild();
    assert_eq!(state.selected_visible_index(), Some(0));
    assert!(state.navigate_rows("up", 1));
    assert_eq!(state.selected.as_ref().unwrap().decoded.byte_offset, 16);
}

struct HidLogPage(gpui_kit::Entity<super::Workbench>);

impl gpui_kit::Render for HidLogPage {
    fn render(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::prelude::*;
        self.0.update(cx, |this, cx| {
            let page = this.hid_log_page(window, cx);
            gpui_kit::div()
                .size_full()
                .track_focus(&this.page_focus)
                .tab_group()
                .flex()
                .flex_col()
                .child(page)
        })
    }
}

#[gpui_kit::test]
fn hid_rows_support_keyboard_pointer_and_virtualized_navigation_without_device_operations(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use gpui_kit::{VisualContext as _, test::TestWindowExt as _};

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let (manager, _listener) = crate::sessions::test_support::fixture();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(|window, cx| {
        let mut workbench =
            crate::ui::test_support::offline_workbench(window, cx, manager, runtime.executor());
        workbench.active_page = super::FeaturePage::HidLog;
        workbench.hid_log.follow = false;
        let receiver = source("receiver-keyboard-fixture");
        for sequence in 1..=120 {
            workbench
                .hid_log
                .retain(report(sequence, receiver.clone(), &event(4)));
        }
        workbench
    });
    let page = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        HidLogPage(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(page, window, cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(workbench.read(cx).page_focus.is_focused(window));
        for _ in 0..32 {
            window.press("tab", cx);
            if window.find("hid-log-navigation").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("hid-log-navigation").focused(), Some(true));
        let row = window.find("hid-log-select:1:0");
        assert_eq!(row.role(), Some(gpui_kit::Role::Button));
        assert!(row.label().unwrap().contains("receiver-keyboard-fixture"));
        window.press("enter", cx);
    });
    workbench.update(cx, |this, _| {
        assert_eq!(this.hid_log.selected.as_ref().unwrap().record.sequence, 1);
    });
    cx.update(|window, cx| {
        window.click("hid-log-select:2:0", cx);
    });
    workbench.update(cx, |this, _| {
        assert_eq!(this.hid_log.selected.as_ref().unwrap().record.sequence, 2);
    });
    cx.update(|window, cx| {
        window.press("end", cx);
        assert!(window.find("hid-log-select:120:0").visible());
        window.press("down", cx);
    });
    workbench.update(cx, |this, _| {
        assert_eq!(this.hid_log.selected.as_ref().unwrap().record.sequence, 120);
        assert!(!this.hid_log.follow);
    });
    let expected_page_rows = cx.update(|window, cx| {
        window.press("home", cx);
        let first = window.find("hid-log-select:1:0").bounds();
        let second = window.find("hid-log-select:2:0").bounds();
        let row_height = second.top() - first.top();
        let viewport_height = window.find("hid-log-list").bounds().size.height;
        let page_rows = (viewport_height / row_height).floor().max(1.) as u64;
        assert!(page_rows > 1 && page_rows < 119);
        window.press("pagedown", cx);
        page_rows
    });
    workbench.update(cx, |this, _| {
        let sequence = this.hid_log.selected.as_ref().unwrap().record.sequence;
        assert_eq!(
            sequence,
            1 + expected_page_rows,
            "one full viewport of rows"
        );
        assert_eq!(this.hid_log.visible.len(), 120);
        assert_eq!(this.hid_log.reports.len(), 120);
        assert!(this.manager.sessions().next().is_none());
    });
    cx.update(|window, cx| {
        window.press("pageup", cx);
    });
    workbench.update(cx, |this, _| {
        assert_eq!(this.hid_log.selected.as_ref().unwrap().record.sequence, 1);
    });
    cx.update(|window, cx| {
        window.press("end", cx);
        window.press("pageup", cx);
    });
    workbench.update(cx, |this, _| {
        assert_eq!(
            this.hid_log.selected.as_ref().unwrap().record.sequence,
            120 - expected_page_rows,
            "reverse page movement uses the same observed viewport"
        );
    });
    cx.update(|window, cx| {
        window.click("hid-log-clear", cx);
        window.press("enter", cx);
    });
    workbench.update(cx, |this, _| {
        assert!(this.hid_log.visible.is_empty());
        assert!(this.hid_log.selected.is_none());
    });
}
