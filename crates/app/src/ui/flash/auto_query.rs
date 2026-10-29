use super::*;

pub(super) fn query_failure(receiver: &ReceiverEndpoint, id: u8, error: String) -> QueryFailure {
    query_failure_identity(&receiver.key, id, error)
}

fn query_failure_identity(receiver_key: &str, id: u8, error: String) -> QueryFailure {
    (format!("{receiver_key}:{id}"), error)
}

impl From<firmware_update::QueryResults> for OtaQueries {
    fn from(query: firmware_update::QueryResults) -> Self {
        Self {
            targets: query.targets,
            failures: query
                .failures
                .into_iter()
                .map(|failure| {
                    query_failure_identity(&failure.receiver_key, failure.id, failure.error)
                })
                .collect(),
        }
    }
}

pub(super) fn query_tracker_eligible(tracker: &TrackerLink) -> bool {
    tracker.online
        && tracker
            .address
            .as_deref()
            .and_then(crate::domain::normalize_address)
            .is_some()
}

pub(super) fn available_query_groups<'a>(
    receivers: impl IntoIterator<Item = &'a ReceiverEndpoint>,
    trackers: impl Iterator<Item = &'a TrackerLink> + Clone,
) -> Vec<(ReceiverEndpoint, Vec<TrackerLink>, bool)> {
    receivers
        .into_iter()
        .map(|receiver| {
            let eligible = trackers
                .clone()
                .filter(|tracker| {
                    tracker.receiver_key == receiver.key && query_tracker_eligible(tracker)
                })
                .cloned()
                .collect();
            (receiver.clone(), eligible, true)
        })
        .collect()
}

fn query_endpoint_matches(expected: &ReceiverEndpoint, current: &ReceiverEndpoint) -> bool {
    expected.key == current.key
        && expected.path == current.path
        && expected.serial_number == current.serial_number
}

fn query_tracker_matches(expected: &TrackerLink, current: &TrackerLink) -> bool {
    query_tracker_eligible(current)
        && expected.receiver_key == current.receiver_key
        && expected.slot == current.slot
        && expected
            .address
            .as_deref()
            .and_then(crate::domain::normalize_address)
            == current
                .address
                .as_deref()
                .and_then(crate::domain::normalize_address)
}

pub(super) fn scoped_query_group(
    expected: &ReceiverEndpoint,
    current: &ReceiverEndpoint,
    tracker: Option<&TrackerLink>,
    live: &[TrackerLink],
) -> Option<(ReceiverEndpoint, Vec<TrackerLink>, bool)> {
    if !query_endpoint_matches(expected, current) {
        return None;
    }
    let trackers = if let Some(expected_tracker) = tracker {
        if expected_tracker.receiver_key != current.key {
            return None;
        }
        vec![
            live.iter()
                .find(|live| query_tracker_matches(expected_tracker, live))?
                .clone(),
        ]
    } else {
        live.iter()
            .filter(|live| live.receiver_key == current.key && query_tracker_eligible(live))
            .cloned()
            .collect()
    };
    Some((current.clone(), trackers, tracker.is_none()))
}

pub(super) fn calibration_query_tracker_admitted(
    calibration: &crate::calibration::CalibrationSession,
    tracker: &TrackerLink,
) -> bool {
    tracker.address.as_deref().is_some_and(|address| {
        !calibration.blocks_scoped_tracker_io(&tracker.receiver_key, address)
    })
}

pub(super) fn retain_calibration_query_groups(
    calibration: &crate::calibration::CalibrationSession,
    groups: &mut Vec<(ReceiverEndpoint, Vec<TrackerLink>, bool)>,
) {
    groups.retain_mut(|(receiver, trackers, include_receiver)| {
        if calibration.blocks_receiver_io(&receiver.key) {
            return false;
        }
        trackers.retain(|tracker| calibration_query_tracker_admitted(calibration, tracker));
        *include_receiver || !trackers.is_empty()
    });
}

impl EsbAutoQuery {
    pub(super) fn enter(&mut self, active: bool) {
        if self.active != active {
            self.active = active;
            self.queued = active;
            self.discovery_since = None;
        }
    }

