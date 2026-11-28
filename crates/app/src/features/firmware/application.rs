use super::components::{clipped, context_button};
use super::devices::tracker_selection_key;
use super::{FeaturePage, Workbench};
use crate::{
    domain::{ReceiverEndpoint, SerialEndpoint, TrackerLink},
    i18n::tr,
    sessions::{SessionIdentityObservation, SessionUsbIdentity},
};
use crate::{
    features::firmware::{
        ble_ota, board_catalog, bundled_resources::Resource, esb_ota, executor as firmware_update,
        source, uf2,
    },
    update::{
        ActivationSubmission, Job, OtaPhase, OtaPreparation, OtaTargetProgress, Progress,
        ReceiverActivationReport, ReceiverAvailability, UpdateOutcome,
    },
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    tooltip::Tooltip,
};
use gpui_kit::{prelude::*, *};
use slimenrf_firmware_image::{
    image::{self as firmware, Uf2ImagePurpose},
    secure_dfu,
};
use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[path = "application/auto_query.rs"]
mod auto_query;
#[path = "application/confirmation.rs"]
mod confirmation;
#[path = "application/image_page.rs"]
mod image_page;
#[path = "application/image_state.rs"]
mod image_state;
#[path = "application/images.rs"]
mod images;
#[path = "application/jobs.rs"]
mod jobs;
#[path = "application/ota_batch.rs"]
mod ota_batch;
#[path = "application/planning.rs"]
mod planning;
#[path = "application/progress.rs"]
mod progress;
#[path = "application/remote.rs"]
mod remote;
#[path = "application/selection.rs"]
mod selection;
#[path = "application/serial_entry.rs"]
mod serial_entry;
#[path = "application/wired_targets.rs"]
mod wired_targets;
#[path = "application/wireless_targets.rs"]
mod wireless_targets;
#[path = "application/workflow.rs"]
mod workflow;

#[cfg(test)]
use auto_query::available_query_groups;
use auto_query::{query_failure, query_tracker_eligible};
use images::{
    configured_opendfu_options, firmware_filter_matches, firmware_page_matches, load_image,
    make_image, uf2_preset_matches,
};
use ota_batch::{OtaBatchPrepared, OtaBatchResult, OtaGroupEpoch};
use planning::{ota_role, ota_supported, ota_update_guidance, target_label, validate_ota_image};
#[cfg(test)]
use planning::{plan_ota_groups, validate_live_ota_targets};
use selection::{
    clear_known_offline_tracker_selection, merge_tracker_snapshot, ota_tracker_activity_tps,
    selected_tracker_is_known_offline,
};
use workflow::{OtaPresentation, OtaStep, TrackerSection, tracker_section};

fn firmware_variant_label(variant: &str) -> std::borrow::Cow<'_, str> {
    match variant {
        "Standard" => tr("firmware.variant.standard").into(),
        "DataCollect CDC" => tr("firmware.variant.datacollect_cdc").into(),
        "Bootloader" => tr("firmware.variant.bootloader").into(),
        _ => variant.into(),
    }
}

fn ota_role_label(target: &esb_ota::Target) -> String {
    ota_role_label_for_receiver(target.id == esb_ota::RECEIVER_ID)
}

fn ota_role_label_for_receiver(receiver: bool) -> String {
    tr(if receiver {
        "common.device_role.receiver"
    } else {
        "common.device_role.tracker"
    })
}

