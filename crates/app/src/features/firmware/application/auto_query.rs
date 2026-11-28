use super::*;

const QUERY_DISCOVERY_SETTLE: Duration = Duration::from_secs(1);

fn query_cache_fresh(queried_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(queried_at) < QUERY_CACHE_TTL
}

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
            .and_then(crate::domain::address_digits)
            .is_some()
}

#[cfg(test)]
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
            .and_then(crate::domain::address_digits)
            == current
                .address
                .as_deref()
                .and_then(crate::domain::address_digits)
}

pub(super) fn calibration_query_tracker_admitted(
    calibration: &crate::features::calibration::engine::CalibrationSession,
    tracker: &TrackerLink,
) -> bool {
    tracker.address.as_deref().is_some_and(|address| {
        !calibration.blocks_scoped_tracker_io(&tracker.receiver_key, address)
    })
}

pub(super) fn retain_calibration_query_groups(
    calibration: &crate::features::calibration::engine::CalibrationSession,
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

impl AutoQueryTracker {
    fn matches(&self, tracker: &TrackerLink) -> bool {
        self.slot == tracker.slot && self.matches_address(tracker.address.as_deref())
    }

    fn matches_address(&self, address: Option<&str>) -> bool {
        address.is_some_and(|address| {
            address == self.address.as_str()
                || crate::domain::address_digits(address)
                    .is_some_and(|digits| digits.as_slice() == self.address.as_bytes())
        })
    }
}

impl EsbAutoQuery {
    pub(super) fn scoped_query_group(
        &self,
        expected: &ReceiverEndpoint,
        current: &ReceiverEndpoint,
        tracker: Option<&TrackerLink>,
        live: &[TrackerLink],
        now: Instant,
    ) -> Option<(ReceiverEndpoint, Vec<TrackerLink>, bool)> {
        if !query_endpoint_matches(expected, current) {
            return None;
        }
        if let Some(expected_tracker) = tracker {
            // A directed retry bypasses the cache but not identity or liveness.
            if expected_tracker.receiver_key != current.key {
                return None;
            }
            let link = live
                .iter()
                .find(|live| query_tracker_matches(expected_tracker, live))?;
            return Some((current.clone(), vec![link.clone()], false));
        }
        let cached = self
            .receivers
            .iter()
            .find(|known| query_endpoint_matches(&known.endpoint, current));
        let include_receiver = !cached
            .and_then(|known| known.queried_at)
            .is_some_and(|at| query_cache_fresh(at, now));
        let trackers: Vec<_> = live
            .iter()
            .filter(|link| {
                link.receiver_key == current.key
                    && query_tracker_eligible(link)
                    && !cached.is_some_and(|known| {
                        known.trackers.iter().any(|tracker| {
                            tracker
                                .queried_at
                                .is_some_and(|at| query_cache_fresh(at, now))
                                && tracker.matches(link)
                        })
                    })
            })
            .cloned()
            .collect();
        (include_receiver || !trackers.is_empty())
            .then(|| (current.clone(), trackers, include_receiver))
    }

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
        if blocked || now.saturating_duration_since(*since) < QUERY_DISCOVERY_SETTLE {
            return false;
        }
        self.queued = false;
        for receiver in &mut self.receivers {
            if receiver
                .queried_at
                .is_some_and(|at| !query_cache_fresh(at, now))
            {
                receiver.attempted = false;
            }
            for tracker in &mut receiver.trackers {
                if tracker
                    .queried_at
                    .is_some_and(|at| !query_cache_fresh(at, now))
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
            if !query_endpoint_matches(&known.endpoint, endpoint) {
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
        for tracker in trackers
            .iter()
            .filter(|link| link.receiver_key == endpoint.key)
        {
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
            if !trackers.iter().any(|tracker| {
                tracker.receiver_key == endpoint.key
                    && tracker.slot == known.slot
                    && tracker.address.is_some()
            }) {
                known.online = false;
            }
        }
        // Missing/address-unknown observations and online=false are not proof
        // of a new episode: reservation clears the roster and traffic expires
        // after three seconds. Positive identity changes reset attempts.
    }

    pub(super) fn full_requested(&mut self, groups: &[(ReceiverEndpoint, Vec<TrackerLink>, bool)]) {
        self.queued = false;
        self.discovery_since = None;
        for (endpoint, trackers, include_receiver) in groups {
            let Some(receiver) = self
                .receivers
                .iter_mut()
                .find(|known| query_endpoint_matches(&known.endpoint, endpoint))
            else {
                continue;
            };
            if *include_receiver {
                receiver.attempted = true;
                receiver.queried_at = None;
            }
            for tracker in &mut receiver.trackers {
                if trackers.iter().any(|live| tracker.matches(live)) {
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
        let settled = |since| now.saturating_duration_since(since) >= QUERY_DISCOVERY_SETTLE;
        let expired =
            |queried_at: Option<Instant>| queried_at.is_some_and(|at| !query_cache_fresh(at, now));
        let mut groups = Vec::new();
        for endpoint in receivers {
            let Some(known) = self
                .receivers
                .iter_mut()
                .find(|known| query_endpoint_matches(&known.endpoint, endpoint))
            else {
                continue;
            };
            let live = trackers
                .clone()
                .filter(|tracker| tracker.receiver_key == endpoint.key && tracker.online);
            let receiver_settled = settled(known.since);
            // Reentry rearms expired entries. While staying on the page, refresh
            // expired information only when new work already needs this handoff.
            if (known.attempted || !receiver_settled)
                && !live.clone().any(|tracker| {
                    known.trackers.iter().any(|candidate| {
                        !candidate.attempted
                            && settled(candidate.since)
                            && candidate.matches(tracker)
                    })
                })
            {
                continue;
            }
            let include_receiver =
                receiver_settled && (!known.attempted || expired(known.queried_at));
            let mut requested = Vec::new();
            for tracker in live {
                let Some(candidate) = known.trackers.iter_mut().find(|candidate| {
                    (!candidate.attempted || expired(candidate.queried_at))
                        && settled(candidate.since)
                        && candidate.matches(tracker)
                }) else {
                    continue;
                };
                requested.push(TrackerLink {
                    receiver_key: tracker.receiver_key.clone(),
                    slot: tracker.slot,
                    address: Some(candidate.address.clone()),
                    online: true,
                });
                // Covers queued, in-flight and failed attempts.
                candidate.attempted = true;
                candidate.queried_at = None;
            }
            if include_receiver {
                known.attempted = true;
                known.queried_at = None;
            }
            groups.push((known.endpoint.clone(), requested, include_receiver));
        }
        groups
    }

    pub(super) fn mark_queried(&mut self, target: &esb_ota::Target, now: Instant) {
        let Some(receiver) = self
            .receivers
            .iter_mut()
            .find(|known| query_endpoint_matches(&known.endpoint, &target.receiver))
        else {
            return;
        };
        if target.id == esb_ota::RECEIVER_ID {
            receiver.queried_at = Some(now);
            receiver.attempted = true;
        } else if let Some(tracker) = receiver.trackers.iter_mut().find(|known| {
            known.slot == target.id && known.matches_address(target.address.as_deref())
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
                tracker.slot == target.id && !tracker.matches_address(target.address.as_deref())
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
            self.finish_job_progress(None);
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
        let now = Instant::now();
        for target in query.targets {
            self.query_failures
                .remove(&format!("{}:{}", target.receiver.key, target.id));
            self.esb_auto_query.mark_queried(&target, now);
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
    pub(in crate::application) fn sync_esb_page(&mut self) {
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
                        .observation_for(current)
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
        let Some(observation) = self.monitor.observation_for(current) else {
            return;
        };
        let now = Instant::now();
        let Some(group) = self.flash.esb_auto_query.scoped_query_group(
            expected,
            current,
            tracker,
            &observation.trackers,
            now,
        ) else {
            return;
        };
        let mut groups = vec![group];
        retain_calibration_query_groups(&self.calibration.session, &mut groups);
        if groups.is_empty() {
            return;
        }
        self.flash
            .esb_auto_query
            .observe(current, &observation.trackers, now);
        merge_tracker_snapshot(
            self.flash
                .tracker_snapshot
                .entry(current.key.clone())
                .or_default(),
            &observation.trackers,
        );
        let plan = OtaQueryPlan {
            groups,
            automatic: false,
        };
        self.flash.esb_auto_query.full_requested(&plan.groups);
        self.flash.review = None;
        self.flash.acknowledged = false;
        self.reserve_flash_action(Action::QueryGroups(plan));
    }

    pub(super) fn poll_esb_queries(&mut self, now: Instant) -> bool {
        if !self.flash.esb_auto_query.active {
            return false;
        }
        let blocked = self.flash.busy() || self.flash.review.is_some();
        let mut auto_query = std::mem::take(&mut self.flash.esb_auto_query);
        let admitted_receivers = || {
            self.inventory
                .receivers
                .iter()
                .filter(|receiver| self.flash_check_receiver_io(&receiver.key).is_ok())
        };
        if auto_query.queued {
            let mut receivers = admitted_receivers().peekable();
            let available = receivers.peek().is_some()
                && receivers.all(|receiver| self.monitor.observation_for(receiver).is_some());
            auto_query.take_ready(available, blocked, now);
        }
        let groups = auto_query.take_incremental(
            admitted_receivers()
                .filter(|receiver| self.monitor.observation_for(receiver).is_some()),
            self.inventory
                .receivers
                .iter()
                .filter_map(|receiver| self.monitor.observation_for(receiver))
                .flat_map(|observation| observation.trackers.iter())
                .filter(|tracker| {
                    calibration_query_tracker_admitted(&self.calibration.session, tracker)
                }),
            blocked,
            now,
        );
        self.flash.esb_auto_query = auto_query;
        if groups.is_empty() {
            return false;
        }
        self.reserve_flash_action(Action::QueryGroups(OtaQueryPlan {
            groups,
            automatic: true,
        }));
        true
    }
}