    pub(super) fn take_ready(&mut self, available: bool, blocked: bool, now: Instant) -> bool {
        if !self.queued || !available {
            self.discovery_since = None;
            return false;
        }
        // Let the existing passive reader publish its initial address/traffic
        // records. This is a one-shot settling window, not another scan.
        let since = self.discovery_since.get_or_insert(now);
        if blocked || now.saturating_duration_since(*since) < Duration::from_secs(1) {
            return false;
        }
        self.queued = false;
        for receiver in &mut self.receivers {
            if receiver
                .queried_at
                .is_some_and(|at| now.saturating_duration_since(at) >= QUERY_CACHE_TTL)
            {
                receiver.attempted = false;
            }
            for tracker in &mut receiver.trackers {
                if tracker
                    .queried_at
                    .is_some_and(|at| now.saturating_duration_since(at) >= QUERY_CACHE_TTL)
                {
                    tracker.attempted = false;
                }
            }
        }
        true
    }

    pub(super) fn observe(
        &mut self,
        endpoint: &ReceiverEndpoint,
        trackers: &[TrackerLink],
        now: Instant,
    ) {
        let index = self
            .receivers
            .iter()
            .position(|known| known.endpoint.key == endpoint.key);
        let receiver = if let Some(index) = index {
            let known = &mut self.receivers[index];
            if known.endpoint.path != endpoint.path
                || known.endpoint.serial_number != endpoint.serial_number
            {
                *known = AutoQueryReceiver {
                    endpoint: endpoint.clone(),
                    attempted: false,
                    queried_at: None,
                    since: now,
                    trackers: Vec::new(),
                };
            }
            known
        } else {
            self.receivers.push(AutoQueryReceiver {
                endpoint: endpoint.clone(),
                attempted: false,
                queried_at: None,
                since: now,
                trackers: Vec::new(),
            });
            self.receivers.last_mut().unwrap()
        };
        for tracker in trackers {
            let Some(raw_address) = tracker.address.as_deref() else {
                continue;
            };
            let known = receiver
                .trackers
                .iter_mut()
                .find(|known| known.slot == tracker.slot);
            let address = if known
                .as_ref()
                .is_some_and(|known| known.address == raw_address)
            {
                std::borrow::Cow::Borrowed(raw_address)
            } else if let Some(address) = crate::domain::normalize_address(raw_address) {
                std::borrow::Cow::Owned(address)
            } else {
                continue;
            };
            if let Some(known) = known {
                if known.address != address.as_ref() {
                    *known = AutoQueryTracker {
                        slot: tracker.slot,
                        address: address.into_owned(),
                        attempted: false,
                        queried_at: None,
                        online: tracker.online,
                        since: now,
                    };
                } else {
                    if tracker.online && !known.online {
                        known.since = now;
                    }
                    known.online = tracker.online;
                }
            } else {
                receiver.trackers.push(AutoQueryTracker {
                    slot: tracker.slot,
                    address: address.into_owned(),
                    attempted: false,
                    queried_at: None,
                    online: tracker.online,
                    since: now,
                });
            }
        }
        for known in &mut receiver.trackers {
            if !trackers
                .iter()
                .any(|tracker| tracker.slot == known.slot && tracker.address.is_some())
            {
                known.online = false;
            }
        }
        // Missing/address-unknown observations and online=false are not proof
        // of a new episode: reservation clears the roster and traffic expires
        // after three seconds. Only positive identity changes reset attempts.
    }

    pub(super) fn full_requested(&mut self, groups: &[(ReceiverEndpoint, Vec<TrackerLink>, bool)]) {
        self.queued = false;
        self.discovery_since = None;
        for (endpoint, trackers, include_receiver) in groups {
            let Some(receiver) = self.receivers.iter_mut().find(|known| {
                known.endpoint.key == endpoint.key
                    && known.endpoint.path == endpoint.path
                    && known.endpoint.serial_number == endpoint.serial_number
            }) else {
                continue;
            };
            if *include_receiver {
                receiver.attempted = true;
                receiver.queried_at = None;
            }
            for tracker in &mut receiver.trackers {
                if trackers.iter().any(|live| {
                    live.slot == tracker.slot
                        && live
                            .address
                            .as_deref()
                            .and_then(crate::domain::normalize_address)
                            .as_deref()
                            == Some(tracker.address.as_str())
                }) {
                    tracker.attempted = true;
                    tracker.queried_at = None;
                }
            }
        }
    }

