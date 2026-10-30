use super::support::{query_link, seeded_query, target};
use crate::application::flash::{
    EsbAutoQuery, FlashState, OtaQueries, available_query_groups, query_tracker_eligible,
};
use crate::domain::TrackerLink;
use crate::features::firmware::esb_ota::RECEIVER_ID;
use std::time::{Duration, Instant};

#[test]
fn scoped_manual_query_targets_only_current_receiver_and_requested_slot() {
    use crate::application::flash::auto_query::scoped_query_group;
    let receiver = target("a", 0, "board").receiver;
    let requested = query_link("a", 1);
    let live = [
        query_link("a", 0),
        requested.clone(),
        query_link("b", 1),
        TrackerLink {
            online: false,
            ..query_link("a", 2)
        },
        TrackerLink {
            address: None,
            ..query_link("a", 3)
        },
    ];
    let group = scoped_query_group(&receiver, &receiver, Some(&requested), &live).unwrap();
    assert_eq!(group.0.key, "a");
    assert!(!group.2);
    assert_eq!(group.1.len(), 1);
    assert_eq!(group.1[0].slot, 1);
    assert_eq!(group.1[0].receiver_key, "a");

    let group = scoped_query_group(&receiver, &receiver, None, &live).unwrap();
    assert!(group.2);
    assert_eq!(
        group
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn scoped_manual_query_rejects_replaced_endpoint_slot_and_offline_identity() {
    use crate::application::flash::auto_query::scoped_query_group;
    let receiver = target("a", 0, "board").receiver;
    let requested = query_link("a", 1);
    for changed in ["path", "serial", "key"] {
        let mut current = receiver.clone();
        match changed {
            "path" => current.path = b"new endpoint".to_vec(),
            "serial" => current.serial_number = Some("new receiver".into()),
            _ => current.key = "b".into(),
        }
        assert!(scoped_query_group(&receiver, &current, None, &[]).is_none());
    }
    for changed in ["address", "offline", "unknown", "receiver", "slot"] {
        let mut current = requested.clone();
        match changed {
            "address" => current.address = Some("AABBCCDDEEFF".into()),
            "offline" => current.online = false,
            "unknown" => current.address = None,
            "receiver" => current.receiver_key = "b".into(),
            _ => current.slot = 2,
        }
        assert!(scoped_query_group(&receiver, &receiver, Some(&requested), &[current]).is_none());
    }
}

#[test]
fn ota_query_excludes_offline_and_unknown_address_slots() {
    let slots = [
        TrackerLink {
            receiver_key: "receiver".into(),
            slot: 0,
            address: Some("AABBCCDDEEFF".into()),
            online: false,
        },
        TrackerLink {
            receiver_key: "receiver".into(),
            slot: 1,
            address: None,
            online: true,
        },
        TrackerLink {
            receiver_key: "receiver".into(),
            slot: 2,
            address: Some("112233445566".into()),
            online: true,
        },
        TrackerLink {
            receiver_key: "receiver".into(),
            slot: 3,
            address: Some("invalid".into()),
            online: true,
        },
    ];
    let eligible = slots
        .iter()
        .filter(|tracker| query_tracker_eligible(tracker))
        .collect::<Vec<_>>();
    assert_eq!(eligible.len(), 1);
    assert_eq!(eligible[0].slot, 2);
}

#[test]
fn esb_entry_query_waits_for_discovery_and_busy_then_runs_only_once() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut query = EsbAutoQuery::default();
    query.enter(true);
    assert!(!query.take_ready(false, false, now));
    assert!(!query.take_ready(true, true, now));
    assert!(!query.take_ready(true, true, now + Duration::from_secs(2)));
    assert!(query.take_ready(true, false, now + Duration::from_secs(3)));
    for seconds in 4..8 {
        query.enter(true);
        assert!(!query.take_ready(true, false, now + Duration::from_secs(seconds)));
    }
    // Leaving before starting cancels the queued request; returning from
    // BLE (or another feature page) starts a new one-shot lifecycle.
    query.enter(false);
    query.enter(true);
    assert!(!query.take_ready(true, false, now + Duration::from_secs(8)));
    query.enter(false);
    assert!(!query.take_ready(true, false, now + Duration::from_secs(10)));
    query.enter(true);
    assert!(!query.take_ready(true, false, now + Duration::from_secs(11)));
    assert!(query.take_ready(true, false, now + Duration::from_secs(12)));
}

#[test]
fn capture_owned_receiver_does_not_block_or_consume_other_query_attempts() {
    let now = Instant::now();
    let receivers = [
        target("recording", 0, "board").receiver,
        target("available", 1, "board").receiver,
    ];
    let links = [query_link("recording", 0), query_link("available", 1)];
    let captured = Some("recording");
    let eligible = || {
        receivers
            .iter()
            .filter(|r| Some(r.key.as_str()) != captured)
    };
    let manual = available_query_groups(eligible(), links.iter());
    assert_eq!(manual.len(), 1);
    assert_eq!(manual[0].0.key, "available");
    let mut query = EsbAutoQuery::default();
    query.enter(true);
    for receiver in &receivers {
        query.observe(receiver, &links, now);
    }
    // Only the non-capture receiver needs an observation to settle entry.
    let available =
        eligible().next().is_some() && eligible().all(|receiver| receiver.key == "available");
    assert!(!query.take_ready(available, false, now));
    assert!(query.take_ready(available, false, now + Duration::from_secs(1)));
    let groups = query.take_incremental(
        eligible(),
        links.iter(),
        false,
        now + Duration::from_secs(1),
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0.key, "available");
    query.full_requested(&groups);
    // Capture exit resumes only the deferred receiver; B is not retried.
    let resumed = query.take_incremental(
        receivers.iter(),
        links.iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].0.key, "recording");
}

