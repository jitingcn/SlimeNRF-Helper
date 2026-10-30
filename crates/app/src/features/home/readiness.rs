use crate::{
    discovery::ReceiverObservation,
    domain::{Inventory, ReceiverEndpoint},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Checking,
    DiscoveryUnavailable,
    NoReceiver,
    ReaderUnavailable,
    Busy,
    WaitingForData,
    Receiving,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Readiness {
    receiver_count: Option<usize>,
    active_trackers: Option<usize>,
    status: Status,
}

impl Readiness {
    pub(crate) fn receiver_count(&self) -> Option<usize> {
        self.receiver_count
    }
    pub(crate) fn active_trackers(&self) -> Option<usize> {
        self.active_trackers
    }
    pub(crate) fn status(&self) -> Status {
        self.status
    }
}

pub(crate) struct ReceiverState<'a> {
    receiver: &'a ReceiverEndpoint,
    reserved: bool,
    observation: Option<&'a ReceiverObservation>,
}

impl<'a> ReceiverState<'a> {
    pub(crate) fn new(
        receiver: &'a ReceiverEndpoint,
        reserved: bool,
        observation: Option<&'a ReceiverObservation>,
    ) -> Self {
        Self {
            receiver,
            reserved,
            observation,
        }
    }
}

pub(crate) fn project<'a>(
    inventory: &Inventory,
    inventory_error: Option<&str>,
    scan_generation: u64,
    scanning_inventory: bool,
    receivers: impl Iterator<Item = ReceiverState<'a>>,
) -> Readiness {
    if scan_generation == 0 || (scan_generation == 1 && scanning_inventory) {
        return Readiness {
            receiver_count: None,
            active_trackers: None,
            status: Status::Checking,
        };
    }
    if inventory_error.is_some()
        || inventory.serial_enumeration_failed
        || !inventory.warnings.is_empty()
    {
        return Readiness {
            receiver_count: None,
            active_trackers: None,
            status: Status::DiscoveryUnavailable,
        };
    }
    let receiver_count = inventory.receivers.len();
    if receiver_count == 0 {
        return Readiness {
            receiver_count: Some(0),
            active_trackers: Some(0),
            status: Status::NoReceiver,
        };
    }

    let mut active_trackers = 0;
    let mut busy = false;
    let mut reader_unavailable = false;
    for state in receivers {
        busy |= state.reserved;
        match state.observation {
            Some(observation) if observation.error.is_none() => {
                active_trackers += observation
                    .trackers
                    .iter()
                    .filter(|tracker| tracker.receiver_key == state.receiver.key && tracker.online)
                    .count();
            }
            _ => reader_unavailable = true,
        }
    }
    let (status, active_trackers) = if busy {
        (Status::Busy, None)
    } else if reader_unavailable {
        (Status::ReaderUnavailable, None)
    } else if active_trackers > 0 {
        (Status::Receiving, Some(active_trackers))
    } else {
        (Status::WaitingForData, Some(0))
    };
    Readiness {
        receiver_count: Some(receiver_count),
        active_trackers,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::{Readiness, ReceiverState, Status, project};
    use crate::{
        discovery::ReceiverObservation,
        domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind, TrackerLink},
    };

    fn receiver(key: &str) -> ReceiverEndpoint {
        ReceiverEndpoint {
            key: key.into(),
            path: key.as_bytes().to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: None,
        }
    }

    fn observation(key: &str, slot: u8, online: bool) -> ReceiverObservation {
        let mut observation = ReceiverObservation::default();
        observation.trackers.push(TrackerLink {
            receiver_key: key.into(),
            slot,
            address: Some("112233445566".into()),
            online,
        });
        observation
    }

    fn summarize(
        inventory: &Inventory,
        error: Option<&str>,
        generation: u64,
        scanning: bool,
        observations: &[(&str, bool, &ReceiverObservation)],
    ) -> Readiness {
        project(
            inventory,
            error,
            generation,
            scanning,
            inventory.receivers.iter().map(|receiver| {
                let current = observations.iter().find(|(key, _, _)| *key == receiver.key);
                ReceiverState {
                    receiver,
                    reserved: current.is_some_and(|(_, reserved, _)| *reserved),
                    observation: current.map(|(_, _, observation)| *observation),
                }
            }),
        )
    }

    #[test]
    fn initial_scan_is_unknown_but_periodic_scans_keep_completed_results() {
        let mut inventory = Inventory::default();
        let unknown = Readiness {
            receiver_count: None,
            active_trackers: None,
            status: Status::Checking,
        };
        assert_eq!(summarize(&inventory, None, 0, false, &[]), unknown);
        assert_eq!(summarize(&inventory, None, 1, true, &[]), unknown);
        let absent = Readiness {
            receiver_count: Some(0),
            active_trackers: Some(0),
            status: Status::NoReceiver,
        };
        assert_eq!(summarize(&inventory, None, 1, false, &[]), absent);
        assert_eq!(summarize(&inventory, None, 2, true, &[]), absent);

        inventory.receivers.push(receiver("a"));
        let active = observation("a", 3, true);
        let observations = [("a", false, &active)];
        let receiving = Readiness {
            receiver_count: Some(1),
            active_trackers: Some(1),
            status: Status::Receiving,
        };
        assert_eq!(
            summarize(&inventory, None, 1, false, &observations),
            receiving
        );
        assert_eq!(
            summarize(&inventory, None, 2, true, &observations),
            receiving
        );
    }

    #[test]
    fn failed_or_partial_enumeration_never_trusts_retained_counts() {
        let mut inventory = Inventory {
            receivers: vec![receiver("a")],
            ..Inventory::default()
        };
        let active = observation("a", 3, true);
        let observations = [("a", false, &active)];
        let unavailable = Readiness {
            receiver_count: None,
            active_trackers: None,
            status: Status::DiscoveryUnavailable,
        };
        assert_eq!(
            summarize(&inventory, Some("scan failed"), 1, false, &observations),
            unavailable
        );
        assert_eq!(
            summarize(&inventory, Some("scan failed"), 2, true, &observations),
            unavailable
        );
        inventory.warnings.push("HID enumeration failed".into());
        assert_eq!(
            summarize(&inventory, None, 2, false, &observations),
            unavailable
        );
        inventory.receivers.clear();
        assert_eq!(summarize(&inventory, None, 2, false, &[]), unavailable);
        inventory.warnings.clear();
        inventory.serial_enumeration_failed = true;
        assert_eq!(summarize(&inventory, None, 2, false, &[]), unavailable);
        inventory.serial_enumeration_failed = false;
        assert_eq!(
            summarize(&inventory, None, 3, false, &[]).status,
            Status::NoReceiver
        );
    }

    #[test]
    fn registrations_expired_activity_and_direct_ports_are_not_recent_wireless_data() {
        let mut inventory = Inventory {
            ports: vec![SerialEndpoint {
                port_name: "synthetic-usb-tracker".into(),
                label: "USB tracker".into(),
                manufacturer: None,
                product: None,
                kind: SerialPortKind::Usb,
                serial_number: Some("direct-tracker".into()),
                vid: None,
                pid: None,
                usb_interface: None,
            }],
            ..Inventory::default()
        };
        assert_eq!(
            summarize(&inventory, None, 1, false, &[]),
            Readiness {
                receiver_count: Some(0),
                active_trackers: Some(0),
                status: Status::NoReceiver,
            }
        );
        inventory.receivers.push(receiver("a"));
        let mut observed = observation("a", 4, false);
        assert_eq!(
            summarize(&inventory, None, 1, false, &[("a", false, &observed)]),
            Readiness {
                receiver_count: Some(1),
                active_trackers: Some(0),
                status: Status::WaitingForData,
            }
        );
        observed.trackers[0].online = true;
        assert_eq!(
            summarize(&inventory, None, 1, false, &[("a", false, &observed)]).active_trackers,
            Some(1)
        );
        observed.trackers[0].online = false;
        assert_eq!(
            summarize(&inventory, None, 1, false, &[("a", false, &observed)]).status,
            Status::WaitingForData
        );
    }

    #[test]
    fn equal_slots_and_addresses_on_two_current_receivers_are_separate_scopes() {
        let mut inventory = Inventory {
            receivers: vec![receiver("a"), receiver("b")],
            ..Inventory::default()
        };
        let first = observation("a", 4, true);
        let second = observation("b", 4, true);
        let detached = observation("detached", 4, true);
        let observations = [
            ("a", false, &first),
            ("b", false, &second),
            ("detached", false, &detached),
        ];
        assert_eq!(
            summarize(&inventory, None, 1, false, &observations),
            Readiness {
                receiver_count: Some(2),
                active_trackers: Some(2),
                status: Status::Receiving,
            }
        );
        inventory.receivers.pop();
        assert_eq!(
            summarize(&inventory, None, 2, false, &observations).active_trackers,
            Some(1)
        );
        assert_eq!(
            summarize(&inventory, None, 2, false, &[("a", false, &detached)]).active_trackers,
            Some(0)
        );
    }

    #[test]
    fn busy_handoff_and_reader_errors_override_partial_traffic_not_receiver_count() {
        let inventory = Inventory {
            receivers: vec![receiver("a"), receiver("b")],
            ..Inventory::default()
        };
        let active = observation("a", 1, true);
        let empty = ReceiverObservation::default();
        let mut failed = observation("b", 2, true);
        failed.error = Some("reader disconnected with retained traffic".into());
        let unreadable = Readiness {
            receiver_count: Some(2),
            active_trackers: None,
            status: Status::ReaderUnavailable,
        };
        assert_eq!(
            summarize(&inventory, None, 1, false, &[("a", false, &active)]),
            unreadable
        );
        assert_eq!(
            summarize(
                &inventory,
                None,
                1,
                false,
                &[("a", false, &active), ("b", false, &failed)]
            ),
            unreadable
        );
        let busy = Readiness {
            receiver_count: Some(2),
            active_trackers: None,
            status: Status::Busy,
        };
        assert_eq!(
            summarize(
                &inventory,
                None,
                1,
                false,
                &[("a", true, &empty), ("b", false, &failed)]
            ),
            busy
        );
        assert_eq!(
            summarize(
                &inventory,
                None,
                2,
                true,
                &[("a", false, &active), ("b", true, &empty)]
            ),
            busy
        );
        assert_eq!(
            summarize(
                &inventory,
                None,
                2,
                false,
                &[("a", false, &active), ("b", false, &empty)]
            ),
            Readiness {
                receiver_count: Some(2),
                active_trackers: Some(1),
                status: Status::Receiving,
            }
        );
    }
}