    pub(super) fn take_incremental<'a>(
        &mut self,
        receivers: impl Iterator<Item = &'a ReceiverEndpoint>,
        trackers: impl Iterator<Item = &'a TrackerLink> + Clone,
        blocked: bool,
        now: Instant,
    ) -> Vec<(ReceiverEndpoint, Vec<TrackerLink>, bool)> {
        if !self.active || self.queued || blocked {
            return Vec::new();
        }
        let mut groups = Vec::new();
        for endpoint in receivers {
            let Some(known) = self.receivers.iter_mut().find(|known| {
                known.endpoint.key == endpoint.key
                    && known.endpoint.path == endpoint.path
                    && known.endpoint.serial_number == endpoint.serial_number
            }) else {
                continue;
            };
            let include_receiver = !known.attempted
                && now.saturating_duration_since(known.since) >= Duration::from_secs(1);
            let mut requested = Vec::new();
            for tracker in trackers
                .clone()
                .filter(|tracker| tracker.receiver_key == endpoint.key && tracker.online)
            {
                let Some(candidate) = known.trackers.iter_mut().find(|candidate| {
                    !candidate.attempted
                        && candidate.slot == tracker.slot
                        && (tracker.address.as_deref() == Some(candidate.address.as_str())
                            || tracker
                                .address
                                .as_deref()
                                .and_then(crate::domain::normalize_address)
                                .as_deref()
                                == Some(candidate.address.as_str()))
                }) else {
                    continue;
                };
                if now.saturating_duration_since(candidate.since) >= Duration::from_secs(1) {
                    requested.push(TrackerLink {
                        receiver_key: tracker.receiver_key.clone(),
                        slot: tracker.slot,
                        address: Some(candidate.address.clone()),
                        online: true,
                    });
                    // Covers queued, in-flight, failed and successful attempts.
                    candidate.attempted = true;
                    candidate.queried_at = None;
                }
            }
            if include_receiver || !requested.is_empty() {
                known.attempted |= include_receiver;
                if include_receiver {
                    known.queried_at = None;
                }
                groups.push((known.endpoint.clone(), requested, include_receiver));
            }
        }
        groups
    }

    pub(super) fn mark_queried(&mut self, target: &esb_ota::Target, now: Instant) {
        let Some(receiver) = self.receivers.iter_mut().find(|known| {
            known.endpoint.key == target.receiver.key
                && known.endpoint.path == target.receiver.path
                && known.endpoint.serial_number == target.receiver.serial_number
        }) else {
            return;
        };
        if target.id == esb_ota::RECEIVER_ID {
            receiver.queried_at = Some(now);
            receiver.attempted = true;
        } else if let Some(tracker) = receiver.trackers.iter_mut().find(|known| {
            known.slot == target.id
                && target
                    .address
                    .as_deref()
                    .and_then(crate::domain::normalize_address)
                    .as_deref()
                    == Some(known.address.as_str())
        }) {
            tracker.queried_at = Some(now);
            tracker.attempted = true;
        }
    }

    pub(super) fn invalidate(&mut self, receiver_key: &str, id: u8) {
        let Some(receiver) = self
            .receivers
            .iter_mut()
            .find(|known| known.endpoint.key == receiver_key)
        else {
            return;
        };
        if id == esb_ota::RECEIVER_ID {
            receiver.queried_at = None;
            receiver.attempted = false;
        } else if let Some(tracker) = receiver.trackers.iter_mut().find(|known| known.slot == id) {
            tracker.queried_at = None;
            tracker.attempted = false;
        }
    }

    fn abandon_pending(&mut self, plan: &OtaQueryPlan) {
        for (receiver, trackers, include_receiver) in &plan.groups {
            if *include_receiver {
                self.invalidate(&receiver.key, esb_ota::RECEIVER_ID);
            }
            for tracker in trackers {
                self.invalidate(&receiver.key, tracker.slot);
            }
        }
    }

    fn accepts_result(&self, target: &esb_ota::Target, receivers: &[ReceiverEndpoint]) -> bool {
        let endpoint_matches = |receiver: &ReceiverEndpoint| {
            receiver.path == target.receiver.path
                && receiver.serial_number == target.receiver.serial_number
        };
        if receivers
            .iter()
            .any(|receiver| receiver.key == target.receiver.key && !endpoint_matches(receiver))
        {
            return false;
        }
        let Some(known) = self
            .receivers
            .iter()
            .find(|known| known.endpoint.key == target.receiver.key)
        else {
            return true;
        };
        if !endpoint_matches(&known.endpoint) {
            return false;
        }
        target.id == esb_ota::RECEIVER_ID
            || !known.trackers.iter().any(|tracker| {
                tracker.slot == target.id
                    && target
                        .address
                        .as_deref()
                        .and_then(crate::domain::normalize_address)
                        .as_deref()
                        != Some(tracker.address.as_str())
            })
    }
}

