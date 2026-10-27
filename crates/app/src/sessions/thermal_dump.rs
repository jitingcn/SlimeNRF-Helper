use super::{SessionManager, port_key};
use parking_lot::Mutex;
use slimenrf_recording::thermal_dump::{ThermalDump, ThermalDumpDecoder, ThermalDumpError};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RESULT_CAPACITY: usize = 32;
const DUMP_TIMEOUT: Duration = Duration::from_secs(15);

/// Identity of one accepted dump, independent of its transport write receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThermalDumpRequest {
    pub port: String,
    pub generation: u64,
    pub request_id: u64,
}

#[derive(Clone, Debug)]
pub enum ThermalDumpOutcome {
    Complete(ThermalDump),
    Failed(ThermalDumpError),
    Cancelled,
    Disconnected,
    Handoff,
    WriteFailed(String),
    TimedOut,
    GenerationReplaced,
}

#[derive(Clone, Debug)]
pub struct ThermalDumpEvent {
    pub request: ThermalDumpRequest,
    pub outcome: ThermalDumpOutcome,
}

#[derive(Debug)]
pub(super) struct DumpRequests {
    active: BTreeMap<String, ThermalDumpRequest>,
    // An armed request without a clean terminal response cannot safely be
    // followed by another command: the protocol has no transaction nonce.
    // Keep the port quarantined until its owning session is closed.
    responses: BTreeMap<String, ThermalDumpRequest>,
    events: VecDeque<ThermalDumpEvent>,
    next_id: u64,
    pub(super) timeout: Duration,
}

impl Default for DumpRequests {
    fn default() -> Self {
        Self {
            active: BTreeMap::new(),
            events: VecDeque::new(),
            responses: BTreeMap::new(),
            next_id: 0,
            timeout: DUMP_TIMEOUT,
        }
    }
}

impl DumpRequests {
    pub(super) fn begin(
        &mut self,
        port: &str,
        generation: u64,
    ) -> Result<ThermalDumpRequest, String> {
        let key = port_key(port);
        if self.busy(port) {
            return Err(format!(
                "{port} has an active temperature dump; wait or reconnect first"
            ));
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "Cannot collect another temperature dump; restart the app".to_owned())?;
        let request = ThermalDumpRequest {
            port: key.clone(),
            generation,
            request_id: self.next_id,
        };
        self.active.insert(key, request.clone());
        Ok(request)
    }

    pub(super) fn is_active(&self, request: &ThermalDumpRequest) -> bool {
        self.active.get(&request.port) == Some(request)
    }

    pub(super) fn busy(&self, port: &str) -> bool {
        let key = port_key(port);
        self.active.contains_key(&key) || self.responses.contains_key(&key)
    }

    pub(super) fn needs_reconnect(&self, port: &str) -> bool {
        let key = port_key(port);
        self.responses.contains_key(&key) && !self.active.contains_key(&key)
    }

    pub(super) fn reject(&mut self, request: &ThermalDumpRequest) {
        if self.is_active(request) {
            self.active.remove(&request.port);
        }
    }

    pub(super) fn finish(
        &mut self,
        request: &ThermalDumpRequest,
        outcome: ThermalDumpOutcome,
    ) -> bool {
        if !self.is_active(request) {
            return false;
        }
        self.active.remove(&request.port);
        if self.events.len() == RESULT_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(ThermalDumpEvent {
            request: request.clone(),
            outcome,
        });
        true
    }

    pub(super) fn finish_port(&mut self, port: &str, outcome: ThermalDumpOutcome) {
        self.responses.remove(&port_key(port));
        if let Some(request) = self.active.get(&port_key(port)).cloned() {
            self.finish(&request, outcome);
        }
    }

    pub(super) fn finish_generation(
        &mut self,
        port: &str,
        generation: u64,
        outcome: ThermalDumpOutcome,
    ) {
        if self
            .responses
            .get(&port_key(port))
            .is_some_and(|request| request.generation == generation)
        {
            self.responses.remove(&port_key(port));
        }
        if let Some(request) = self.active.get(&port_key(port)).cloned()
            && request.generation == generation
        {
            self.finish(&request, outcome);
        }
    }
}

#[derive(Debug)]
pub(super) struct DumpCommand {
    pub(super) request: ThermalDumpRequest,
    pub(super) requests: Arc<Mutex<DumpRequests>>,
}

pub(super) struct DumpCollector {
    command: DumpCommand,
    decoder: ThermalDumpDecoder,
    deadline: Instant,
}

impl DumpCollector {
    /// Called by the owning worker immediately before writing this command.
    pub(super) fn arm(command: DumpCommand) -> Option<Self> {
        let mut requests = command.requests.lock();
        if !requests.is_active(&command.request) {
            return None;
        }
        let deadline = Instant::now() + requests.timeout;
        requests
            .responses
            .insert(command.request.port.clone(), command.request.clone());
        drop(requests);
        Some(Self {
            command,
            decoder: ThermalDumpDecoder::new_live_response(),
            deadline,
        })
    }

    /// False means cancelled/replaced/expired; never send another hardware command.
    pub(super) fn active(&self) -> bool {
        let mut requests = self.command.requests.lock();
        if Instant::now() >= self.deadline {
            requests.finish(&self.command.request, ThermalDumpOutcome::TimedOut);
        }
        requests.is_active(&self.command.request)
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        if self.active()
            && let Some(result) = self.decoder.feed(bytes)
        {
            let terminated = match &result {
                Ok(_) => true,
                Err(error) => matches!(error, ThermalDumpError::CountMismatch { .. }),
            };
            let outcome = match result {
                Ok(dump) => ThermalDumpOutcome::Complete(dump),
                Err(error) => ThermalDumpOutcome::Failed(error),
            };
            let mut requests = self.command.requests.lock();
            if terminated
                && requests.responses.get(&self.command.request.port) == Some(&self.command.request)
            {
                requests.responses.remove(&self.command.request.port);
            }
            requests.finish(&self.command.request, outcome);
        }
    }

    pub(super) fn finish(&self, outcome: ThermalDumpOutcome) {
        self.command
            .requests
            .lock()
            .finish(&self.command.request, outcome);
    }
}

impl SessionManager {
    /// Pending and receiving requests. Results are consumed separately from writes.
    pub fn active_thermal_dumps(&self) -> Vec<ThermalDumpRequest> {
        self.thermal_dumps.lock().active.values().cloned().collect()
    }

    /// An interrupted response has no nonce; reconnect before another command.
    pub fn thermal_dump_needs_reconnect(&self, port: &str) -> bool {
        self.thermal_dumps.lock().needs_reconnect(port)
    }

    /// Retains only the latest 32 terminal results, including while the page is hidden.
    pub fn drain_thermal_dump_events(&mut self) -> Vec<ThermalDumpEvent> {
        self.thermal_dumps.lock().events.drain(..).collect()
    }

    /// Local cancellation only: does not transmit Ctrl-C or close the serial session.
    pub fn cancel_thermal_dump(&mut self, request: &ThermalDumpRequest) -> bool {
        self.thermal_dumps
            .lock()
            .finish(request, ThermalDumpOutcome::Cancelled)
    }
}
