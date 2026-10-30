use super::{Carrier, RegistryUiState};
use crate::{
    discovery::ReceiverMonitor,
    domain::{DeviceRole, Inventory, SerialEndpoint, TrackerLink, receiver_port},
    features::{
        device_registry::model::{DeviceKey, ObservedDevices, ReportedMetadata},
        firmware::esb_ota::{self, Target},
    },
    sessions::{ConnectionState, SessionManager},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{DefaultHasher, Hash, Hasher},
};

pub(super) struct RegistrySources<'a> {
    pub(super) inventory: &'a Inventory,
    pub(super) manager: &'a SessionManager,
    pub(super) monitor: &'a ReceiverMonitor,
    pub(super) targets: &'a [Target],
    pub(super) inventory_failed: bool,
}

impl RegistrySources<'_> {
    fn fingerprint(&self) -> u64 {
        let mut fingerprint = DefaultHasher::new();
        self.inventory
            .serial_enumeration_failed
            .hash(&mut fingerprint);
        self.inventory_failed.hash(&mut fingerprint);
        self.inventory.warnings.is_empty().hash(&mut fingerprint);
        for receiver in &self.inventory.receivers {
            receiver.key.hash(&mut fingerprint);
            receiver.path.hash(&mut fingerprint);
            receiver.serial_number.hash(&mut fingerprint);
            receiver.label.hash(&mut fingerprint);
            if let Some(observation) = self.monitor.observation_for(receiver) {
                observation.error.is_some().hash(&mut fingerprint);
                for tracker in &observation.trackers {
                    tracker.slot.hash(&mut fingerprint);
                    tracker.receiver_key.hash(&mut fingerprint);
                    tracker.address.hash(&mut fingerprint);
                    tracker.online.hash(&mut fingerprint);
                }
            }
        }
        for endpoint in &self.inventory.ports {
            endpoint.port_name.hash(&mut fingerprint);
            endpoint.serial_number.hash(&mut fingerprint);
            endpoint.label.hash(&mut fingerprint);
            endpoint.vid.hash(&mut fingerprint);
            endpoint.pid.hash(&mut fingerprint);
            std::mem::discriminant(&endpoint.kind).hash(&mut fingerprint);
            if let Some(interface) = &endpoint.usb_interface {
                interface.number.hash(&mut fingerprint);
                std::mem::discriminant(&interface.description).hash(&mut fingerprint);
                if let crate::domain::InterfaceDescription::Value(value) = &interface.description {
                    value.hash(&mut fingerprint);
                }
            }
        }
        for session in self.manager.sessions() {
            session.port.hash(&mut fingerprint);
            session.generation.hash(&mut fingerprint);
            (session.state == ConnectionState::Connected).hash(&mut fingerprint);
            self.current_endpoint(&session.port, session.generation)
                .is_some()
                .hash(&mut fingerprint);
            session.identity.name.hash(&mut fingerprint);
            session.identity.address.hash(&mut fingerprint);
            session.identity.board.hash(&mut fingerprint);
            session.identity.target.hash(&mut fingerprint);
            std::mem::discriminant(&session.identity.role).hash(&mut fingerprint);
        }
        for target in self.targets {
            target.receiver.key.hash(&mut fingerprint);
            target.receiver.path.hash(&mut fingerprint);
            target.receiver.serial_number.hash(&mut fingerprint);
            target.id.hash(&mut fingerprint);
            target.address.hash(&mut fingerprint);
            target.info.version.hash(&mut fingerprint);
            target.info.board_target.hash(&mut fingerprint);
        }
        fingerprint.finish()
    }

    fn current_endpoint(&self, port: &str, generation: u64) -> Option<&SerialEndpoint> {
        let endpoint = self
            .inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)?;
        self.manager.connected_identity_for(endpoint, generation)?;
        Some(endpoint)
    }

    fn tracker_metadata(&self, tracker: &TrackerLink) -> ReportedMetadata {
        self.targets
            .iter()
            .find(|target| {
                target.id == tracker.slot
                    && target.receiver.key == tracker.receiver_key
                    && target.address == tracker.address
                    && self.inventory.receivers.iter().any(|receiver| {
                        receiver.key == tracker.receiver_key
                            && receiver.path == target.receiver.path
                            && receiver.serial_number == target.receiver.serial_number
                    })
            })
            .map_or_else(ReportedMetadata::default, |target| ReportedMetadata {
                target: Some(target.info.board_target.clone()),
                firmware: Some(target.info.version.clone()),
                ..Default::default()
            })
    }
}