impl FlashState {
    // Returns whether a never-started reservation needs releasing. A running
    // bounded readonly query keeps its lease and completes even off-page.
    pub(super) fn enter_esb_page(&mut self, active: bool) -> bool {
        self.esb_auto_query.enter(active);
        if !active && matches!(&self.pending, Some(Action::QueryGroups(plan)) if plan.automatic) {
            if let Some(Action::QueryGroups(plan)) = self.pending.take() {
                self.esb_auto_query.abandon_pending(&plan);
            }
            self.invalidated_query_receivers.clear();
            self.activity_kind = ActivityKind::Preparation;
            return true;
        }
        false
    }

    pub(super) fn apply_current_queries(
        &mut self,
        mut query: OtaQueries,
        receivers: &[ReceiverEndpoint],
    ) {
        // Reservation makes the monitor temporarily unknown. Reject only a
        // positively observed replacement, retaining good results across that
        // gap but never resurrecting a superseded endpoint or RF slot binding.
        let invalidated = std::mem::take(&mut self.invalidated_query_receivers);
        query.targets.retain(|target| {
            (target.id == esb_ota::RECEIVER_ID || !invalidated.contains(&target.receiver.key))
                && self.esb_auto_query.accepts_result(target, receivers)
        });
        query.failures.retain(|(key, _)| {
            key.rsplit_once(':').is_none_or(|(receiver, id)| {
                !invalidated.contains(receiver) || id.parse::<u8>() == Ok(esb_ota::RECEIVER_ID)
            })
        });
        self.apply_queries(query);
    }

    pub(super) fn apply_queries(&mut self, query: OtaQueries) {
        self.preserve_query_card_layout = false;
        self.review = None;
        self.acknowledged = false;
        let preferred = query
            .targets
            .iter()
            .find(|target| {
                target.id != esb_ota::RECEIVER_ID
                    && self
                        .tracker_selection
                        .contains(&format!("tracker:{}:{}", target.receiver.key, target.id))
                    && !self.selected_targets.contains(&ota_target_key(target))
            })
            .map(ota_assignment_key);
        for target in query.targets {
            self.query_failures
                .remove(&format!("{}:{}", target.receiver.key, target.id));
            self.esb_auto_query.mark_queried(&target, Instant::now());
            let key = ota_target_key(&target);
            let previous = self
                .targets
                .iter()
                .position(|old| ota_target_key(old) == key);
            let same_identity = previous.is_some_and(|index| {
                let old = &self.targets[index];
                old.receiver.path == target.receiver.path
                    && old.receiver.serial_number == target.receiver.serial_number
                    && old
                        .address
                        .as_deref()
                        .and_then(crate::domain::normalize_address)
                        == target
                            .address
                            .as_deref()
                            .and_then(crate::domain::normalize_address)
            });
            if let Some(index) = previous {
                let old = &self.targets[index];
                if !same_identity
                    || old.info.board_target != target.info.board_target
                    || old.info.bootloader != target.info.bootloader
                    || old.info.protocol_version != target.info.protocol_version
                {
                    let old_group = ota_assignment_key(old);
                    self.assignments.remove(&old_group);
                    self.group_generations.remove(&old_group);
                }
            }
            let explicitly_selected = if target.id == esb_ota::RECEIVER_ID {
                same_identity && self.selected_targets.contains(&key)
            } else {
                self.tracker_selection
                    .contains(&format!("tracker:{}:{}", target.receiver.key, target.id))
                    && (previous.is_none() || same_identity)
            };
            if explicitly_selected && ota_supported(&target) {
                self.selected_targets.insert(key);
            } else {
                self.selected_targets.remove(&key);
            }
            if let Some(index) = previous {
                self.targets[index] = target;
            } else {
                self.targets.push(target);
            }
        }
        // A failed refresh does not erase the last successful FW_INFO. Live
        // identity/online validation remains mandatory at review and execution.
        self.query_failures.extend(query.failures);
        self.reconcile_groups(preferred);
    }
}

impl Workbench {
    pub(in crate::ui) fn sync_esb_page(&mut self) {
        let active = self.active_page == FeaturePage::Ota && self.flash_mode() == Mode::Esb;
        if self.flash.enter_esb_page(active) {
            self.release_flash_lease();
        }
    }

