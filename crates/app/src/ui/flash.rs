use super::components::{clipped, context_button};
use super::devices::tracker_selection_key;
use super::{FeaturePage, Workbench};
use crate::{
    ble_ota, board_catalog,
    bundled_resources::Resource,
    esb_ota, firmware_source as source, firmware_update, uf2,
    update::{
        ActivationSubmission, Job, OtaPhase, OtaPreparation, OtaTargetProgress, Progress,
        ReceiverActivationReport, ReceiverAvailability, UpdateOutcome,
    },
};
use crate::{
    domain::{ReceiverEndpoint, SerialEndpoint, TrackerLink},
    i18n::tr,
    sessions::{SessionIdentityObservation, SessionUsbIdentity},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    tab::{Tab, TabBar},
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

mod auto_query;
mod confirmation;
mod image_page;
mod image_state;
mod images;
mod jobs;
mod planning;
mod progress;
mod remote;
mod selection;
mod serial_entry;
mod wired_targets;
mod wireless_targets;
mod workflow;

#[cfg(test)]
use auto_query::available_query_groups;
use auto_query::{query_failure, query_tracker_eligible};
use images::{
    configured_opendfu_options, firmware_filter_matches, firmware_page_matches, image_category,
    load_image, make_image, uf2_preset_matches,
};
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
    tr(if target.id == esb_ota::RECEIVER_ID {
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

// Successful FW_INFO is reused for 30 seconds on ESB page reentry only.
// Failures remain attempted until manual refresh or a positive identity change.
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
    mode: Mode,
    destination: ImageDestination,
    category: ImageCategory,
    channel: source::SourceChannel,
    generation: u64,
    version: Option<u64>,
}

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
    Images(ImageDestination, Vec<Image>),
    Versions(RemoteRequest, Vec<source::Version>),
    Latest(RemoteRequest, source::Version),
    Artifacts(RemoteRequest, Vec<source::Artifact>),
    RemoteImages(RemoteRequest, Vec<Image>),
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
    registry_key: Option<crate::device_registry::DeviceKey>,
    receiver_registry_key: Option<crate::device_registry::DeviceKey>,
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
    incremental: bool,
}

type QueryFailure = (String, (String, Result<(), String>));

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
    ota_defaults: HashMap<String, OtaDefault>,
    next_group_generation: u64,
    ble: Vec<ble_ota::BleDevice>,
    selected_ble: BTreeSet<String>,
    image: Option<Image>,
    images: Vec<Image>,
    images_destination: ImageDestination,
    remote_destination: ImageDestination,
    remote_view: bool,
    latest_version: Option<(source::SourceChannel, u64)>,
    channel: source::SourceChannel,
    versions: Vec<source::Version>,
    artifacts: Vec<source::Artifact>,
    selected_version: Option<u64>,
    remote_category: ImageCategory,
    remote_generation: u64,
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
    query_results_dismissed: bool,
    reveal_query_results: bool,
    operation_targets: Vec<ResultTarget>,
    query_failures: std::collections::BTreeMap<String, (String, Result<(), String>)>,
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
            next_group_generation: 0,
            ble: vec![],
            selected_ble: BTreeSet::new(),
            image: None,
            images: vec![],
            images_destination: ImageDestination::Global,
            remote_destination: ImageDestination::Global,
            remote_view: false,
            latest_version: None,
            channel: source::SourceChannel::Release,
            versions: vec![],
            artifacts: vec![],
            selected_version: None,
            remote_category: ImageCategory::Application,
            remote_generation: 0,
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
            query_results_dismissed: false,
            reveal_query_results: false,
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
        if self.querying() {
            self.query_cancelled = true;
            self.esb_auto_query.queued = false;
        }
        if let Some(job) = &self.job {
            job.cancel();
        }
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
            self.remote_generation += 1;
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

    fn preserves_query_layout(&self, key: &str) -> bool {
        self.querying_receiver(key) && self.reveal_query_results
    }
}

impl Workbench {
    fn flash_mode(&self) -> Mode {
        self.flash
            .effective_mode(self.active_page, self.preferences.advanced_features)
    }

    pub(super) fn set_feature_page(&mut self, page: FeaturePage, _cx: &mut Context<Self>) -> bool {
        if self.active_page != page {
            if self.app_update_navigation_locked() {
                return false;
            }
            self.flash.remote_generation += 1;
            self.active_page = page;
            self.flash.serial_dfu.warning = None;
            self.flash.review = None;
            self.flash.acknowledged = false;
            self.flash.auto = false;
            self.flash.auto_requested = false;
            self.sync_esb_page();
        }
        true
    }

    // GPUI builders are large values. Keep section and row temporaries in separate
    // non-inlined frames, returning erased elements before constructing the next section.
    #[inline(never)]
    pub(super) fn flash_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.flash.reconcile_ota_step(self.flash_mode());
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
mod guidance_tests;
#[cfg(test)]
mod tests;