impl RegistryUiState {
    pub(super) fn reconcile_sources(&mut self, sources: RegistrySources<'_>) -> bool {
        let fingerprint = sources.fingerprint();
        if self.fingerprint == Some(fingerprint) && self.absent.is_empty() {
            return false;
        }
        self.fingerprint = Some(fingerprint);
        let mut observed = ObservedDevices::default();
        let mut carriers: BTreeMap<DeviceKey, BTreeSet<Carrier>> = BTreeMap::new();
        let mut current_carriers: BTreeMap<DeviceKey, BTreeSet<Carrier>> = BTreeMap::new();
        let mut candidates = BTreeSet::new();
        let mut sessions = BTreeMap::new();
        if !sources.inventory_failed {
            for receiver in &sources.inventory.receivers {
                if let Some(key) = receiver
                    .serial_number
                    .as_deref()
                    .and_then(DeviceKey::receiver_serial)
                {
                    candidates.insert(key);
                }
                let Some(key) = DeviceKey::receiver(receiver, sources.inventory) else {
                    continue;
                };
                let contradictory = sources.manager.sessions().any(|session| {
                    let Some(endpoint) =
                        sources.current_endpoint(&session.port, session.generation)
                    else {
                        return false;
                    };
                    session.identity.role == DeviceRole::Tracker
                        && receiver_port(receiver, sources.inventory)
                            .is_some_and(|port| port.port_name == endpoint.port_name)
                });
                if contradictory {
                    continue;
                }
                let mut reported = sources
                    .manager
                    .sessions()
                    .find_map(|session| {
                        let endpoint =
                            sources.current_endpoint(&session.port, session.generation)?;
                        receiver_port(receiver, sources.inventory)
                            .filter(|port| port.port_name == endpoint.port_name)?;
                        Some(ReportedMetadata::from_identity(&session.identity))
                    })
                    .unwrap_or_default();
                if reported.name.is_none() {
                    reported.name = Some(receiver.label.clone());
                }
                if let Some(target) = sources.targets.iter().find(|target| {
                    target.id == esb_ota::RECEIVER_ID
                        && target.receiver.key == receiver.key
                        && target.receiver.path == receiver.path
                        && target.receiver.serial_number == receiver.serial_number
                }) {
                    if reported.target.is_none() {
                        reported.target = Some(target.info.board_target.clone());
                    }
                    reported.firmware = Some(target.info.version.clone());
                }
                observed.add_receiver(receiver, sources.inventory, reported);
                let carrier = Carrier::Receiver(key);
                carriers.entry(key).or_default().insert(carrier.clone());
                current_carriers
                    .entry(key)
                    .or_default()
                    .insert(carrier.clone());
                if let Some(observation) = sources.monitor.observation_for(receiver) {
                    for tracker in &observation.trackers {
                        if tracker.receiver_key != receiver.key {
                            continue;
                        }
                        if let Some(tracker_key) = DeviceKey::tracker(tracker) {
                            candidates.insert(tracker_key);
                            observed.add_tracker(
                                tracker,
                                observation.error.is_none().then_some(key),
                                sources.tracker_metadata(tracker),
                            );
                            carriers
                                .entry(tracker_key)
                                .or_default()
                                .insert(carrier.clone());
                            if tracker.online {
                                current_carriers
                                    .entry(tracker_key)
                                    .or_default()
                                    .insert(carrier.clone());
                            }
                        }
                    }
                }
            }
            for session in sources
                .manager
                .sessions()
                .filter(|session| session.state == ConnectionState::Connected)
            {
                let Some(endpoint) = sources.current_endpoint(&session.port, session.generation)
                else {
                    continue;
                };
                if session.identity.role == DeviceRole::Tracker
                    && endpoint.vid == Some(0x1209)
                    && endpoint.pid == Some(0x7690)
                {
                    continue;
                }
                if let Some(key) = DeviceKey::direct_tracker(&session.identity) {
                    candidates.insert(key);
                }
                let key = if session.identity.role == DeviceRole::Tracker {
                    observed.add_direct_tracker(&session.identity);
                    DeviceKey::direct_tracker(&session.identity)
                } else {
                    sources
                        .inventory
                        .receivers
                        .iter()
                        .find(|receiver| {
                            receiver_port(receiver, sources.inventory)
                                .is_some_and(|port| port.port_name == endpoint.port_name)
                        })
                        .and_then(|receiver| DeviceKey::receiver(receiver, sources.inventory))
                };
                if let Some(key) = key {
                    sessions.insert(session.port.clone(), (session.generation, key));
                    let carrier = Carrier::Serial {
                        serial: endpoint.serial_number.clone(),
                        vid: endpoint.vid,
                        pid: endpoint.pid,
                    };
                    carriers.entry(key).or_default().insert(carrier.clone());
                    current_carriers.entry(key).or_default().insert(carrier);
                }
            }
        }
        let current = observed
            .observations()
            .filter(|observation| observed.is_current(&observation.key))
            .map(|observation| observation.key)
            .collect::<BTreeSet<_>>();
        sessions.retain(|_, (_, key)| observed.get(key).is_some());
        let observed = observed
            .into_observations()
            .into_iter()
            .map(|item| (item.key, item))
            .collect::<BTreeMap<_, _>>();
        let changed =
            self.current != current || self.observed != observed || !self.absent.is_empty();
        self.current = current;
        self.sessions.retain(|port, (generation, _)| {
            sources.manager.session(port).is_some_and(|session| {
                session.generation == *generation && session.state != ConnectionState::Connected
            })
        });
        self.ambiguous = candidates
            .into_iter()
            .filter(|key| !observed.contains_key(key))
            .collect();
        self.sessions.extend(sessions);
        for (key, carriers) in carriers {
            if observed.contains_key(&key) {
                self.carriers
                    .insert(key, current_carriers.remove(&key).unwrap_or(carriers));
            }
        }
        if self.observed != observed || !self.absent.is_empty() {
            self.observed = observed;
            self.pending_observe = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::RegistrySources;
    use crate::{
        application::device_registry::RegistryUiState,
        discovery::ReceiverMonitor,
        domain::{Inventory, ReceiverEndpoint},
        features::{
            device_registry::{
                model::DeviceKey,
                store::{RegistryEdit, RegistryStore},
            },
            firmware::esb_ota::{FirmwareInfo, Target},
        },
        sessions::SessionManager,
    };

    fn receiver() -> ReceiverEndpoint {
        ReceiverEndpoint {
            key: "hid:synthetic".into(),
            path: b"synthetic-path".to_vec(),
            serial_number: Some("0011223344556677".into()),
            label: "Native receiver".into(),
        }
    }

    fn reconcile(
        state: &mut RegistryUiState,
        inventory: &Inventory,
        monitor: &ReceiverMonitor,
        inventory_failed: bool,
    ) -> bool {
        state.reconcile_sources(RegistrySources {
            inventory,
            manager: &SessionManager::new(),
            monitor,
            targets: &[],
            inventory_failed,
        })
    }

    #[test]
    fn positive_batch_then_unchanged_and_tps_only_ticks_dispatch_nothing() {
        let receiver = receiver();
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        let manager = SessionManager::new();
        let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 3, "AABBCCDDEEFF", true);
        let mut state = RegistryUiState::default();
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false
        }));
        let RegistryEdit::Observe {
            observations,
            absent,
        } = state.next_observation_edit()
        else {
            panic!("observation batch required");
        };
        assert!(absent.is_empty());
        assert_eq!(observations.len(), 2);
        assert_eq!(
            observations[0].key,
            DeviceKey::receiver_serial("0011223344556677").unwrap()
        );
        assert_eq!(
            observations[0].reported.name.as_deref(),
            Some("Native receiver")
        );
        assert_eq!(
            observations[1].key,
            DeviceKey::tracker_address("AABBCCDDEEFF").unwrap()
        );
        assert!(!state.pending_observe);
        for tps in [0, 20, 100, 500] {
            monitor.fixture_tps(tps);
            assert_eq!(monitor.observation(&receiver.key).unwrap().tps(3), tps);
            assert!(!state.reconcile_sources(RegistrySources {
                inventory: &inventory,
                manager: &manager,
                monitor: &monitor,
                targets: &[],
                inventory_failed: false
            }));
            assert!(!state.pending_observe);
        }
        monitor = ReceiverMonitor::fixture(receiver, 9, "AABBCCDDEEFF", true);
        assert!(!state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false
        }));
        assert!(!state.pending_observe);
    }

    #[test]
    fn unique_offline_pairing_is_admitted_without_becoming_current() {
        let receiver = receiver();
        let mut inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        let manager = SessionManager::new();
        let monitor = ReceiverMonitor::fixture(receiver.clone(), 3, "AABBCCDDEEFF", false);
        let mut state = RegistryUiState::default();
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false,
        }));
        let tracker = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        let receiver_key = DeviceKey::receiver_serial("0011223344556677").unwrap();
        assert!(state.observed.contains_key(&tracker));
        assert!(!state.current.contains(&tracker));
        assert!(state.observed.contains_key(&receiver_key));
        assert!(state.current.contains(&receiver_key));
        assert!(!state.ambiguous.contains(&tracker));

        let mut duplicate = receiver;
        duplicate.key = "hid:duplicate".into();
        duplicate.path = b"duplicate-path".to_vec();
        inventory.receivers.push(duplicate);
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false,
        }));
        assert!(!state.observed.contains_key(&receiver_key));
        assert!(!state.observed.contains_key(&tracker));
    }

    #[test]
    fn cached_firmware_at_reused_receiver_path_and_slot_is_not_reported() {
        let receiver = receiver();
        let mut replaced = receiver.clone();
        replaced.serial_number = Some("8899AABBCCDDEEFF".into());
        let info = FirmwareInfo {
            version: "wrong-device-version".into(),
            build_date: "synthetic".into(),
            firmware_size: 0,
            bootloader: "synthetic".into(),
            protocol_version: 1,
            board_target: "wrong-device-board".into(),
            flash_base: 0,
        };
        let targets = vec![
            Target {
                receiver: replaced.clone(),
                id: crate::features::firmware::esb_ota::RECEIVER_ID,
                address: None,
                info: info.clone(),
            },
            Target {
                receiver: replaced,
                id: 3,
                address: Some("AABBCCDDEEFF".into()),
                info,
            },
        ];
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        let manager = SessionManager::new();
        let monitor = ReceiverMonitor::fixture(receiver, 3, "AABBCCDDEEFF", true);
        let mut state = RegistryUiState::default();
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &targets,
            inventory_failed: false
        }));
        let RegistryEdit::Observe { observations, .. } = state.next_observation_edit() else {
            panic!("observation batch required");
        };
        assert_eq!(observations.len(), 2);
        for observation in observations {
            assert_eq!(observation.reported.firmware, None);
            assert_eq!(observation.reported.target, None);
        }
    }

    #[test]
    fn last_receiver_moves_to_live_b_and_survives_stale_a_after_restart() {
        let first = receiver();
        let mut second = first.clone();
        second.key = "hid:second".into();
        second.path = b"second-path".to_vec();
        second.serial_number = Some("8899AABBCCDDEEFF".into());
        let tracker = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        let first_key = DeviceKey::receiver_serial("0011223344556677").unwrap();
        let second_key = DeviceKey::receiver_serial("8899AABBCCDDEEFF").unwrap();
        for reverse in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("device-registry.json");
            let mut store = RegistryStore::load(path.clone()).unwrap();
            let mut state = RegistryUiState::default();
            let mut inventory = Inventory {
                receivers: vec![first.clone()],
                ..Default::default()
            };
            let monitor = ReceiverMonitor::fixture(first.clone(), 3, "AABBCCDDEEFF", true);
            assert!(reconcile(&mut state, &inventory, &monitor, false));
            store.apply(state.next_observation_edit()).unwrap();
            assert_eq!(
                store.snapshot().get(&tracker).unwrap().last_receiver,
                Some(first_key)
            );
            inventory.receivers.push(second.clone());
            if reverse {
                inventory.receivers.reverse();
            }
            let mut monitor = ReceiverMonitor::fixture(first.clone(), 3, "AABBCCDDEEFF", false);
            monitor.fixture_extend(second.clone(), 9, "AABBCCDDEEFF", true);
            assert!(reconcile(&mut state, &inventory, &monitor, false));
            store.apply(state.next_observation_edit()).unwrap();
            assert_eq!(
                store.snapshot().get(&tracker).unwrap().last_receiver,
                Some(second_key)
            );
            inventory.receivers = vec![first.clone()];
            let monitor = ReceiverMonitor::fixture(first.clone(), 3, "AABBCCDDEEFF", false);
            assert!(reconcile(&mut state, &inventory, &monitor, false));
            assert!(!state.current.contains(&tracker));
            store.apply(state.next_observation_edit()).unwrap();
            assert_eq!(
                store.snapshot().get(&tracker).unwrap().last_receiver,
                Some(second_key)
            );

            let mut restarted = RegistryStore::load(path).unwrap();
            let mut state = RegistryUiState::default();
            assert!(reconcile(&mut state, &inventory, &monitor, false));
            restarted.apply(state.next_observation_edit()).unwrap();
            assert_eq!(
                restarted.snapshot().get(&tracker).unwrap().last_receiver,
                Some(second_key)
            );
            let replacement = ReceiverMonitor::fixture(first.clone(), 3, "112233445566", true);
            assert!(reconcile(&mut state, &inventory, &replacement, false));
            restarted.apply(state.next_observation_edit()).unwrap();
            let snapshot = restarted.snapshot();
            assert_eq!(
                snapshot.get(&tracker).unwrap().last_receiver,
                Some(second_key)
            );
            assert_eq!(
                snapshot
                    .get(&DeviceKey::tracker_address("112233445566").unwrap())
                    .unwrap()
                    .last_receiver,
                Some(first_key)
            );
            assert!(reconcile(&mut state, &inventory, &monitor, true));
            restarted.apply(state.next_observation_edit()).unwrap();
            assert_eq!(
                restarted.snapshot().get(&tracker).unwrap().last_receiver,
                Some(second_key)
            );
        }
    }

    #[test]
    fn stale_monitor_cannot_stamp_replacement_serial_or_path_onto_old_roster() {
        let original = receiver();
        let monitor = ReceiverMonitor::fixture(original.clone(), 3, "AABBCCDDEEFF", true);
        let tracker = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        for change_serial in [false, true] {
            let mut replacement = original.clone();
            if change_serial {
                replacement.serial_number = Some("8899AABBCCDDEEFF".into());
            } else {
                replacement.path = b"replacement-path".to_vec();
            }
            let inventory = Inventory {
                receivers: vec![replacement.clone()],
                ..Default::default()
            };
            let mut state = RegistryUiState::default();
            assert!(reconcile(&mut state, &inventory, &monitor, false));
            assert_eq!(state.observed.get(&tracker), None);
            assert!(!state.current.contains(&tracker));
            assert_eq!(state.observed.len(), 1);
            let current = ReceiverMonitor::fixture(replacement.clone(), 3, "AABBCCDDEEFF", true);
            assert!(reconcile(&mut state, &inventory, &current, false));
            assert_eq!(
                state.observed.get(&tracker).unwrap().last_receiver,
                replacement
                    .serial_number
                    .as_deref()
                    .and_then(DeviceKey::receiver_serial)
            );
            assert!(state.current.contains(&tracker));
        }
    }

    #[test]
    fn mismatched_roster_key_and_monitor_errors_cannot_supply_receiver_evidence() {
        let receiver = receiver();
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        let tracker = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 3, "AABBCCDDEEFF", true);
        monitor.fixture_observation_mut(&receiver).trackers[0].receiver_key = "hid:other".into();
        let mut state = RegistryUiState::default();
        assert!(reconcile(&mut state, &inventory, &monitor, false));
        assert_eq!(state.observed.get(&tracker), None);
        assert_eq!(state.observed.len(), 1);
        monitor.fixture_observation_mut(&receiver).trackers[0].receiver_key = receiver.key.clone();
        monitor.fixture_observation_mut(&receiver).error = Some("Synthetic reader error".into());
        assert!(reconcile(&mut state, &inventory, &monitor, false));
        assert_eq!(state.observed.get(&tracker).unwrap().last_receiver, None);
        monitor.fixture_observation_mut(&receiver).error = None;
        assert!(reconcile(&mut state, &inventory, &monitor, false));
        assert_eq!(
            state.observed.get(&tracker).unwrap().last_receiver,
            DeviceKey::receiver_serial("0011223344556677")
        );
        let mut duplicate = monitor.fixture_observation_mut(&receiver).trackers[0].clone();
        duplicate.slot = 7;
        monitor
            .fixture_observation_mut(&receiver)
            .trackers
            .push(duplicate);
        assert!(reconcile(&mut state, &inventory, &monitor, false));
        assert_eq!(state.observed.get(&tracker), None);
        assert!(!state.current.contains(&tracker));
    }
}
