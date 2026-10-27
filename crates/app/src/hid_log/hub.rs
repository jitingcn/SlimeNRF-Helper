use super::model::{Record, RecordData, Source, TraceItem};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::VecDeque,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

const DEFAULT_CAPACITY: usize = 4096;
struct Queue {
    items: Mutex<VecDeque<TraceItem>>,
    capacity: usize,
    missed: AtomicU64,
    active: AtomicBool,
}
struct HubState {
    subscribers: Vec<Weak<Queue>>,
}
/// One serialization boundary for all native owners. Producers never wait for it.
pub(crate) struct Hub {
    state: Mutex<HubState>,
    publishers: RwLock<()>,
    active: AtomicUsize,
    sequence: AtomicU64,
    contention: AtomicU64,
    epoch: Instant,
    pub(crate) epoch_utc: String,
}
impl Hub {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HubState {
                subscribers: Vec::new(),
            }),
            publishers: RwLock::new(()),
            active: AtomicUsize::new(0),
            sequence: AtomicU64::new(1),
            contention: AtomicU64::new(0),
            epoch: Instant::now(),
            epoch_utc: chrono::Utc::now().to_rfc3339(),
        })
    }
    pub(crate) fn enabled(&self) -> bool {
        self.active.load(Ordering::Relaxed) != 0
    }
    pub(crate) fn subscribe(self: &Arc<Self>, capacity: usize) -> Subscription {
        assert!(capacity > 0);
        let queue = Arc::new(Queue {
            items: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            missed: AtomicU64::new(0),
            active: AtomicBool::new(true),
        });
        let mut state = self.state.lock();
        state.subscribers.retain(|s| s.strong_count() != 0);
        let contention_seen = self.contention.load(Ordering::Acquire);
        state.subscribers.push(Arc::downgrade(&queue));
        self.active.fetch_add(1, Ordering::Relaxed);
        Subscription {
            hub: self.clone(),
            queue,
            contention_seen,
            stopped: false,
        }
    }
    pub(crate) fn publish(&self, source: &Arc<Source>, data: RecordData) {
        self.publish_with(source, data, || {});
    }
    fn publish_with(&self, source: &Arc<Source>, data: RecordData, before_loss: impl FnOnce()) {
        if !self.enabled() {
            return;
        }
        let Some(_publisher) = self.publishers.try_read() else {
            // This increment is the linearization point of a rejected attempt.
            // It has no Record, and therefore reserves no record sequence.
            self.contention.fetch_add(1, Ordering::Release);
            return;
        };
        let Some(state) = self.state.try_lock() else {
            self.sequence.fetch_add(1, Ordering::Relaxed);
            before_loss();
            self.contention.fetch_add(1, Ordering::Release);
            return;
        };
        let record = Arc::new(Record {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            elapsed_us: self.epoch.elapsed().as_micros().min(u64::MAX as u128) as u64,
            source: source.clone(),
            data,
        });
        for subscriber in &state.subscribers {
            let Some(queue) = subscriber.upgrade() else {
                continue;
            };
            if !queue.active.load(Ordering::Acquire) {
                continue;
            }
            let Some(mut items) = queue.items.try_lock() else {
                queue.missed.fetch_add(1, Ordering::Release);
                continue;
            };
            if items.len() == queue.capacity {
                queue.missed.fetch_add(1, Ordering::Release);
                continue;
            }
            let missed = queue.missed.swap(0, Ordering::AcqRel);
            if missed != 0 {
                items.push_back(TraceItem::Gap { count: missed });
            }
            if items.len() < queue.capacity {
                items.push_back(TraceItem::Record(record.clone()));
            } else {
                queue.missed.fetch_add(1, Ordering::Release);
            }
        }
    }
}
pub(crate) fn global() -> &'static Arc<Hub> {
    static HUB: LazyLock<Arc<Hub>> = LazyLock::new(Hub::new);
    &HUB
}
pub fn subscribe() -> Subscription {
    global().subscribe(DEFAULT_CAPACITY)
}
/// Publishes through exactly the same bounded path as native I/O (also useful for host diagnostics).
pub fn publish(source: &Arc<Source>, data: RecordData) {
    global().publish(source, data);
}