#[test]
fn incremental_query_settles_batches_only_new_targets_and_deduplicates_failed_attempts() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let mut links = vec![query_link("a", 0)];
    let mut query = seeded_query(std::slice::from_ref(&receiver), &links, now);
    links.extend([query_link("a", 1), query_link("a", 2)]);
    query.observe(&receiver, &links, now);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                links.iter(),
                false,
                now + Duration::from_millis(500)
            )
            .is_empty()
    );
    // Busy includes jobs, pending reservations and the file chooser; review
    // independently blocks automatic discovery without discarding arrivals.
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                links.iter(),
                true,
                now + Duration::from_secs(2)
            )
            .is_empty()
    );
    let groups = query.take_incremental(
        std::iter::once(&receiver),
        links.iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].2, "existing receiver must not be queried again");
    assert_eq!(
        groups[0]
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    // No result needs to be delivered: a failure to reserve/query is still
    // an attempt, not an instruction to retry every frame.
    for second in 3..10 {
        query.observe(&receiver, &links, now + Duration::from_secs(second));
        assert!(
            query
                .take_incremental(
                    std::iter::once(&receiver),
                    links.iter(),
                    false,
                    now + Duration::from_secs(second)
                )
                .is_empty()
        );
    }
}

#[test]
fn reservation_unknown_roster_and_traffic_expiry_never_create_an_arrival() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let live = query_link("a", 0);
    let mut query = seeded_query(
        std::slice::from_ref(&receiver),
        std::slice::from_ref(&live),
        now,
    );
    let unknown = TrackerLink {
        address: None,
        ..live.clone()
    };
    let expired = TrackerLink {
        online: false,
        ..live.clone()
    };
    for (second, links) in [
        (1, vec![]),
        (2, vec![unknown]),
        (6, vec![expired]),
        (10, vec![live.clone()]),
    ] {
        query.observe(&receiver, &links, now + Duration::from_secs(second));
        assert!(
            query
                .take_incremental(
                    std::iter::once(&receiver),
                    links.iter(),
                    false,
                    now + Duration::from_secs(second + 2)
                )
                .is_empty()
        );
    }
    // Even inventory absence alone does not make a cached identical RF
    // route a new identity when the monitor resumes.
    assert!(
        query
            .take_incremental(
                std::iter::empty(),
                std::iter::empty(),
                false,
                now + Duration::from_secs(20)
            )
            .is_empty()
    );
    query.observe(
        &receiver,
        std::slice::from_ref(&live),
        now + Duration::from_secs(21),
    );
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&live),
                false,
                now + Duration::from_secs(23)
            )
            .is_empty()
    );
}

