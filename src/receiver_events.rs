use std::time::{Duration, Instant};

const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const RENEW_INTERVAL: Duration = Duration::from_secs(5);
const OPCODE: u8 = 224;
const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventWatchStatus {
    Disabled,
    Checking,
    Active,
    Paused,
    Unsupported(String),
    Error(String),
}

fn is_failure(status: &EventWatchStatus) -> bool {
    matches!(
        status,
        EventWatchStatus::Unsupported(_) | EventWatchStatus::Error(_)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Query = 0,
    Subscribe = 1,
    Renew = 2,
    Stop = 3,
}

#[derive(Clone, Copy)]
struct Pending {
    action: Action,
    sequence: u8,
    deadline: Instant,
}

pub(crate) struct Command {
    pub bytes: [u8; 65],
}

/// Only advances between ordinary main-interface reads. No ACK wait owns the reader.
pub(crate) struct Watch {
    generation: u64,
    desired: bool,
    status: EventWatchStatus,
    sequence: u8,
    pending: Option<Pending>,
    need_subscribe: bool,
    lease_possible: bool,
    need_stop: bool,
    next_renew: Option<Instant>,
    filter: u8,
    mask: u8,
}

impl Watch {
    pub fn new(
        enabled: bool,
        generation: u64,
        status: EventWatchStatus,
        filter: u8,
        mask: u8,
    ) -> Self {
        Self {
            generation,
            desired: enabled,
            status,
            sequence: 0,
            pending: None,
            need_subscribe: false,
            lease_possible: false,
            need_stop: false,
            next_renew: None,
            filter,
            mask,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn status(&self) -> &EventWatchStatus {
        &self.status
    }
    pub fn cleanup_pending(&self) -> bool {
        self.lease_possible
    }

    pub fn sync(&mut self, enabled: bool, generation: u64, cancelled: bool) {
        let desired = enabled && !cancelled;
        if self.generation == generation && self.desired == desired {
            return;
        }
        self.generation = generation;
        self.desired = desired;
        self.status = if desired {
            EventWatchStatus::Checking
        } else {
            EventWatchStatus::Disabled
        };
        self.need_subscribe = false;
        self.next_renew = None;
        self.need_stop = self.lease_possible;
        if !self
            .pending
            .is_some_and(|pending| pending.action == Action::Stop)
        {
            self.pending = None;
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<Command> {
        if let Some(pending) = self.pending {
            if now < pending.deadline {
                return None;
            }
            self.pending = None;
            if pending.action == Action::Stop {
                self.lease_possible = false;
                self.need_stop = false;
            }
            self.fail(EventWatchStatus::Error(format!(
                "No event subscription ACK for action {}; check receiver firmware and enable events again",
                pending.action as u8
            )));
        }
        if self.need_stop {
            return Some(self.command(Action::Stop, now));
        }
        if !self.desired || is_failure(&self.status) {
            return None;
        }
        if self.need_subscribe {
            self.need_subscribe = false;
            return Some(self.command(Action::Subscribe, now));
        }
        if let Some(next) = self.next_renew {
            return (now >= next).then(|| self.command(Action::Renew, now));
        }
        Some(self.command(Action::Query, now))
    }

    fn command(&mut self, action: Action, now: Instant) -> Command {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let mut bytes = [0; 65];
        bytes[1] = 254;
        bytes[2] = self.sequence;
        bytes[3] = OPCODE;
        bytes[5] = VERSION;
        bytes[6] = action as u8;
        bytes[7] = self.filter;
        bytes[8] = if matches!(action, Action::Subscribe | Action::Renew) {
            self.mask
        } else {
            0
        };
        self.pending = Some(Pending {
            action,
            sequence: self.sequence,
            deadline: now + ACK_TIMEOUT,
        });
        // Even a timed-out/short subscription write may have reached the receiver.
        if action == Action::Subscribe {
            self.lease_possible = true;
        }
        Command { bytes }
    }

    pub fn write_failed(&mut self, error: String) {
        if self
            .pending
            .take()
            .is_some_and(|pending| pending.action == Action::Stop)
        {
            self.lease_possible = false;
            self.need_stop = false;
        }
        self.fail(EventWatchStatus::Error(error));
    }

    fn fail(&mut self, status: EventWatchStatus) {
        if !is_failure(&self.status) {
            self.status = status;
        }
        self.need_subscribe = false;
        self.next_renew = None;
        self.need_stop = self.lease_possible;
    }

    pub fn ingest(&mut self, report: &[u8], now: Instant) {
        if report.len() != 64 || self.pending.is_none() {
            return;
        }
        for record in report.as_chunks::<16>().0 {
            let Some(pending) = self.pending else {
                continue;
            };
            if now >= pending.deadline {
                continue;
            }
            if record[0] != 251 || record[1] != pending.sequence || record[2] != OPCODE {
                continue;
            }
            self.pending = None;
            if pending.action == Action::Stop {
                // Bounded cleanup ends even on a negative ACK; the log preserves it.
                self.lease_possible = false;
                self.need_stop = false;
            }
            if record[3] != 0 {
                let status = if record[3] == 5 {
                    EventWatchStatus::Unsupported(
                        "Receiver firmware does not support tracker events".into(),
                    )
                } else {
                    EventWatchStatus::Error(format!(
                        "Event subscription rejected (status {}); enable events to retry",
                        record[3]
                    ))
                };
                self.fail(status);
                continue;
            }
            if record[4] != VERSION || (pending.action == Action::Query && record[5] & 1 == 0) {
                self.fail(EventWatchStatus::Unsupported(
                    "Receiver firmware does not support event protocol v1".into(),
                ));
                continue;
            }
            match pending.action {
                Action::Query => self.need_subscribe = true,
                Action::Subscribe | Action::Renew => {
                    if record[14] != self.filter
                        || record[15] & 1 == 0
                        || (record[15] >> 1) & 31 != self.mask
                    {
                        self.fail(EventWatchStatus::Error("Receiver did not activate the requested event subscription; enable events to retry".into()));
                    } else {
                        self.status = EventWatchStatus::Active;
                        self.next_renew = Some(now + RENEW_INTERVAL);
                    }
                }
                Action::Stop => {
                    if record[15] & 1 != 0 {
                        self.fail(EventWatchStatus::Error(
                            "Receiver event subscription remains active after stop".into(),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ACK_TIMEOUT, EventWatchStatus, RENEW_INTERVAL, Watch};
    use std::time::Instant;

    fn watch(filter: u8, mask: u8) -> Watch {
        Watch::new(true, 1, EventWatchStatus::Checking, filter, mask)
    }
    fn ack(sequence: u8, filter: u8, mask: u8) -> [u8; 64] {
        let mut report = [0; 64];
        report[48..54].copy_from_slice(&[251, sequence, 224, 0, 1, 1]);
        report[62] = filter;
        report[63] = 1 | mask << 1;
        report
    }
    fn active(now: Instant) -> Watch {
        let mut driver = watch(255, 31);
        assert_eq!(
            &driver.tick(now).unwrap().bytes[..9],
            &[0, 254, 1, 224, 0, 1, 0, 255, 0]
        );
        driver.ingest(&ack(1, 255, 31), now);
        assert_eq!(
            &driver.tick(now).unwrap().bytes[..9],
            &[0, 254, 2, 224, 0, 1, 1, 255, 31]
        );
        driver.ingest(&ack(2, 255, 31), now);
        assert_eq!(driver.status(), &EventWatchStatus::Active);
        driver
    }
    #[test]
    fn filters_are_exact_for_passive_and_exclusive_consumers() {
        let now = Instant::now();
        active(now);
        let mut driver = watch(7, 1);
        assert_eq!(
            &driver.tick(now).unwrap().bytes[..9],
            &[0, 254, 1, 224, 0, 1, 0, 7, 0]
        );
        driver.ingest(&ack(1, 255, 31), now);
        assert_eq!(
            &driver.tick(now).unwrap().bytes[..9],
            &[0, 254, 2, 224, 0, 1, 1, 7, 1]
        );
        driver.ingest(&ack(2, 7, 3), now);
        assert!(matches!(driver.status(), EventWatchStatus::Error(_)));
        assert_eq!(driver.tick(now).unwrap().bytes[6], 3);
    }
    #[test]
    fn ack_correlation_and_all_record_positions() {
        let now = Instant::now();
        for index in 0..4 {
            let mut driver = watch(7, 1);
            driver.tick(now).unwrap();
            driver.ingest(&ack(2, 7, 1), now);
            assert!(driver.tick(now).is_none());
            let mut input = [0; 64];
            input[index * 16..index * 16 + 16].copy_from_slice(&ack(1, 7, 1)[48..64]);
            driver.ingest(&input, now);
            assert_eq!(driver.tick(now).unwrap().bytes[6], 1);
        }
    }
    #[test]
    fn renew_and_stop_are_bounded_and_do_not_retry_writes() {
        let now = Instant::now();
        let mut driver = active(now);
        assert!(
            driver
                .tick(now + RENEW_INTERVAL - std::time::Duration::from_millis(1))
                .is_none()
        );
        assert_eq!(driver.tick(now + RENEW_INTERVAL).unwrap().bytes[6], 2);
        driver.write_failed("partial renewal".into());
        assert_eq!(driver.tick(now + RENEW_INTERVAL).unwrap().bytes[6], 3);
        assert!(driver.cleanup_pending());
        assert!(driver.tick(now + RENEW_INTERVAL + ACK_TIMEOUT).is_none());
        assert!(!driver.cleanup_pending());
        assert_eq!(
            driver.status(),
            &EventWatchStatus::Error("partial renewal".into())
        );
    }
    #[test]
    fn subscription_partial_write_and_cancel_always_attempt_stop() {
        let now = Instant::now();
        let mut driver = watch(7, 1);
        driver.tick(now).unwrap();
        driver.ingest(&ack(1, 7, 1), now);
        driver.tick(now).unwrap();
        driver.write_failed("partial subscribe".into());
        assert!(driver.cleanup_pending());
        driver.sync(false, 2, true);
        assert_eq!(driver.tick(now).unwrap().bytes[6], 3);
        let mut response = ack(3, 7, 0);
        response[63] = 0;
        driver.ingest(&response, now);
        assert!(!driver.cleanup_pending());
        assert!(driver.tick(now + RENEW_INTERVAL).is_none());
    }
    #[test]
    fn unsupported_or_expired_ack_requires_explicit_new_intent() {
        let now = Instant::now();
        for (offset, value) in [(52, 2), (53, 0), (51, 5)] {
            let mut driver = watch(255, 31);
            driver.tick(now).unwrap();
            let mut response = ack(1, 255, 31);
            response[offset] = value;
            driver.ingest(&response, now);
            assert!(matches!(driver.status(), EventWatchStatus::Unsupported(_)));
            assert!(driver.tick(now + ACK_TIMEOUT * 100).is_none());
            driver.sync(true, 2, false);
            assert_eq!(driver.tick(now).unwrap().bytes[6], 0);
        }
        let mut driver = watch(7, 1);
        driver.tick(now).unwrap();
        driver.ingest(&ack(1, 7, 1), now + ACK_TIMEOUT);
        assert!(driver.tick(now + ACK_TIMEOUT).is_none());
        assert!(matches!(driver.status(), EventWatchStatus::Error(_)));
    }
    #[test]
    fn enabling_during_stop_waits_for_the_stop_ack() {
        let now = Instant::now();
        let mut driver = active(now);
        driver.sync(false, 2, false);
        let stop = driver.tick(now).unwrap();
        driver.sync(true, 3, false);
        assert!(driver.tick(now).is_none());
        let mut response = ack(stop.bytes[2], 255, 0);
        response[63] = 0;
        driver.ingest(&response, now);
        assert_eq!(driver.tick(now).unwrap().bytes[6], 0);
    }
    #[test]
    fn replacement_keeps_failure_until_explicit_enable_and_stop_write_failure_releases() {
        let now = Instant::now();
        let mut driver = Watch::new(
            true,
            1,
            EventWatchStatus::Unsupported("old firmware".into()),
            255,
            31,
        );
        assert!(driver.tick(now).is_none());
        driver.sync(true, 2, false);
        assert_eq!(driver.tick(now).unwrap().bytes[6], 0);
        let mut driver = active(now);
        driver.sync(false, 2, false);
        assert_eq!(driver.tick(now).unwrap().bytes[6], 3);
        driver.write_failed("stop write failed".into());
        assert!(!driver.cleanup_pending());
        assert_eq!(
            driver.status(),
            &EventWatchStatus::Error("stop write failed".into())
        );
        assert!(driver.tick(now + RENEW_INTERVAL * 10).is_none());
    }
}