pub struct Subscription {
    hub: Arc<Hub>,
    queue: Arc<Queue>,
    contention_seen: u64,
    stopped: bool,
}
impl Subscription {
    pub fn try_recv(&mut self) -> Option<TraceItem> {
        let mut items = self.queue.items.lock();
        if let Some(item) = items.pop_front() {
            return Some(item);
        }
        let missed = self.queue.missed.swap(0, Ordering::AcqRel);
        let total = if self.stopped {
            self.contention_seen
        } else {
            self.hub.contention.load(Ordering::Acquire)
        };
        let contention = total.saturating_sub(self.contention_seen);
        self.contention_seen = total;
        let count = missed.saturating_add(contention);
        (count != 0).then_some(TraceItem::Gap { count })
    }
    /// Detach first, then drain. No future producer can enter this subscription.
    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // Wait only on the stopping consumer, never on the native HID producer.
        // Admitted publishers keep their gate through final loss accounting.
        let _publishers = self.hub.publishers.write();
        let _state = self.hub.state.lock();
        self.queue.active.store(false, Ordering::Release);
        self.hub.active.fetch_sub(1, Ordering::Relaxed);
        let total = self.hub.contention.load(Ordering::Acquire);
        self.queue.missed.fetch_add(
            total.saturating_sub(self.contention_seen),
            Ordering::Release,
        );
        self.contention_seen = total;
        self.stopped = true;
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::{Hub, TraceItem};
    use crate::domain::ReceiverEndpoint;
    use crate::hid_log::{RecordData, Source};
    use std::sync::Arc;
    fn source() -> Arc<Source> {
        Arc::new(Source::main(&ReceiverEndpoint {
            key: "test".into(),
            path: vec![0, 255],
            label: "receiver".into(),
            serial_number: Some("serial".into()),
        }))
    }
    #[test]
    fn full_queue_reports_trailing_loss_and_stop_excludes_future_packets() {
        let hub = Hub::new();
        let source = source();
        let mut sub = hub.subscribe(2);
        for _ in 0..5 {
            hub.publish(&source, RecordData::Open);
        }
        sub.stop();
        hub.publish(&source, RecordData::Close);
        assert!(matches!(sub.try_recv(), Some(TraceItem::Record(_))));
        assert!(matches!(sub.try_recv(), Some(TraceItem::Record(_))));
        assert!(matches!(sub.try_recv(), Some(TraceItem::Gap { count: 3 })));
        assert!(sub.try_recv().is_none());
    }
    #[test]
    fn pending_gap_consumes_capacity_without_silently_losing_next_record() {
        let hub = Hub::new();
        let source = source();
        let mut sub = hub.subscribe(1);
        hub.publish(&source, RecordData::Open);
        hub.publish(&source, RecordData::Open);
        sub.try_recv();
        hub.publish(&source, RecordData::Close);
        assert!(matches!(sub.try_recv(), Some(TraceItem::Gap { count: 1 })));
        sub.stop();
        assert!(matches!(sub.try_recv(), Some(TraceItem::Gap { count: 1 })));
    }
    #[test]
    fn contention_is_explicit_and_independent_for_each_subscriber() {
        let hub = Hub::new();
        let source = source();
        let mut a = hub.subscribe(4);
        let mut b = hub.subscribe(4);
        {
            let _guard = hub.state.lock();
            hub.publish(&source, RecordData::Open);
        }
        assert!(matches!(a.try_recv(), Some(TraceItem::Gap { count: 1 })));
        b.stop();
        assert!(matches!(b.try_recv(), Some(TraceItem::Gap { count: 1 })));
        hub.publish(&source, RecordData::Close);
        assert!(matches!(a.try_recv(), Some(TraceItem::Record(_))));
        assert!(b.try_recv().is_none());
    }
    #[test]
    fn slow_subscriber_cannot_hold_back_independent_consumers() {
        let hub = Hub::new();
        let source = source();
        let mut slow = hub.subscribe(1);
        let mut fast = hub.subscribe(2);
        for expected in 1..=20 {
            hub.publish(&source, RecordData::Open);
            assert!(
                matches!(fast.try_recv(), Some(TraceItem::Record(record)) if record.sequence == expected)
            );
            assert!(fast.try_recv().is_none());
        }
        slow.stop();
        assert!(matches!(slow.try_recv(), Some(TraceItem::Record(record)) if record.sequence == 1));
        assert!(matches!(
            slow.try_recv(),
            Some(TraceItem::Gap { count: 19 })
        ));
        assert!(slow.try_recv().is_none());
    }
    #[test]
    fn inactive_bus_does_not_construct_or_sequence_records() {
        let hub = Hub::new();
        let source = source();
        hub.publish(&source, RecordData::Open);
        assert!(!hub.enabled());
        assert_eq!(hub.sequence.load(std::sync::atomic::Ordering::Relaxed), 1);
        let mut sub = hub.subscribe(2);
        hub.publish(&source, RecordData::Open);
        assert!(matches!(sub.try_recv(), Some(TraceItem::Record(record)) if record.sequence == 1));
        sub.stop();
        sub.stop();
        assert!(!hub.enabled());
    }
    #[test]
    fn stop_fences_an_admitted_publisher_paused_before_final_loss_accounting() {
        use std::{sync::mpsc, thread, time::Duration};
        let hub = Hub::new();
        let source = source();
        let mut sub = hub.subscribe(4);
        let state = hub.state.lock();
        let (paused_tx, paused_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let producer_hub = hub.clone();
        let producer = thread::spawn(move || {
            producer_hub.publish_with(&source, RecordData::Open, || {
                paused_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        paused_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (starting_tx, starting_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let stopper = thread::spawn(move || {
            starting_tx.send(()).unwrap();
            sub.stop();
            done_tx.send(sub).unwrap();
        });
        starting_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(state);
        // Only the paused producer's read gate can prevent the final stop snapshot now.
        let not_finished = matches!(
            done_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        let mut sub = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        stopper.join().unwrap();
        assert!(
            not_finished,
            "stop returned before the admitted loss was accounted"
        );
        assert!(matches!(sub.try_recv(), Some(TraceItem::Gap { count: 1 })));
        assert!(sub.try_recv().is_none());
    }
    #[test]
    fn publisher_does_not_wait_for_a_stopping_consumer_gate() {
        let hub = Hub::new();
        let source = source();
        let mut sub = hub.subscribe(4);
        {
            let _gate = hub.publishers.write();
            hub.publish(&source, RecordData::Open);
            assert!(matches!(sub.try_recv(), Some(TraceItem::Gap { count: 1 })));
        }
        sub.stop();
        assert!(sub.try_recv().is_none());
    }
}