    pub(super) fn can_query_flash_group(
        &self,
        expected: &ReceiverEndpoint,
        tracker: Option<&TrackerLink>,
    ) -> bool {
        !self.shutting_down
            && !self.flash.busy()
            && self.flash_check_receiver_io(&expected.key).is_ok()
            && tracker.is_none_or(|tracker| {
                tracker.address.as_deref().is_some_and(|address| {
                    self.flash_check_scoped_tracker_io(&expected.key, address)
                        .is_ok()
                })
            })
            && self.inventory.receivers.iter().any(|current| {
                query_endpoint_matches(expected, current)
                    && self
                        .monitor
                        .observation(&current.key)
                        .is_some_and(|observation| {
                            tracker.is_none_or(|expected_tracker| {
                                expected_tracker.receiver_key == current.key
                                    && observation
                                        .trackers
                                        .iter()
                                        .any(|live| query_tracker_matches(expected_tracker, live))
                            })
                        })
            })
    }

    pub(super) fn query_flash_group(
        &mut self,
        expected: &ReceiverEndpoint,
        tracker: Option<&TrackerLink>,
    ) {
        if !self.can_query_flash_group(expected, tracker) {
            return;
        }
        let Some(current) = self
            .inventory
            .receivers
            .iter()
            .find(|current| query_endpoint_matches(expected, current))
        else {
            return;
        };
        let Some(observation) = self.monitor.observation(&current.key) else {
            return;
        };
        let Some(group) = scoped_query_group(expected, current, tracker, &observation.trackers)
        else {
            return;
        };
        self.flash
            .esb_auto_query
            .observe(current, &observation.trackers, Instant::now());
        merge_tracker_snapshot(
            self.flash
                .tracker_snapshot
                .entry(current.key.clone())
                .or_default(),
            &observation.trackers,
        );
        let mut plan = OtaQueryPlan {
            groups: vec![group],
            automatic: false,
            incremental: true,
        };
        retain_calibration_query_groups(&self.calibration.session, &mut plan.groups);
        self.flash.esb_auto_query.full_requested(&plan.groups);
        self.flash.review = None;
        self.flash.acknowledged = false;
        self.reserve_flash_action(Action::QueryGroups(plan));
    }

    pub(super) fn query_flash_targets(&mut self, automatic: bool) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        self.sync_flash_tracker_selection();
        self.flash.review = None;
        self.flash.acknowledged = false;
        let mut auto_query = std::mem::take(&mut self.flash.esb_auto_query);
        let plan = OtaQueryPlan {
            groups: if automatic {
                auto_query.take_incremental(
                    self.inventory
                        .receivers
                        .iter()
                        .filter(|receiver| self.flash_check_receiver_io(&receiver.key).is_ok()),
                    self.inventory
                        .receivers
                        .iter()
                        .filter_map(|receiver| self.monitor.observation(&receiver.key))
                        .flat_map(|observation| observation.trackers.iter())
                        .filter(|tracker| {
                            calibration_query_tracker_admitted(&self.calibration.session, tracker)
                        }),
                    false,
                    Instant::now(),
                )
            } else {
                available_query_groups(
                    self.inventory
                        .receivers
                        .iter()
                        .filter(|receiver| self.flash_check_receiver_io(&receiver.key).is_ok()),
                    self.inventory
                        .receivers
                        .iter()
                        .filter_map(|receiver| self.monitor.observation(&receiver.key))
                        .flat_map(|observation| observation.trackers.iter())
                        .filter(|tracker| {
                            calibration_query_tracker_admitted(&self.calibration.session, tracker)
                        }),
                )
            },
            automatic,
            incremental: automatic,
        };
        self.flash.esb_auto_query = auto_query;
        self.flash.esb_auto_query.full_requested(&plan.groups);
        for receiver in &self.inventory.receivers {
            let observation = self.monitor.observation(&receiver.key);
            if let Some(observation) = observation {
                let snapshot = self
                    .flash
                    .tracker_snapshot
                    .entry(receiver.key.clone())
                    .or_default();
                merge_tracker_snapshot(snapshot, &observation.trackers);
                merge_tracker_snapshot(snapshot, &[]);
            }
        }
        if plan.groups.is_empty() {
            if automatic {
                return;
            }
            self.flash.query_error = Some(
                self.inventory
                    .receivers
                    .iter()
                    .find_map(|receiver| self.flash_check_receiver_io(&receiver.key).err())
                    .unwrap_or_else(|| tr("firmware.query.connect_receiver")),
            );
            return;
        }
        self.reserve_flash_action(Action::QueryGroups(plan));
    }
}