#[derive(Clone)]
pub(super) struct Image {
    name: String,
    bytes: Arc<[u8]>,
    provenance: String,
    resource: Option<Arc<Resource>>,
    digest: String,
    local: bool,
    parsed: Option<Arc<firmware::FirmwareImage>>,
    hints: Arc<[board_catalog::FirmwareHint]>,
    acquisition_policy: source::BootloaderNamePolicy,
    uf2_family: Option<u32>,
    uf2_application_error: Option<String>,
    approval: Option<firmware_update::CompatibilityApproval>,
    validation_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Uf2,
    OpenDfu,
    Esb,
    Ble,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ImageCategory {
    #[default]
    Application,
    Bootloader,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ConfirmationPolicy {
    #[default]
    Standard,
    Minimal,
}

impl ConfirmationPolicy {
    fn from_preference(minimal: bool) -> Self {
        if minimal {
            Self::Minimal
        } else {
            Self::Standard
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Compatibility {
    Matched,
    Unknown,
    Mismatch,
}

// Reuse successful FW_INFO for 30 seconds for the same receiver and RF slot binding.
// Failed attempts can be retried manually or after a positive identity change.
const QUERY_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct EsbAutoQuery {
    active: bool,
    queued: bool,
    discovery_since: Option<Instant>,
    receivers: Vec<AutoQueryReceiver>,
}

struct AutoQueryReceiver {
    endpoint: ReceiverEndpoint,
    attempted: bool,
    queried_at: Option<Instant>,
    since: Instant,
    trackers: Vec<AutoQueryTracker>,
}

struct AutoQueryTracker {
    slot: u8,
    address: String,
    attempted: bool,
    queried_at: Option<Instant>,
    online: bool,
    since: Instant,
}

#[derive(Clone)]
enum Action {
    Uf2(Vec<uf2::Uf2Volume>, Image, Uf2ImagePurpose),
    Reboot(Vec<uf2::Uf2Volume>),
    Enter(SerialEndpoint),
    OpenDfu(SerialEndpoint, Image, secure_dfu::Options),
    QueryGroups(OtaQueryPlan),
    EsbGroups(Vec<OtaPlanGroup>),
    Ble(Vec<ble_ota::BleDevice>, Image),
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum ImageDestination {
    Global,
    Ota { key: String, generation: u64 },
}

#[derive(Clone)]
pub(super) struct RemoteRequest {
    category: ImageCategory,
    channel: source::SourceChannel,
    name_policy: source::BootloaderNamePolicy,
    generation: u64,
    version: Option<u64>,
}

#[derive(Clone)]
pub(super) struct RemoteImageRequest {
    catalog: RemoteRequest,
    mode: Mode,
    destination: ImageDestination,
    generation: u64,
    target_revision: RemoteTargetRevision,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RemoteTargetRevision {
    Global,
    Uf2(u64),
    Ota { focus: u64, members: u64 },
}

type FirmwareFilterTarget = (String, Option<board_catalog::EsbOtaKind>);

#[derive(Default)]
struct OtaDefault {
    generation: u64,
    attempted: bool,
    explicit: bool,
    error: Option<String>,
}

pub(super) enum Payload {
    Catalog(Result<board_catalog::Catalog, String>, Vec<uf2::Uf2Volume>),
    Volumes(Vec<uf2::Uf2Volume>),
    OtaBatch(OtaBatchPrepared),
    Images(ImageDestination, Vec<Image>),
    Versions(RemoteRequest, Vec<source::Version>),
    Latest(RemoteRequest, source::Version, Vec<source::Artifact>),
    Artifacts(RemoteRequest, Vec<source::Artifact>),
    RemoteImages(RemoteImageRequest, Vec<Image>),
    Ble(Vec<ble_ota::BleDevice>),
    Update(firmware_update::UpdateCompletion),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResultTarget {
    key: String,
    label: String,
    identity: String,
    receiver: Option<(String, String)>,
    id: Option<u8>,
    registry_key: Option<crate::features::device_registry::model::DeviceKey>,
    receiver_registry_key: Option<crate::features::device_registry::model::DeviceKey>,
}

#[derive(Default, Debug, PartialEq, Eq)]
struct OtaCounts {
    completed: usize,
    activation_unconfirmed: usize,
    failed: usize,
    cancelled: usize,
    queued: usize,
    active: usize,
}

#[derive(Clone)]
pub(super) struct OtaPlanGroup {
    pub(super) targets: Vec<esb_ota::Target>,
    pub(super) image: Image,
}

pub(super) struct OtaAssignmentView {
    pub(super) key: String,
    pub(super) label: String,
    board_target: String,
    pub(super) bootloader: String,
    pub(super) image_name: Option<String>,
}

#[derive(Clone)]
struct OtaQueryPlan {
    groups: Vec<(ReceiverEndpoint, Vec<TrackerLink>, bool)>,
    automatic: bool,
}

type QueryFailure = (String, String);

pub(super) struct OtaQueries {
    targets: Vec<esb_ota::Target>,
    failures: Vec<QueryFailure>,
}

enum Lease {
    Serial {
        token: crate::sessions::SerialReservation,
        port: String,
    },
    Hid(crate::discovery::ReceiverReservation),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    Preparation,
    Query,
    Operation,
}

#[derive(Default)]
struct OperationPins {
    addresses: BTreeSet<String>,
    ports: BTreeSet<String>,
    usb: BTreeSet<SessionUsbIdentity>,
}

impl OperationPins {
    fn freeze<'a>(
        addresses: BTreeSet<String>,
        receivers: impl IntoIterator<Item = &'a ReceiverEndpoint>,
        manager: &crate::sessions::SessionManager,
        endpoints: &[SerialEndpoint],
    ) -> Self {
        let mut pins = Self {
            addresses,
            ..Self::default()
        };
        for receiver in receivers {
            if let Some(usb) = receiver
                .serial_number
                .as_deref()
                .and_then(|serial| SessionUsbIdentity::new(serial, 0x1209, 0x7690))
            {
                pins.usb.insert(usb);
            }
        }
        for observation in manager.identity_observations() {
            pins.observe_identity(&observation);
        }
        for endpoint in endpoints {
            pins.observe_endpoint(endpoint);
        }
        pins
    }

    fn observe_changes(
        &mut self,
        changes: &crate::sessions::SessionChanges,
        endpoints: &[SerialEndpoint],
    ) {
        for observation in &changes.identities {
            self.observe_identity(observation);
        }
        for endpoint in endpoints {
            self.observe_endpoint(endpoint);
        }
    }

    fn observe_identity(&mut self, observation: &SessionIdentityObservation) {
        if !self.addresses.contains(&observation.address) {
            return;
        }
        if !self.ports.contains(&observation.port) {
            self.ports.insert(observation.port.clone());
        }
        if let Some(usb) = &observation.usb
            && !self.usb.contains(usb)
        {
            self.usb.insert(usb.clone());
        }
    }

    fn observe_endpoint(&mut self, endpoint: &SerialEndpoint) {
        if self.usb.iter().any(|usb| usb.matches_endpoint(endpoint))
            && !self.ports.contains(&endpoint.port_name)
        {
            self.ports.insert(endpoint.port_name.clone());
        }
    }

    fn blocks_port(&self, port: &str, endpoint: Option<&SerialEndpoint>) -> bool {
        self.ports
            .iter()
            .any(|reserved| crate::serial_port::same_port_name(reserved, port))
            || endpoint
                .is_some_and(|endpoint| self.usb.iter().any(|usb| usb.matches_endpoint(endpoint)))
    }
}

pub(super) struct FlashState {
    pub(super) job: Option<Job<Payload>>,
    volume_scan: Option<Job<Payload>>,
    // Read-only defaults never own foreground activity or device reservations.
    default_job: Option<(ImageDestination, Job<Image>)>,
    refresh_after_job: bool,
    pending: Option<Action>,
    review: Option<Action>,
    confirmation_policy: ConfirmationPolicy,
    review_policy: ConfirmationPolicy,
    review_compatibility: Compatibility,
    risk_acknowledged: bool,
    leases: Vec<Lease>,
    operation_pins: OperationPins,
    mode: Mode,
    ota_step: OtaStep,
    ota_presentation: OtaPresentation,
    catalog: Option<board_catalog::Catalog>,
    catalog_loaded: bool,
    catalog_error: Option<String>,
    volumes: Vec<uf2::Uf2Volume>,
    selected_volumes: BTreeSet<String>,
    selected_port: Option<String>,
    serial_dfu: serial_entry::SerialDfuState,
    esb_auto_query: EsbAutoQuery,
    tracker_snapshot: HashMap<String, Vec<TrackerLink>>,
    invalidated_query_receivers: BTreeSet<String>,
    category: ImageCategory,
    pub(super) targets: Vec<esb_ota::Target>,
    selected_targets: BTreeSet<String>,
    tracker_selection: BTreeSet<String>,
    assignments: HashMap<String, Image>,
    active_group: Option<String>,
    group_generations: HashMap<String, u64>,
    group_epochs: HashMap<String, OtaGroupEpoch>,
    ota_defaults: HashMap<String, OtaDefault>,
    source_revision: u64,
    ota_batch_result: Option<OtaBatchResult>,
    ota_batch_inflight: bool,
    ota_batch_cancelled: bool,
    next_group_generation: u64,
    ble: Vec<ble_ota::BleDevice>,
    selected_ble: BTreeSet<String>,
    image: Option<Image>,
    images: Vec<Image>,
    images_destination: ImageDestination,
    remote_view: bool,
    latest_version: Option<(source::SourceChannel, u64)>,
    channel: source::SourceChannel,
    versions: Vec<source::Version>,
    artifacts: Option<Vec<source::Artifact>>,
    selected_version: Option<u64>,
    remote_category: ImageCategory,
    remote_generation: u64,
    remote_download_generation: u64,
    ota_focus_generation: u64,
    uf2_selection_generation: u64,
    application_channel: source::SourceChannel,
    matched_only: bool,
    variant_filter: Option<String>,
    error: Option<String>,
    error_origin: Mode,
    activity_origin: Mode,
    operation_origin: Mode,
    query_error: Option<String>,
    query_cancelled: bool,
    results: Vec<(ResultTarget, UpdateOutcome)>,
    receiver_result_joined_at: Option<Instant>,
    operation_results_dismissed: bool,
    query_tracker_slots: HashMap<String, u16>,
    operation_targets: Vec<ResultTarget>,
    query_failures: std::collections::BTreeMap<String, String>,
    activity_kind: ActivityKind,
    result_success: &'static str,
    result_notes: HashMap<String, String>,
    acknowledged: bool,
    auto: bool,
    auto_requested: bool,
    last_progress: Option<Progress>,
    auto_seen: BTreeSet<String>,
    next_scan: Instant,
    chooser: Option<Task<()>>,
    // Frozen at job/chooser start; navigating to Settings must not hide BLE work.
    ble_activity: bool,
}

impl Default for FlashState {
    fn default() -> Self {
        Self {
            job: None,
            volume_scan: None,
            default_job: None,
            refresh_after_job: false,
            pending: None,
            review: None,
            confirmation_policy: ConfirmationPolicy::Standard,
            review_policy: ConfirmationPolicy::Standard,
            review_compatibility: Compatibility::Unknown,
            risk_acknowledged: false,
            leases: Vec::new(),
            operation_pins: OperationPins::default(),
            mode: Mode::Uf2,
            ota_step: OtaStep::Devices,
            ota_presentation: OtaPresentation::Configuring,
            catalog: None,
            catalog_loaded: false,
            catalog_error: None,
            volumes: vec![],
            selected_volumes: BTreeSet::new(),
            selected_port: None,
            serial_dfu: serial_entry::SerialDfuState::default(),
            esb_auto_query: EsbAutoQuery::default(),
            tracker_snapshot: HashMap::new(),
            invalidated_query_receivers: BTreeSet::new(),
            category: ImageCategory::Application,
            targets: vec![],
            selected_targets: BTreeSet::new(),
            tracker_selection: BTreeSet::new(),
            assignments: HashMap::new(),
            active_group: None,
            group_generations: HashMap::new(),
            ota_defaults: HashMap::new(),
            group_epochs: HashMap::new(),
            next_group_generation: 0,
            source_revision: 0,
            ota_batch_result: None,
            ota_batch_inflight: false,
            ota_batch_cancelled: false,
            ble: vec![],
            selected_ble: BTreeSet::new(),
            image: None,
            images: vec![],
            images_destination: ImageDestination::Global,
            remote_view: false,
            latest_version: None,
            channel: source::SourceChannel::Release,
            versions: vec![],
            artifacts: None,
            selected_version: None,
            remote_category: ImageCategory::Application,
            remote_generation: 0,
            remote_download_generation: 0,
            ota_focus_generation: 0,
            uf2_selection_generation: 0,
            application_channel: source::SourceChannel::Release,
            matched_only: true,
            variant_filter: None,
            error: None,
            error_origin: Mode::Uf2,
            activity_origin: Mode::Uf2,
            operation_origin: Mode::Uf2,
            query_error: None,
            query_cancelled: false,
            results: vec![],
            receiver_result_joined_at: None,
            operation_results_dismissed: false,
            query_tracker_slots: HashMap::new(),
            operation_targets: vec![],
            query_failures: Default::default(),
            activity_kind: ActivityKind::Preparation,
            result_success: "firmware.result.operation_completed",
            result_notes: HashMap::new(),
            acknowledged: false,
            auto: false,
            auto_requested: false,
            last_progress: None,
            auto_seen: BTreeSet::new(),
            next_scan: Instant::now(),
            chooser: None,
            ble_activity: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OtaUpdateGuidance {
    OpenDfu,
    Uf2,
    DocumentedMethod,
}

pub(super) fn ota_target_key(target: &esb_ota::Target) -> String {
    format!("{}:{}", target.receiver.key, target.id)
}

pub(super) fn ota_assignment_key(target: &esb_ota::Target) -> String {
    format!(
        "{}:{}:{}",
        target.info.board_target,
        ota_role(target),
        target.info.bootloader == "mcuboot"
    )
}

#[inline(never)]
fn flash_target_text(text: String) -> AnyElement {
    div().text_sm().child(text).into_any_element()
}

fn flash_panel(cx: &App) -> Div {
    div()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
}

pub(super) fn select_receiver_trackers(
    selected: &mut BTreeSet<String>,
    receiver: &str,
    trackers: &[TrackerLink],
    online: bool,
) {
    if online {
        selected.extend(
            trackers
                .iter()
                .filter(|tracker| tracker.receiver_key == receiver && tracker.online)
                .map(tracker_selection_key),
        );
    } else {
        selected.retain(|key| {
            key.strip_prefix("tracker:")
                .and_then(|key| key.rsplit_once(':'))
                .is_none_or(|(owner, _)| owner != receiver)
        });
    }
}

impl Action {
    fn mode(&self) -> Mode {
        match self {
            Self::Uf2(..) | Self::Reboot(..) => Mode::Uf2,
            Self::OpenDfu(..) | Self::Enter(..) => Mode::OpenDfu,
            Self::QueryGroups(..) | Self::EsbGroups(..) => Mode::Esb,
            Self::Ble(..) => Mode::Ble,
        }
    }

    fn approve(&mut self, approval: firmware_update::CompatibilityApproval) {
        match self {
            Self::Uf2(_, image, _) | Self::OpenDfu(_, image, _) | Self::Ble(_, image) => {
                image.approval = Some(approval);
            }
            Self::EsbGroups(groups) => {
                for group in groups {
                    group.image.approval = Some(approval);
                }
            }
            Self::Reboot(_) | Self::Enter(_) | Self::QueryGroups(_) => {}
        }
    }

    fn is_firmware_update(&self) -> bool {
        matches!(
            self,
            Self::Uf2(..) | Self::OpenDfu(..) | Self::EsbGroups(..) | Self::Ble(..)
        )
    }

    fn same_update(&self, other: &Self) -> bool {
        fn same_image(left: &Image, right: &Image) -> bool {
            Arc::ptr_eq(&left.bytes, &right.bytes)
                && Arc::ptr_eq(&left.hints, &right.hints)
                && (match (&left.parsed, &right.parsed) {
                    (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                    (None, None) => true,
                    _ => false,
                })
                && left.name == right.name
                && left.digest == right.digest
                && left.provenance == right.provenance
                && left.resource == right.resource
                && left.local == right.local
                && left.acquisition_policy == right.acquisition_policy
                && left.uf2_family == right.uf2_family
                && left.uf2_application_error == right.uf2_application_error
                && left.approval == right.approval
                && left.validation_error == right.validation_error
        }

        match (self, other) {
            (
                Self::Uf2(left, left_image, left_purpose),
                Self::Uf2(right, right_image, right_purpose),
            ) => {
                left_purpose == right_purpose
                    && same_image(left_image, right_image)
                    && left.len() == right.len()
                    && left.iter().zip(right).all(|(left, right)| {
                        left.identity == right.identity
                            && left.mount == right.mount
                            && left.label == right.label
                            && left.board_id == right.board_id
                    })
            }
            (
                Self::OpenDfu(left, left_image, left_options),
                Self::OpenDfu(right, right_image, right_options),
            ) => {
                same_image(left_image, right_image)
                    && left.port_name == right.port_name
                    && left.kind == right.kind
                    && left.serial_number == right.serial_number
                    && left.vid == right.vid
                    && left.pid == right.pid
                    && left.usb_interface == right.usb_interface
                    && left.product == right.product
                    && left.manufacturer == right.manufacturer
                    && left_options.hw_version == right_options.hw_version
                    && left_options.app_version == right_options.app_version
                    && left_options.sd_req == right_options.sd_req
                    && left_options.app_base == right_options.app_base
            }
            (Self::EsbGroups(left), Self::EsbGroups(right)) => {
                left.len() == right.len()
                    && left.iter().zip(right).all(|(left, right)| {
                        same_image(&left.image, &right.image)
                            && left.targets.len() == right.targets.len()
                            && left
                                .targets
                                .iter()
                                .zip(&right.targets)
                                .all(|(left, right)| {
                                    left.id == right.id
                                        && left.address == right.address
                                        && left.info == right.info
                                        && left.receiver.key == right.receiver.key
                                        && left.receiver.path == right.receiver.path
                                        && left.receiver.serial_number
                                            == right.receiver.serial_number
                                })
                    })
            }
            _ => false,
        }
    }
}

impl ActivityKind {
    fn for_action(action: &Action) -> Self {
        if matches!(action, Action::QueryGroups(_)) {
            Self::Query
        } else {
            Self::Operation
        }
    }
}

impl FlashState {
    pub(in crate::application) fn from_confirmation_preference(minimal: bool) -> Self {
        let mut state = Self::default();
        state.set_confirmation_policy(minimal);
        state
    }

    pub(in crate::application) fn set_confirmation_policy(&mut self, minimal: bool) {
        let policy = ConfirmationPolicy::from_preference(minimal);
        if self.confirmation_policy != policy {
            self.confirmation_policy = policy;
            self.review = None;
            self.acknowledged = false;
            self.risk_acknowledged = false;
            self.auto_requested = false;
            self.clear_remote();
        }
    }

    fn set_error(&mut self, mode: Mode, error: String) {
        self.error_origin = mode;
        self.error = Some(error);
    }

    fn set_activity_error(&mut self, error: String) {
        if self.querying() {
            self.query_error = Some(error);
        } else {
            self.set_error(self.activity_origin, error);
        }
    }

    fn cancel_activity(&mut self) {
        self.auto = false;
        if self.ota_batch_inflight {
            self.ota_batch_cancelled = true;
        }
        if self.querying() {
            self.query_cancelled = true;
            self.esb_auto_query.queued = false;
        }
        if let Some(job) = &self.job {
            job.cancel();
        }
    }

    pub(super) fn navigation_changed(&mut self) {
        self.remote_download_generation += 1;
        self.serial_dfu.warning = None;
        self.review = None;
        self.acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
    }

    pub(super) fn blocks_receiver(&self, key: &str) -> bool {
        self.leases
            .iter()
            .any(|lease| matches!(lease, Lease::Hid(token) if token.endpoint().key == key))
    }

    pub(super) fn blocks_port(&self, port: &str, endpoint: Option<&SerialEndpoint>) -> bool {
        self.operation_pins.blocks_port(port, endpoint)
            || self.leases.iter().any(|lease| {
                matches!(lease, Lease::Serial { port: reserved, .. } if crate::serial_port::same_port_name(reserved, port))
            })
    }

    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
            || self.pending.is_some()
            || self.chooser.is_some()
            || self.serial_dfu.busy()
    }

    pub(super) fn advanced_features_locked(&self) -> bool {
        (self.ble_activity && (self.job.is_some() || self.chooser.is_some()))
            || matches!(self.pending, Some(Action::Ble(..)))
    }

    pub(super) fn prepare_advanced_features(&mut self, enabled: bool) -> bool {
        if !enabled {
            if self.advanced_features_locked() {
                return false;
            }
            if self.mode == Mode::Ble || matches!(self.review, Some(Action::Ble(..))) {
                self.transition_mode(Mode::Esb);
            }
        }
        true
    }

    fn transition_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            self.remote_download_generation += 1;
        }
        self.mode = mode;
        self.serial_dfu.warning = None;
        self.review = None;
        self.acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
    }

    fn effective_mode(&self, page: FeaturePage, advanced_features: bool) -> Mode {
        let mode = if self.mode == Mode::Ble && !advanced_features {
            Mode::Esb
        } else {
            self.mode
        };
        match page {
            FeaturePage::Uf2 if mode != Mode::OpenDfu => Mode::Uf2,
            FeaturePage::Ota if mode != Mode::Ble => Mode::Esb,
            _ => mode,
        }
    }

    fn querying(&self) -> bool {
        self.activity_kind == ActivityKind::Query
            || matches!(self.pending, Some(Action::QueryGroups(_)))
    }

    fn querying_receiver(&self, key: &str) -> bool {
        if let Some(Action::QueryGroups(plan)) = &self.pending {
            return plan
                .groups
                .iter()
                .any(|(receiver, _, _)| receiver.key == key);
        }
        self.querying()
            && self
                .leases
                .iter()
                .any(|lease| matches!(lease, Lease::Hid(token) if token.endpoint().key == key))
    }

    fn querying_tracker(&self, tracker: &TrackerLink) -> bool {
        self.querying_receiver(&tracker.receiver_key)
            && !self
                .invalidated_query_receivers
                .contains(&tracker.receiver_key)
            && self
                .query_tracker_slots
                .get(&tracker.receiver_key)
                .is_some_and(|slots| {
                    1_u16
                        .checked_shl(tracker.slot.into())
                        .is_some_and(|slot| *slots & slot != 0)
                })
    }
}

impl Workbench {
    fn flash_mode(&self) -> Mode {
        self.flash
            .effective_mode(self.active_page, self.preferences.advanced_features)
    }

    pub(super) fn observe_firmware_workflow(cx: &mut Context<Self>) -> Subscription {
        cx.observe_self(|this, cx| {
            if !this.shutting_down && this.reconcile_flash_workflow() {
                cx.notify();
            }
        })
    }

    pub(super) fn reconcile_flash_workflow(&mut self) -> bool {
        self.flash.reconcile_ota_step(self.flash_mode())
    }

    // GPUI builders are large values. Keep section and row temporaries in separate
    // non-inlined frames, returning erased elements before constructing the next section.
    #[inline(never)]
    pub(super) fn flash_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("firmware-page")
            .relative()
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .child(self.flash_mode_tabs(cx))
            .child(self.flash_columns(cx))
            .when(self.flash.floating_activity(self.flash_mode()), |page| {
                page.child(self.flash_activity_toast(cx))
            })
            .into_any_element()
    }

    #[inline(never)]
    fn flash_targets(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.flash_mode() {
            Mode::Uf2 => self.flash_uf2_targets(cx),
            Mode::OpenDfu => self.flash_dfu_targets(cx),
            Mode::Esb => self.flash_esb_targets(cx),
            Mode::Ble => self.flash_ble_targets(cx),
        }
    }

    #[inline(never)]
    fn flash_configured_button(
        &self,
        id: ElementId,
        label: SharedString,
        path: &Resource,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if path.is_intentionally_omitted() {
            return div().into_any_element();
        }
        let name = path.filename().unwrap_or_default();
        let hints = self
            .flash
            .catalog
            .as_ref()
            .map(|catalog| catalog.match_filename(name))
            .unwrap_or_default();
        if !self.candidate_allowed(name, &hints, false) {
            return div().into_any_element();
        }
        let filename: SharedString = name.to_owned().into();
        let accessible_label: SharedString = format!("{name} · {label}").into();
        let path = path.to_owned();
        Button::new(id)
            .small()
            .h_auto()
            .py_2()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .tooltip(accessible_label.clone())
            .accessibility_label(accessible_label)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_left()
                    .whitespace_normal()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_sm()
                            .child(filename),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    ),
            )
            .disabled(self.flash.busy())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.load_configured_image(path.clone());
                cx.notify();
            }))
            .into_any_element()
    }
}

impl OtaUpdateGuidance {
    fn wired_mode(self) -> Option<Mode> {
        match self {
            Self::OpenDfu => Some(Mode::OpenDfu),
            Self::Uf2 => Some(Mode::Uf2),
            Self::DocumentedMethod => None,
        }
    }
}

#[cfg(test)]
#[path = "application/guidance_tests.rs"]
mod guidance_tests;
#[cfg(test)]
#[path = "application/tests.rs"]
mod tests;