#[test]
fn offline_unattempted_arrival_waits_for_fresh_online_settling() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let mut tracker = query_link("a", 1);
    tracker.online = false;
    let mut query = seeded_query(
        std::slice::from_ref(&receiver),
        std::slice::from_ref(&tracker),
        now,
    );
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&tracker),
                false,
                now + Duration::from_secs(4)
            )
            .is_empty()
    );
    tracker.online = true;
    query.observe(
        &receiver,
        std::slice::from_ref(&tracker),
        now + Duration::from_secs(5),
    );
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&tracker),
                false,
                now + Duration::from_millis(5500)
            )
            .is_empty()
    );
    let groups = query.take_incremental(
        std::iter::once(&receiver),
        std::iter::once(&tracker),
        false,
        now + Duration::from_secs(6),
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1[0].slot, 1);
    assert!(!groups[0].2);
}

#[test]
fn manual_full_refresh_supersedes_queued_work_and_retries_attempted_targets() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let mut links = vec![query_link("a", 0)];
    let mut query = seeded_query(std::slice::from_ref(&receiver), &links, now);
    links.push(query_link("a", 1));
    query.observe(&receiver, &links, now);
    let full = available_query_groups(std::slice::from_ref(&receiver), links.iter());
    assert!(full[0].2);
    assert_eq!(full[0].1.len(), 2);
    query.full_requested(&full);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                links.iter(),
                false,
                now + Duration::from_secs(2)
            )
            .is_empty()
    );
    // Refresh remains full even after a failed or successful prior attempt.
    assert_eq!(
        available_query_groups(std::slice::from_ref(&receiver), links.iter())[0]
            .1
            .len(),
        2
    );
}

#[test]
fn queued_address_replacement_discards_old_identity_and_normalizes_new_route() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let mut query = seeded_query(std::slice::from_ref(&receiver), &[], now);
    let old = query_link("a", 1);
    query.observe(&receiver, std::slice::from_ref(&old), now);
    let replacement = TrackerLink {
        address: Some("11:22:33:aa:bb:cc".into()),
        ..old.clone()
    };
    query.observe(
        &receiver,
        std::slice::from_ref(&replacement),
        now + Duration::from_secs(1),
    );
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&old),
                false,
                now + Duration::from_secs(3)
            )
            .is_empty()
    );
    let groups = query.take_incremental(
        std::iter::once(&receiver),
        std::iter::once(&replacement),
        false,
        now + Duration::from_secs(3),
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1[0].address.as_deref(), Some("112233AABBCC"));
    assert!(!groups[0].2);
    query.observe(
        &receiver,
        std::slice::from_ref(&old),
        now + Duration::from_secs(4),
    );
    assert_eq!(
        query.take_incremental(
            std::iter::once(&receiver),
            std::iter::once(&old),
            false,
            now + Duration::from_secs(5)
        )[0]
        .1[0]
            .address,
        old.address
    );
}

#[test]
fn incremental_routes_are_scoped_to_receiver_and_frozen_endpoint() {
    let now = Instant::now();
    let a = target("a", 0, "board").receiver;
    let mut b = target("b", 0, "board").receiver;
    let receivers = [a.clone(), b.clone()];
    let mut query = seeded_query(&receivers, &[], now);
    let links = [query_link("a", 1), query_link("b", 1)];
    query.observe(&a, &links[..1], now);
    query.observe(&b, &links[1..], now);
    let groups = query.take_incremental(
        std::iter::once(&a),
        links.iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0.key, "a");
    assert_eq!(groups[0].1[0].receiver_key, "a");
    b.path = b"replacement".to_vec();
    assert!(
        query
            .take_incremental(
                std::iter::once(&b),
                links.iter(),
                false,
                now + Duration::from_secs(2)
            )
            .is_empty()
    );
    query.observe(&b, &links[1..], now + Duration::from_secs(3));
    let groups = query.take_incremental(
        std::iter::once(&b),
        links.iter(),
        false,
        now + Duration::from_secs(4),
    );
    assert_eq!(groups.len(), 1);
    assert!(
        groups[0].2,
        "positive endpoint replacement needs receiver information"
    );
    assert_eq!(groups[0].0.path, b"replacement");
    assert_eq!(groups[0].1[0].receiver_key, "b");
}

#[test]
fn new_receiver_queries_itself_and_only_its_new_members_without_selecting_them() {
    let now = Instant::now();
    let a = target("a", 0, "board").receiver;
    let b = target("b", 0, "board").receiver;
    let old = query_link("a", 0);
    let new = query_link("b", 0);
    let mut query = seeded_query(std::slice::from_ref(&a), std::slice::from_ref(&old), now);
    query.observe(&b, std::slice::from_ref(&new), now);
    let groups = query.take_incremental(
        [&a, &b].into_iter(),
        [&old, &new].into_iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0.key, "b");
    assert!(groups[0].2);
    assert_eq!(groups[0].1.len(), 1);
    let mut state = FlashState::default();
    state.apply_queries(OtaQueries {
        targets: vec![
            target("b", RECEIVER_ID, "receiver"),
            target("b", 0, "board"),
        ],
        failures: vec![],
    });
    assert!(state.selected_targets.is_empty());
    assert!(state.assignments.is_empty());
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
}

#[test]
fn leaving_esb_defers_arrivals_until_targeted_entry_query() {
    let now = Instant::now();
    let receiver = target("a", 0, "board").receiver;
    let tracker = query_link("a", 1);
    let mut query = seeded_query(std::slice::from_ref(&receiver), &[], now);
    query.observe(&receiver, std::slice::from_ref(&tracker), now);
    query.enter(false);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&tracker),
                false,
                now + Duration::from_secs(2)
            )
            .is_empty()
    );
    query.enter(true);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&tracker),
                false,
                now + Duration::from_secs(3)
            )
            .is_empty()
    );
    assert!(!query.take_ready(true, false, now + Duration::from_secs(3)));
    assert!(query.take_ready(true, false, now + Duration::from_secs(4)));
    let groups = query.take_incremental(
        std::iter::once(&receiver),
        std::iter::once(&tracker),
        false,
        now + Duration::from_secs(4),
    );
    assert_eq!(groups.len(), 1);
    assert!(
        !groups[0].2,
        "page reentry must not resend a previously attempted receiver"
    );
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].slot, tracker.slot);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&tracker),
                false,
                now + Duration::from_secs(6)
            )
            .is_empty()
    );
}

struct CalibrationConsole {
    now: u64,
    input: std::collections::VecDeque<u8>,
    writes: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl crate::features::calibration::engine::fixture::DirectIo for CalibrationConsole {
    fn now_ms(&self) -> u64 {
        self.now
    }

    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
        self.now += 100;
        let count = bytes.len().min(self.input.len());
        for byte in &mut bytes[..count] {
            *byte = self.input.pop_front().unwrap();
        }
        Ok(count)
    }

    fn write_command(
        &mut self,
        bytes: &[u8],
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        let response = match bytes {
            b"help\n" => {
                "\
=== Available Commands ===\n\n\
Device Information:\n  info                       Get device information\n\n\
Sensor Management:\n  calibrate                  Calibrate sensor ZRO\n\n\
Connection:\n  pair                       Enter pairing mode\n\n\
System:\n  reboot                     Soft reset the device\n\n\
Other:\n  help                       Show this help message\n\n\
Debug Commands:\n  reset all                  Clear all settings\n\n\
Button Functions (current build):\n  No sw0 button is defined for this board\n\n"
            }
            b"info\n" => {
                "\
* SlimeVR Tracker *\n\
SlimeVR synthetic fixture\n\
Repo: https://example.invalid/tracker | Branch: fixture\n\n\
Board: fixture\nSOC: nrf52840\nTarget: fixture/nrf52840\n\n\
IMU: BMI270\nInterface: SPI\n\n\
Magnetometer: None (disabled)\n\n\
ODR: gyro 200.0Hz / accel 100.0Hz / mag n/a | fusion 100.0Hz\n\
Gyroscope bias: 0.10000 0.20000 0.30000\n\
Gyroscope sensitivity scale: 1.00000 1.00000 1.00000\n\n\
Fusion: VQF\n\n\
Tracker ID: None\nDevice address: AABBCCDDEEFF\n\
Receiver address: None\nRF Channel: 40 (default)\n\n\
Battery: Waiting for valid unplugged reading\n\
Remaining runtime: Not available\nFully charged runtime: Not available\n"
            }
            b"calibrate\n" => {
                "\
[00:00:01.000,000] <inf> cal_imu: Calibrating main accelerometer and gyroscope zero rate offset\n\
[00:00:01.001,000] <inf> cal_imu: Reading data\n\
[00:00:01.002,000] <inf> cal_imu: Calibration queued for next sensor frame\n"
            }
            _ => return Err("Unexpected calibration fixture command".into()),
        };
        self.input.extend(response.bytes());
        Ok(())
    }
}

#[test]
fn joined_unknown_calibration_allows_other_slot_commands_and_scoped_firmware_queries() {
    use crate::application::calibration::check_scoped_tracker_io;
    use crate::application::flash::auto_query::{
        calibration_query_tracker_admitted, retain_calibration_query_groups, scoped_query_group,
    };
    use crate::discovery::ReceiverMonitor;
    use crate::domain::{Inventory, SerialEndpoint, SerialPortKind};
    use crate::features::calibration::engine::{
        Application, CalibrationSession, CalibrationTask, Dispatch, GuideSelection, RunMode,
        Target, fixture,
    };
    use crate::features::commands::policy::{
        CommandContext, CommandSnapshot, RouteEvidence, presets,
    };
    use crate::sessions::test_support;
    use std::sync::{Arc, Mutex};

    let receiver = target("calibration-receiver", 2, "board").receiver;
    let endpoint = SerialEndpoint {
        port_name: "calibration-fixture".into(),
        label: "Synthetic USB tracker".into(),
        manufacturer: Some("SlimeVR".into()),
        product: Some("SlimeVR Tracker".into()),
        kind: SerialPortKind::Usb,
        serial_number: Some("SYNTHETIC-TRACKER".into()),
        vid: Some(0x1209),
        pid: Some(1),
        usb_interface: None,
    };
    let inventory = Inventory {
        receivers: vec![receiver.clone()],
        ports: vec![endpoint.clone()],
        ..Inventory::default()
    };
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 2, "AABBCCDDEEFF", true);
    let mut calibration = CalibrationSession::default();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let observed = writes.clone();
    calibration.install_fixture(Arc::new(move |_| {
        Ok(fixture::Transport::Direct(Box::new(CalibrationConsole {
            now: 0,
            input: Default::default(),
            writes: observed.clone(),
        })))
    }));
    let ticket = calibration
        .review(
            GuideSelection {
                target: Target::Direct {
                    endpoint,
                    generation: None,
                    address: Some("AABBCCDDEEFF".into()),
                },
                label: "Synthetic tracker 2".into(),
            },
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    calibration
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    let same = TrackerLink {
        receiver_key: receiver.key.clone(),
        slot: 2,
        address: Some("AABBCCDDEEFF".into()),
        online: true,
    };
    let unrelated = TrackerLink {
        receiver_key: receiver.key.clone(),
        slot: 7,
        address: Some("112233445566".into()),
        online: true,
    };
    let roster = [same.clone(), unrelated.clone()];
    assert!(check_scoped_tracker_io(&calibration, &receiver.key, "112233445566").is_err());
    let mut live_groups = available_query_groups([&receiver], roster.iter());
    retain_calibration_query_groups(&calibration, &mut live_groups);
    assert!(live_groups.is_empty());
    let deadline = Instant::now() + Duration::from_secs(3);
    while calibration.native_busy() {
        calibration.poll(&inventory, &mut manager, &mut monitor);
        assert!(
            Instant::now() < deadline,
            "calibration fixture did not join"
        );
        std::thread::yield_now();
    }
    let result = calibration.snapshot().unwrap();
    assert_eq!(result.application, Application::CandidateQueued);
    assert_eq!(result.dispatch, Dispatch::Written);
    assert!(result.joined);
    assert!(calibration.busy());
    assert_eq!(
        *writes.lock().unwrap(),
        [
            b"help\n".to_vec(),
            b"info\n".to_vec(),
            b"calibrate\n".to_vec(),
            b"info\n".to_vec(),
        ]
    );
    assert!(calibration.blocks_receiver(&receiver.key));
    assert!(check_scoped_tracker_io(&calibration, &receiver.key, "AABBCCDDEEFF").is_err());
    check_scoped_tracker_io(&calibration, &receiver.key, "112233445566").unwrap();
    let route = RouteEvidence::Forwarded {
        port: "receiver-console",
        generation: 4,
        connected: true,
        receiver_key: &receiver.key,
        slot: 7,
        address: unrelated.address.as_deref(),
        unique_receiver_route: true,
    };
    let preset = presets(CommandContext::ForwardedTracker)
        .find(|preset| preset.id == "remote-sens")
        .unwrap();
    let command = CommandSnapshot::capture(preset, "0,0,0", route).unwrap();
    command.authorize(route, true).unwrap();
    assert_eq!(command.command(), "send 7 sens 0,0,0");
    let mut scoped =
        vec![scoped_query_group(&receiver, &receiver, Some(&unrelated), &roster).unwrap()];
    retain_calibration_query_groups(&calibration, &mut scoped);
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].1.len(), 1);
    assert_eq!(scoped[0].1[0].slot, 7);
    assert_eq!(scoped[0].1[0].address.as_deref(), Some("112233445566"));
    assert!(!scoped[0].2);
    let mut same_query =
        vec![scoped_query_group(&receiver, &receiver, Some(&same), &roster).unwrap()];
    retain_calibration_query_groups(&calibration, &mut same_query);
    assert!(same_query.is_empty());
    let mut full = available_query_groups([&receiver], roster.iter());
    retain_calibration_query_groups(&calibration, &mut full);
    assert_eq!(full.len(), 1);
    assert_eq!(
        full[0]
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [7]
    );
    assert!(full[0].2);
    let now = Instant::now();
    let mut automatic = EsbAutoQuery::default();
    automatic.enter(true);
    automatic.observe(&receiver, &roster, now);
    assert!(!automatic.take_ready(true, false, now));
    assert!(automatic.take_ready(true, false, now + Duration::from_secs(2)));
    let before_ack = automatic.take_incremental(
        std::iter::once(&receiver),
        roster
            .iter()
            .filter(|tracker| calibration_query_tracker_admitted(&calibration, tracker)),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(
        before_ack[0]
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [7]
    );
    calibration.acknowledge().unwrap();
    let mut acknowledged = available_query_groups([&receiver], roster.iter());
    retain_calibration_query_groups(&calibration, &mut acknowledged);
    assert_eq!(
        acknowledged[0]
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [2, 7]
    );
    assert_eq!(
        calibration.snapshot().unwrap().application,
        Application::CandidateQueued
    );
    let after_ack = automatic.take_incremental(
        std::iter::once(&receiver),
        roster
            .iter()
            .filter(|tracker| calibration_query_tracker_admitted(&calibration, tracker)),
        false,
        now + Duration::from_secs(3),
    );
    assert_eq!(after_ack.len(), 1);
    assert!(!after_ack[0].2);
    assert_eq!(
        after_ack[0]
            .1
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [2]
    );
}
