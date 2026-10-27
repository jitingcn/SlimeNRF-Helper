mod observations;

use observations::RegistrySources;

use super::{
    FeaturePage, Workbench, command_panel::CommandFocus, components::clipped,
    terminal::serial_display_name,
};
use crate::{
    device_registry::{
        DeviceKey, DeviceObservation, DeviceRecord, DeviceRegistry, RegistryValidationError,
    },
    domain::{DeviceRole, Inventory, ReceiverEndpoint, SerialEndpoint, TrackerLink},
    i18n::tr,
    sessions::ConnectionState,
    settings::device_registry::{
        self as storage, RegistryEdit, RegistryPersistenceError, RegistryStore, RegistryStoreError,
    },
};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Enter, Escape, Input, InputState},
};
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, EntityInputHandler as _, FocusHandle, FontWeight,
    ScrollHandle, Task, Window, div, prelude::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Default)]
pub(super) struct RegistryUiState {
    store: Option<RegistryStore>,
    snapshot: Option<Arc<DeviceRegistry>>,
    observed: BTreeMap<DeviceKey, DeviceObservation>,
    current: BTreeSet<DeviceKey>,
    ambiguous: BTreeSet<DeviceKey>,
    carriers: BTreeMap<DeviceKey, BTreeSet<Carrier>>,
    sessions: BTreeMap<String, (u64, DeviceKey)>,
    fingerprint: Option<u64>,
    pending_observe: bool,
    forgotten: BTreeSet<DeviceKey>,
    absent: BTreeSet<DeviceKey>,
    open: bool,
    focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    editor: Option<NicknameEditor>,
    next_editor_id: u64,
    saving: Option<NicknameEditorId>,
    forget: Option<DeviceKey>,
    error: Option<String>,
    job: Option<Task<()>>,
    io_join: Option<Shared<Task<()>>>,
    output: Option<Arc<parking_lot::Mutex<Option<StoreResult>>>>,
}

impl RegistryUiState {
    #[cfg(test)]
    pub(super) fn set_test_snapshot(&mut self, snapshot: DeviceRegistry) {
        self.snapshot = Some(Arc::new(snapshot));
    }

    fn next_observation_edit(&mut self) -> RegistryEdit {
        let absent = std::mem::take(&mut self.absent);
        self.pending_observe = self.observed.keys().any(|key| absent.contains(key));
        RegistryEdit::Observe {
            observations: self
                .observed
                .values()
                .filter(|observation| !absent.contains(&observation.key))
                .cloned()
                .collect(),
            absent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Carrier {
    Receiver(DeviceKey),
    Serial {
        serial: Option<String>,
        vid: Option<u16>,
        pid: Option<u16>,
    },
}

impl Carrier {
    fn present(&self, inventory: &Inventory) -> bool {
        match self {
            Self::Receiver(key) => inventory.receivers.iter().any(|receiver| {
                receiver
                    .serial_number
                    .as_deref()
                    .and_then(DeviceKey::receiver_serial)
                    == Some(*key)
            }),
            Self::Serial { serial, vid, pid } => inventory.ports.iter().any(|endpoint| {
                serial
                    .as_deref()
                    .zip(endpoint.serial_number.as_deref())
                    .is_some_and(|(known, current)| {
                        known.trim().eq_ignore_ascii_case(current.trim())
                    })
                    && endpoint.vid == *vid
                    && endpoint.pid == *pid
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NicknameEditorId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
enum NicknameEditorScope {
    Registry,
    CurrentDevice(CommandFocus),
}

struct NicknameEditSession {
    id: NicknameEditorId,
    key: DeviceKey,
    scope: NicknameEditorScope,
    error: Option<String>,
}

impl NicknameEditSession {
    fn matches_current_device(&self, focus: Option<&CommandFocus>, key: Option<DeviceKey>) -> bool {
        matches!(&self.scope, NicknameEditorScope::CurrentDevice(expected)
            if focus == Some(expected) && key == Some(self.key))
    }

    fn complete(&mut self, id: NicknameEditorId, result: &Result<(), RegistryStoreError>) -> bool {
        if self.id != id {
            return false;
        }
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = matches!(error, RegistryStoreError::Validation(_))
                    .then(|| registry_store_error(error));
                false
            }
        }
    }
}

struct NicknameEditor {
    session: NicknameEditSession,
    input: Entity<InputState>,
}

enum StoreResult {
    Loaded(Result<RegistryStore, String>),
    Changed {
        store: RegistryStore,
        result: Result<(), RegistryStoreError>,
        mutation: Option<RegistryMutation>,
        absent: BTreeSet<DeviceKey>,
    },
}

enum RegistryMutation {
    Nickname(NicknameEditorId),
    Forget(DeviceKey),
    Remember(DeviceKey),
}

impl Workbench {
    pub(super) fn initialize_device_registry(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.registry.io_join.is_some() || self.registry.store.is_some() {
            return;
        }
        self.registry.error = None;
        self.start_registry_job(
            || {
                StoreResult::Loaded(storage::default_path().and_then(|path| {
                    RegistryStore::load(path).map_err(|error| registry_store_error(&error))
                }))
            },
            cx,
        );
    }

    fn start_registry_job(
        &mut self,
        operation: impl FnOnce() -> StoreResult + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let output = Arc::new(parking_lot::Mutex::new(None));
        let worker_output = output.clone();
        let join = cx
            .background_executor()
            .spawn(async move {
                *worker_output.lock() = Some(operation());
            })
            .shared();
        self.registry.output = Some(output.clone());
        self.registry.io_join = Some(join.clone());
        self.registry.job = Some(cx.spawn(async move |this, cx| {
            join.await;
            let result = output.lock().take();
            let _ = this.update(cx, |this, cx| {
                this.registry.io_join = None;
                this.registry.output = None;
                this.registry.job = None;
                this.registry.saving = None;
                match result.expect("retained registry operation returns its owned store") {
                    StoreResult::Loaded(Ok(store)) => {
                        this.registry.snapshot = Some(store.snapshot());
                        this.registry.store = Some(store);
                        this.registry.error = None;
                    }
                    StoreResult::Loaded(Err(error)) => this.registry.error = Some(error),
                    StoreResult::Changed {
                        store,
                        result,
                        mutation,
                        absent,
                    } => {
                        this.registry.snapshot = Some(store.snapshot());
                        if let Some(RegistryMutation::Nickname(id)) = &mutation
                            && this
                                .registry
                                .editor
                                .as_mut()
                                .is_some_and(|editor| editor.session.complete(*id, &result))
                        {
                            this.registry.editor = None;
                        }
                        match result {
                            Ok(()) => {
                                this.registry.error = None;
                                for key in absent {
                                    this.registry.forgotten.remove(&key);
                                }
                                if let Some(mutation) = mutation {
                                    match mutation {
                                        RegistryMutation::Forget(key) => {
                                            this.registry.forgotten.insert(key);
                                            if this.registry.forget == Some(key) {
                                                this.registry.forget = None;
                                            }
                                        }
                                        RegistryMutation::Remember(key) => {
                                            this.registry.forgotten.remove(&key);
                                        }
                                        RegistryMutation::Nickname(_) => {}
                                    }
                                    this.publish_temperature_devices(cx);
                                }
                            }
                            Err(RegistryStoreError::Validation(_))
                                if matches!(mutation, Some(RegistryMutation::Nickname(_))) => {}
                            Err(error) => {
                                this.registry.error = Some(registry_store_error(&error));
                                if mutation.is_none() {
                                    this.registry.pending_observe = true;
                                    this.registry.absent.extend(absent);
                                }
                            }
                        }
                        this.registry.store = Some(store);
                    }
                }
                this.prune_registry_carriers();
                if this.registry.error.is_none() {
                    this.flush_registry_observations(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn registry_edit(&mut self, edit: RegistryEdit, cx: &mut Context<Self>) {
        if self.shutting_down || self.registry.io_join.is_some() {
            return;
        }
        let Some(mut store) = self.registry.store.take() else {
            return;
        };
        let mutation = match &edit {
            RegistryEdit::Rename { key, .. } => {
                let Some(editor) = self.registry.editor.as_ref().filter(|editor| {
                    editor.session.key == *key && self.nickname_editor_is_current(&editor.session)
                }) else {
                    self.registry.store = Some(store);
                    return;
                };
                self.registry.saving = Some(editor.session.id);
                RegistryMutation::Nickname(editor.session.id)
            }
            RegistryEdit::Forget(key) => {
                self.registry.absent.remove(key);
                RegistryMutation::Forget(*key)
            }
            RegistryEdit::Remember(observation) => {
                self.registry.absent.remove(&observation.key);
                RegistryMutation::Remember(observation.key)
            }
            RegistryEdit::Observe { .. } => {
                unreachable!("automatic observations use their coalesced path")
            }
        };
        self.start_registry_job(
            move || {
                let result = store.apply(edit);
                StoreResult::Changed {
                    store,
                    result,
                    mutation: Some(mutation),
                    absent: BTreeSet::new(),
                }
            },
            cx,
        );
    }

    fn flush_registry_observations(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || !self.registry.pending_observe || self.registry.io_join.is_some() {
            return;
        }
        let Some(mut store) = self.registry.store.take() else {
            return;
        };
        let edit = self.registry.next_observation_edit();
        let absent = match &edit {
            RegistryEdit::Observe { absent, .. } => absent.clone(),
            _ => unreachable!("the observation builder returns an observation edit"),
        };
        self.start_registry_job(
            move || {
                let result = store.apply(edit);
                StoreResult::Changed {
                    store,
                    result,
                    mutation: None,
                    absent,
                }
            },
            cx,
        );
    }

    pub(super) fn registry_shutdown(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        self.registry.job = None;
        let join = self.registry.io_join.take();
        let output = self.registry.output.take();
        let mut store = self.registry.store.take();
        let pending = self
            .registry
            .pending_observe
            .then(|| self.registry.next_observation_edit());
        let followup = self
            .registry
            .pending_observe
            .then(|| self.registry.next_observation_edit());
        if join.is_none() && pending.is_none() {
            return None;
        }
        Some(cx.background_executor().spawn(drain_registry_store(
            async move {
                if let Some(join) = join {
                    join.await;
                    store = output
                        .and_then(|output| output.lock().take())
                        .and_then(|result| match result {
                            StoreResult::Loaded(result) => result.ok(),
                            StoreResult::Changed { store, .. } => Some(store),
                        });
                }
                store
            },
            pending,
            followup,
        )))
    }

    fn prune_registry_carriers(&mut self) {
        self.registry.carriers.retain(|key, _| {
            self.registry.observed.contains_key(key)
                || self.registry.forgotten.contains(key)
                || self
                    .registry
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.get(key).is_some())
        });
    }

    pub(super) fn registry_inventory_arrived(&mut self) {
        self.registry.absent.extend(absent_carrier_keys(
            &self.inventory,
            &self.registry.carriers,
            &self.registry.forgotten,
        ));
    }

    pub(super) fn sync_registry_observations(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let sources = RegistrySources {
            inventory: &self.inventory,
            manager: &self.manager,
            monitor: &self.monitor,
            targets: &self.flash.targets,
            inventory_failed: self.inventory_error.is_some(),
        };
        if self.registry.reconcile_sources(sources) {
            self.flush_registry_observations(cx);
            self.prune_registry_carriers();
            cx.notify();
        }
    }

    pub(super) fn calibration_device_label(
        &self,
        target: &crate::calibration::Target,
        default: String,
    ) -> String {
        match target {
            crate::calibration::Target::Direct { address, .. } => {
                self.address_label(address.as_deref(), default)
            }
            crate::calibration::Target::Wireless {
                receiver, address, ..
            } => self.address_label(Some(address), self.frozen_receiver_label(receiver, default)),
        }
    }

    pub(super) fn hid_source_label(
        &self,
        source: &crate::hid_log::Source,
        default: impl Into<String>,
    ) -> String {
        let key = (source.vendor_id == 0x1209 && source.product_id == 0x7690)
            .then(|| {
                source
                    .serial_number
                    .as_deref()
                    .and_then(DeviceKey::receiver_serial)
            })
            .flatten()
            .filter(|key| !self.registry.ambiguous.contains(key));
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn serial_generation_label(
        &self,
        port: &str,
        generation: u64,
        default: String,
    ) -> String {
        let key = self
            .registry
            .sessions
            .get(port)
            .filter(|(bound_generation, _)| {
                *bound_generation == generation
                    && self.manager.session(port).is_some_and(|session| {
                        session.generation == generation
                            && (session.state != ConnectionState::Connected
                                || self.registry_current_endpoint(port, generation).is_some())
                    })
            })
            .map(|(_, key)| key);
        self.registry_label(key, default)
    }

    pub(super) fn serial_session_label(&self, port: &str, default: impl Into<String>) -> String {
        let default = default.into();
        match self.manager.session(port) {
            Some(session) => self.serial_generation_label(port, session.generation, default),
            None => default,
        }
    }

    pub(super) fn registry_label(
        &self,
        key: Option<&DeviceKey>,
        default: impl Into<String>,
    ) -> String {
        let default = default.into();
        match key.and_then(|key| {
            self.registry
                .snapshot
                .as_ref()?
                .get(key)?
                .nickname
                .as_deref()
        }) {
            Some(nickname) => format!("{nickname} · {default}"),
            None => default,
        }
    }

    fn registry_current_endpoint(&self, port: &str, generation: u64) -> Option<&SerialEndpoint> {
        let endpoint = self
            .inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)?;
        self.manager.connected_identity_for(endpoint, generation)?;
        Some(endpoint)
    }

    pub(super) fn registry_is_open(&self) -> bool {
        self.registry.open
    }

    pub(super) fn frozen_receiver_label(
        &self,
        receiver: &ReceiverEndpoint,
        default: impl Into<String>,
    ) -> String {
        let key = receiver
            .serial_number
            .as_deref()
            .and_then(DeviceKey::receiver_serial)
            .filter(|key| !self.registry.ambiguous.contains(key));
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn address_label(
        &self,
        address: Option<&str>,
        default: impl Into<String>,
    ) -> String {
        let key = address
            .and_then(DeviceKey::tracker_address)
            .filter(|key| !self.registry.ambiguous.contains(key));
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn receiver_label(
        &self,
        receiver: &ReceiverEndpoint,
        default: impl Into<String>,
    ) -> String {
        let key = DeviceKey::receiver(receiver, &self.inventory)
            .filter(|key| self.registry.observed.contains_key(key));
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn tracker_label(
        &self,
        tracker: &TrackerLink,
        default: impl Into<String>,
    ) -> String {
        let key =
            DeviceKey::tracker(tracker).filter(|key| self.registry.observed.contains_key(key));
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn frozen_serial_endpoint_label(
        &self,
        endpoint: &SerialEndpoint,
        default: impl Into<String>,
    ) -> String {
        let key = frozen_serial_endpoint_key(endpoint);
        self.registry_label(key.as_ref(), default)
    }

    pub(super) fn serial_endpoint_label(
        &self,
        endpoint: &SerialEndpoint,
        default: impl Into<String>,
    ) -> String {
        let key = self.serial_endpoint_key(endpoint);
        self.registry_label(key.as_ref(), default)
    }

    fn serial_endpoint_key(&self, endpoint: &SerialEndpoint) -> Option<DeviceKey> {
        let current = self
            .inventory
            .ports
            .iter()
            .find(|current| current.port_name == endpoint.port_name)?;
        if current.kind != endpoint.kind
            || !super::command_panel::same_serial_endpoint(Some(current), Some(endpoint))
        {
            return None;
        }
        if let Some(session) = self.manager.session(&endpoint.port_name)
            && self
                .manager
                .connected_identity_for(endpoint, session.generation)
                .is_some()
            && let Some((generation, key)) = self.registry.sessions.get(&endpoint.port_name)
            && *generation == session.generation
        {
            return Some(*key);
        }
        self.inventory
            .receivers
            .iter()
            .find(|receiver| {
                crate::domain::receiver_serial_ports(receiver, &self.inventory).is_some_and(
                    |ports| {
                        [ports.console, ports.raw_data]
                            .into_iter()
                            .flatten()
                            .any(|port| port.port_name == endpoint.port_name)
                    },
                )
            })
            .and_then(|receiver| DeviceKey::receiver(receiver, &self.inventory))
            .filter(|key| self.registry.observed.contains_key(key))
    }

    pub(super) fn serial_device_label(&self, port: &str, default: impl Into<String>) -> String {
        let default = default.into();
        match self
            .inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)
        {
            Some(endpoint) => self.serial_endpoint_label(endpoint, default),
            None => default,
        }
    }

    pub(super) fn serial_reported_name(&self, endpoint: &SerialEndpoint) -> Option<&str> {
        let session = self.manager.session(&endpoint.port_name)?;
        self.manager
            .connected_identity_for(endpoint, session.generation)?
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
    }

    pub(super) fn serial_name(&self, port: &str) -> String {
        let default = self
            .manager
            .session(port)
            .and_then(|session| session.identity.name.as_deref())
            .or_else(|| {
                self.inventory
                    .ports
                    .iter()
                    .find(|endpoint| endpoint.port_name == port)
                    .map(|endpoint| endpoint.label.as_str())
            })
            .map(|label| serial_display_name(port, label))
            .filter(|label| !label.is_empty())
            .unwrap_or(port);
        self.serial_session_label(port, default)
    }

    pub(super) fn open_device_registry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_feature_page(FeaturePage::Settings);
        self.registry.open = true;
        let focus = self
            .registry
            .focus
            .get_or_insert_with(|| cx.focus_handle().tab_stop(false));
        focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn current_nickname_key(&self, focus: &CommandFocus) -> Option<DeviceKey> {
        let key = match focus {
            CommandFocus::Receiver(key) => self
                .inventory
                .receivers
                .iter()
                .find(|receiver| &receiver.key == key)
                .and_then(|receiver| DeviceKey::receiver(receiver, &self.inventory)),
            CommandFocus::Tracker {
                receiver,
                slot,
                address,
            } => self
                .visible_trackers()
                .find(|tracker| {
                    tracker.receiver_key == *receiver
                        && tracker.slot == *slot
                        && tracker.address == *address
                })
                .and_then(DeviceKey::tracker),
            CommandFocus::Serial(port) => self
                .inventory
                .ports
                .iter()
                .find(|endpoint| &endpoint.port_name == port)
                .and_then(|endpoint| self.serial_endpoint_key(endpoint)),
        }?;
        self.registry.observed.contains_key(&key).then_some(key)
    }

    fn nickname_editor_is_current(&self, session: &NicknameEditSession) -> bool {
        match &session.scope {
            NicknameEditorScope::Registry => {
                self.active_page == FeaturePage::Settings && self.registry.open
            }
            NicknameEditorScope::CurrentDevice(focus) => {
                self.active_page == FeaturePage::DeviceSetup
                    && self.sidebar.shows_current_device()
                    && session.matches_current_device(
                        self.command_focus.as_ref(),
                        self.current_nickname_key(focus),
                    )
            }
        }
    }

    pub(super) fn sync_nickname_editor(&mut self) {
        if self
            .registry
            .editor
            .as_ref()
            .is_some_and(|editor| !self.nickname_editor_is_current(&editor.session))
        {
            self.registry.editor = None;
        }
    }

    fn edit_device_nickname(
        &mut self,
        key: DeviceKey,
        scope: NicknameEditorScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down {
            return;
        }
        let Some(record) = self
            .registry
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.get(&key))
        else {
            return;
        };
        let value = record.nickname.clone().unwrap_or_default();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder(tr("devices.registry.nickname.placeholder"));
            input.set_value(value, window, cx);
            input
        });
        self.registry.next_editor_id += 1;
        self.registry.editor = Some(NicknameEditor {
            session: NicknameEditSession {
                id: NicknameEditorId(self.registry.next_editor_id),
                key,
                scope,
                error: None,
            },
            input: input.clone(),
        });
        self.registry.forget = None;
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn save_device_nickname(&mut self, id: NicknameEditorId, clear: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.registry.editor.as_ref().filter(|editor| {
            editor.session.id == id && self.nickname_editor_is_current(&editor.session)
        }) else {
            return;
        };
        let edit = RegistryEdit::Rename {
            key: editor.session.key,
            nickname: if clear {
                String::new()
            } else {
                editor.input.read(cx).text().to_string()
            },
        };
        self.registry_edit(edit, cx);
    }

    fn cancel_device_nickname(&mut self, id: NicknameEditorId, cx: &mut Context<Self>) {
        if self
            .registry
            .editor
            .as_ref()
            .is_some_and(|editor| editor.session.id == id)
        {
            self.registry.editor = None;
            cx.notify();
        }
    }

    fn nickname_shortcut_ready(
        &self,
        id: NicknameEditorId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.registry
            .editor
            .as_ref()
            .filter(|editor| editor.session.id == id)
            .is_some_and(|editor| {
                !editor.input.update(cx, |input, cx| {
                    input.marked_text_range(window, cx).is_some()
                })
            })
    }

    pub(super) fn current_device_nickname_header(
        &self,
        summary: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.registry.io_join.is_some();
        let key = self
            .command_focus
            .as_ref()
            .and_then(|focus| self.current_nickname_key(focus));
        let editor = self.registry.editor.as_ref().filter(|editor| {
            matches!(editor.session.scope, NicknameEditorScope::CurrentDevice(_))
                && self.nickname_editor_is_current(&editor.session)
        });
        let mut context = div().flex_1().flex().flex_col().gap_1().min_w_0();
        for (index, part) in summary.split(" · ").enumerate() {
            context = context.child(clipped(
                format!("current-nickname-identity:{index}"),
                part.to_owned(),
            ));
        }
        let mut identity = div()
            .flex()
            .items_start()
            .gap_1()
            .min_w_0()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(context);
        if let Some(key) = key
            && editor.is_none()
        {
            let remembered = self
                .registry
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.get(&key).is_some());
            if remembered {
                identity = identity.child(
                    Button::new("current-nickname-edit")
                        .small()
                        .ghost()
                        .flex_shrink_0()
                        .icon(gpui_kit::assets::IconName::Pencil)
                        .tooltip(tr("devices.registry.action.edit"))
                        .accessibility_label(tr("devices.registry.action.edit"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let Some(focus) = this.command_focus.clone().filter(|focus| {
                                this.sidebar.shows_current_device()
                                    && this.current_nickname_key(focus) == Some(key)
                            }) else {
                                return;
                            };
                            this.edit_device_nickname(
                                key,
                                NicknameEditorScope::CurrentDevice(focus),
                                window,
                                cx,
                            );
                        })),
                );
            } else if self.registry.snapshot.is_some() {
                identity = identity.child(
                    Button::new("current-nickname-remember")
                        .small()
                        .flex_shrink_0()
                        .label(tr("devices.registry.action.remember"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.command_focus.as_ref().is_some_and(|focus| {
                                this.sidebar.shows_current_device()
                                    && this.current_nickname_key(focus) == Some(key)
                            }) && let Some(observation) =
                                this.registry.observed.get(&key).cloned()
                            {
                                this.registry_edit(RegistryEdit::Remember(observation), cx);
                            }
                        })),
                );
            }
        }
        let mut header = div().flex().flex_col().gap_2().min_w_0().child(identity);
        if let Some(editor) = editor {
            let id = editor.session.id;
            header = header.child(
                div()
                    .id("current-nickname-editor")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .capture_action(cx.listener(move |this, _: &Enter, window, cx| {
                                if this.nickname_shortcut_ready(id, window, cx) {
                                    this.save_device_nickname(id, false, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .capture_action(cx.listener(move |this, _: &Escape, window, cx| {
                                if this.nickname_shortcut_ready(id, window, cx) {
                                    this.cancel_device_nickname(id, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .on_action(|_: &Enter, _, cx| cx.stop_propagation())
                            .on_action(|_: &Escape, _, cx| cx.stop_propagation())
                            .child(
                                Input::new(&editor.input)
                                    .id("current-nickname")
                                    .small()
                                    .disabled(self.registry.saving == Some(id)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("current-nickname-save")
                                    .small()
                                    .label(tr("devices.registry.action.save_compact"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_device_nickname(id, false, cx);
                                    })),
                            )
                            .child(
                                Button::new("current-nickname-cancel")
                                    .small()
                                    .label(tr("common.action.cancel"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cancel_device_nickname(id, cx);
                                    })),
                            ),
                    )
                    .when_some(editor.session.error.as_ref(), |form, error| {
                        form.child(
                            div()
                                .id("current-nickname-error")
                                .role(gpui_kit::Role::Alert)
                                .test_support()
                                .aria_label(error.clone())
                                .text_xs()
                                .whitespace_normal()
                                .text_color(cx.theme().danger)
                                .child(error.clone()),
                        )
                    }),
            );
        }
        if let Some(error) = &self.registry.error {
            header = header
                .child(
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
                .when(!busy, |header| {
                    header.child(
                        Button::new("current-nickname-retry")
                            .small()
                            .label(tr("devices.registry.action.retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.registry.store.is_none() {
                                    this.initialize_device_registry(cx);
                                } else {
                                    this.registry.pending_observe = true;
                                    this.flush_registry_observations(cx);
                                }
                            })),
                    )
                });
        }
        header.into_any_element()
    }

    fn registry_focus_control(&self, id: &'static str, content: impl IntoElement) -> AnyElement {
        super::page_scroll::reveal_on_focus(id, content, &self.registry.scroll).into_any_element()
    }

    pub(super) fn device_registry_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.registry.io_join.is_some();
        let mut page = div()
            .id("device-registry")
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                self.registry_focus_control(
                    "registry-focus-back",
                    Button::new("registry-back")
                        .label(tr("common.action.back"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.registry.open = false;
                            this.registry.editor = None;
                            this.registry.forget = None;
                            this.page_focus.focus(window, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("devices.registry.title")),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr("devices.registry.hint")),
            );
        if busy {
            page = page.child(div().text_sm().child(tr("devices.registry.status.saving")));
        }
        if let Some(error) = &self.registry.error {
            page = page.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(
                        rust_i18n::t!("devices.registry.error.storage", error = error).to_string(),
                    ),
            );
            if !busy {
                page = page.child(
                    self.registry_focus_control(
                        "registry-focus-retry",
                        Button::new("registry-retry")
                            .label(tr("devices.registry.action.retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.registry.store.is_none() {
                                    this.initialize_device_registry(cx);
                                } else {
                                    this.registry.pending_observe = true;
                                    this.flush_registry_observations(cx);
                                }
                            })),
                    ),
                );
            }
        }
        if let Some(snapshot) = &self.registry.snapshot {
            if snapshot.records().next().is_none() {
                page = page.child(div().text_sm().child(tr("devices.registry.empty")));
            }
            for record in snapshot.records() {
                page = page.child(self.registry_record_row(record, cx));
            }
        }
        let unsaved = self.registry.observed.values().filter(|observation| {
            self.registry
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.get(&observation.key).is_none())
        });
        for observation in unsaved {
            let key = observation.key;
            page = page.child(
                div()
                    .id(format!("registry-device-{key}"))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(format!("{} · {}", role_label(key.role()), key.value()))
                    .child(
                        div()
                            .text_sm()
                            .child(tr("devices.registry.status.not_remembered")),
                    )
                    .child(
                        self.registry_focus_control(
                            "registry-focus-remember",
                            Button::new("registry-remember")
                                .small()
                                .label(tr("devices.registry.action.remember"))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(current) = this.registry.observed.get(&key).cloned()
                                    {
                                        this.registry_edit(RegistryEdit::Remember(current), cx);
                                    }
                                })),
                        ),
                    ),
            );
        }
        let page = page.child(
            div()
                .text_sm()
                .whitespace_normal()
                .text_color(cx.theme().muted_foreground)
                .child(tr("devices.registry.identity_hint")),
        );
        super::page_scroll::page_scroll(
            "registry-scroll",
            self.registry
                .focus
                .as_ref()
                .expect("opening the device registry retains its page focus"),
            page,
        )
        .scroll_handle(&self.registry.scroll)
        .into_any_element()
    }

    fn registry_record_row(&self, record: &DeviceRecord, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.registry.io_join.is_some();
        let key = record.key;
        let physical = format!("{} · {}", role_label(key.role()), key.value());
        let mut row = div()
            .id(format!("registry-device-{key}"))
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w_0()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(self.registry_label(Some(&key), physical)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if self.registry.current.contains(&key) {
                        "devices.registry.status.observed"
                    } else {
                        "devices.registry.status.not_observed"
                    })),
            );
        if let Some(receiver) = record.last_receiver
            && let Some(snapshot) = &self.registry.snapshot
        {
            let physical = format!(
                "{} · {}",
                role_label(DeviceRole::Receiver),
                receiver.value()
            );
            let label = receiver_history_label(snapshot, receiver, &physical);
            let line = format!("{}: {label}", tr("devices.registry.last_receiver"));
            row = row.child(
                div()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(clipped("registry-last-receiver".into(), line)),
            );
        }
        for (label, value) in [
            (
                "devices.registry.reported.name",
                record.reported.name.as_deref(),
            ),
            (
                "devices.registry.reported.board",
                record.reported.board.as_deref(),
            ),
            (
                "devices.registry.reported.target",
                record.reported.target.as_deref(),
            ),
            (
                "devices.registry.reported.firmware",
                record.reported.firmware.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                row = row.child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(format!("{}: {value}", tr(label))),
                );
            }
        }
        if let Some(editor) = self.registry.editor.as_ref().filter(|editor| {
            editor.session.key == key && editor.session.scope == NicknameEditorScope::Registry
        }) {
            let id = editor.session.id;
            row = row
                .child(
                    self.registry_focus_control(
                        "registry-focus-nickname",
                        Input::new(&editor.input)
                            .id("registry-nickname")
                            .disabled(self.registry.saving == Some(id)),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            self.registry_focus_control(
                                "registry-focus-save",
                                Button::new("registry-save")
                                    .label(tr("devices.registry.action.save"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_device_nickname(id, false, cx);
                                    })),
                            ),
                        )
                        .child(
                            self.registry_focus_control(
                                "registry-focus-clear-nickname",
                                Button::new("registry-clear-nickname")
                                    .label(tr("devices.registry.action.clear_nickname"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_device_nickname(id, true, cx);
                                    })),
                            ),
                        )
                        .child(
                            self.registry_focus_control(
                                "registry-focus-cancel-edit",
                                Button::new("registry-cancel-edit")
                                    .label(tr("common.action.cancel"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cancel_device_nickname(id, cx);
                                    })),
                            ),
                        ),
                )
                .when_some(editor.session.error.as_ref(), |form, error| {
                    form.child(
                        div()
                            .id("registry-nickname-error")
                            .role(gpui_kit::Role::Alert)
                            .test_support()
                            .aria_label(error.clone())
                            .text_xs()
                            .whitespace_normal()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    )
                });
        } else if self.registry.forget.as_ref() == Some(&key) {
            let forget_key = key;
            row = row
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(tr("devices.registry.forget.confirm")),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            self.registry_focus_control(
                                "registry-focus-confirm-forget",
                                Button::new("registry-confirm-forget")
                                    .label(tr("devices.registry.action.forget"))
                                    .danger()
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.registry_edit(RegistryEdit::Forget(forget_key), cx);
                                    })),
                            ),
                        )
                        .child(
                            self.registry_focus_control(
                                "registry-focus-cancel-forget",
                                Button::new("registry-cancel-forget")
                                    .label(tr("common.action.cancel"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.registry.forget = None;
                                        cx.notify();
                                    })),
                            ),
                        ),
                );
        } else {
            let edit_key = key;
            row = row.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        self.registry_focus_control(
                            "registry-focus-edit",
                            Button::new("registry-edit")
                                .small()
                                .label(tr("devices.registry.action.edit"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_device_nickname(
                                        edit_key,
                                        NicknameEditorScope::Registry,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                    )
                    .child(
                        self.registry_focus_control(
                            "registry-focus-forget",
                            Button::new("registry-forget")
                                .small()
                                .label(tr("devices.registry.action.forget"))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.registry.forget = Some(key);
                                    this.registry.editor = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        row.into_any_element()
    }
}

fn receiver_history_label(
    registry: &DeviceRegistry,
    receiver: DeviceKey,
    physical: &str,
) -> String {
    let label = registry.get(&receiver).and_then(|record| {
        record
            .nickname
            .as_deref()
            .or(record.reported.name.as_deref())
    });
    match label {
        Some(label) => format!("{label} · {physical}"),
        None => physical.to_owned(),
    }
}

fn role_label(role: DeviceRole) -> String {
    tr(match role {
        DeviceRole::Receiver => "common.device_role.receiver",
        DeviceRole::Tracker => "common.device_role.tracker",
        DeviceRole::Unknown => "common.value.unknown",
    })
}

fn registry_store_error(error: &RegistryStoreError) -> String {
    match error {
        RegistryStoreError::Validation(error) => tr(match error {
            RegistryValidationError::TooManyRecords => "devices.registry.error.limit",
            RegistryValidationError::DuplicateIdentity => "devices.registry.error.duplicate",
            RegistryValidationError::InvalidNickname => "devices.registry.error.nickname",
            RegistryValidationError::InvalidMetadata => "devices.registry.error.metadata",
            RegistryValidationError::InvalidReceiver => "devices.registry.error.receiver",
            RegistryValidationError::MissingRecord => "devices.registry.error.missing",
        }),
        RegistryStoreError::Persistence(error) => match error {
            RegistryPersistenceError::Read { path, error } => rust_i18n::t!(
                "devices.registry.error.read",
                path = path.display(),
                error = error
            )
            .to_string(),
            RegistryPersistenceError::Invalid { path, error } => rust_i18n::t!(
                "devices.registry.error.invalid",
                path = path.display(),
                error = error
            )
            .to_string(),
            RegistryPersistenceError::UnsupportedVersion { version } => {
                rust_i18n::t!("devices.registry.error.version", version = version).to_string()
            }
            RegistryPersistenceError::TooLarge => tr("devices.registry.error.size"),
            RegistryPersistenceError::NotRegularFile => tr("devices.registry.error.regular_file"),
            RegistryPersistenceError::Save { path, error } => rust_i18n::t!(
                "devices.registry.error.save",
                path = path.display(),
                error = error
            )
            .to_string(),
        },
    }
}

fn frozen_serial_endpoint_key(endpoint: &SerialEndpoint) -> Option<DeviceKey> {
    if endpoint.kind == crate::domain::SerialPortKind::Usb
        && endpoint.vid == Some(0x1209)
        && endpoint.pid == Some(0x7690)
    {
        endpoint
            .serial_number
            .as_deref()
            .and_then(DeviceKey::receiver_serial)
    } else {
        None
    }
}

fn absent_carrier_keys(
    inventory: &Inventory,
    carriers: &BTreeMap<DeviceKey, BTreeSet<Carrier>>,
    forgotten: &BTreeSet<DeviceKey>,
) -> BTreeSet<DeviceKey> {
    if inventory.serial_enumeration_failed || !inventory.warnings.is_empty() {
        return BTreeSet::new();
    }
    carriers
        .iter()
        .filter(|(key, sources)| {
            forgotten.contains(key)
                && !sources.is_empty()
                && sources.iter().all(|source| !source.present(inventory))
        })
        .map(|(key, _)| *key)
        .collect()
}

async fn drain_registry_store(
    completion: impl Future<Output = Option<RegistryStore>> + Send,
    pending: Option<RegistryEdit>,
    followup: Option<RegistryEdit>,
) {
    if let Some(mut store) = completion.await {
        for edit in pending.into_iter().chain(followup) {
            if store.apply(edit).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Carrier, NicknameEditSession, NicknameEditorId, NicknameEditorScope, RegistrySources,
        RegistryUiState, absent_carrier_keys, drain_registry_store, frozen_serial_endpoint_key,
        receiver_history_label,
    };
    use crate::ui::command_panel::CommandFocus;
    use crate::{
        device_registry::{
            DeviceKey, DeviceObservation, DeviceRecord, DeviceRegistry, RegistryValidationError,
            ReportedMetadata,
        },
        discovery::ReceiverMonitor,
        domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind},
        sessions::SessionManager,
        settings::device_registry::{
            RegistryEdit, RegistryPersistenceError, RegistryStore, RegistryStoreError,
        },
    };
    use futures_util::FutureExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext,
        WindowBounds, WindowOptions, px, size, test::TestWindowExt as _,
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    fn key() -> DeviceKey {
        DeviceKey::tracker_address("001122334455").unwrap()
    }

    fn receiver(serial: &str) -> ReceiverEndpoint {
        ReceiverEndpoint {
            key: "hid:synthetic".into(),
            path: b"same-path".to_vec(),
            label: "Native receiver".into(),
            serial_number: Some(serial.into()),
        }
    }
    fn nickname_session(id: u64, scope: NicknameEditorScope) -> NicknameEditSession {
        NicknameEditSession {
            id: NicknameEditorId(id),
            key: key(),
            scope,
            error: None,
        }
    }

    fn record(key: DeviceKey) -> DeviceRecord {
        DeviceRecord {
            key,
            nickname: None,
            reported: ReportedMetadata::default(),
            last_receiver: None,
        }
    }

    fn registry_fixture(
        cx: &mut TestAppContext,
        records: Vec<DeviceRecord>,
        observed: Vec<DeviceObservation>,
    ) -> (
        AnyWindowHandle,
        Entity<crate::ui::Workbench>,
        crate::app_runtime::AppRuntime,
    ) {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let executor = runtime.executor();

        let (window, workbench) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1320.), px(1000.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        let mut this = crate::ui::test_support::offline_workbench(
                            window,
                            cx,
                            SessionManager::new(),
                            executor,
                        );
                        this.registry.snapshot =
                            Some(Arc::new(DeviceRegistry::from_records(records).unwrap()));
                        this.registry.observed = observed
                            .into_iter()
                            .map(|observation| (observation.key, observation))
                            .collect();
                        this.open_device_registry(window, cx);
                        this
                    })
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert!(
                window.is_window_active(),
                "registry keyboard integration requires an active native window",
            );
            window.render_frame(cx);
        })
        .unwrap();
        (window, workbench, runtime)
    }

    fn settle_registry(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.simulate_next_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
        }
    }

    fn registry_key(cx: &mut TestAppContext, handle: AnyWindowHandle, key: &str) {
        cx.update_window(handle, |_, window, cx| window.press(key, cx))
            .unwrap();
        settle_registry(cx, handle);
    }

    fn assert_registry_focus_visible(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        workbench: &Entity<crate::ui::Workbench>,
        row: Option<DeviceKey>,
        control: &'static str,
    ) {
        cx.update_window(handle, |_, window, cx| {
            let target = match row {
                Some(key) => window
                    .within(format!("registry-device-{key}"))
                    .find(control),
                None => window.find(control),
            };
            let viewport = workbench.read(cx).registry.scroll.bounds();
            let bounds = target.bounds();
            let target_edges = (
                window.pixel_snap(bounds.top()),
                window.pixel_snap(bounds.bottom()),
            );
            let viewport_edges = (
                window.pixel_snap(viewport.top()),
                window.pixel_snap(viewport.bottom()),
            );
            assert_eq!(
                target.focused(),
                Some(true),
                "{control} must own native focus"
            );
            assert!(
                target.visible(),
                "{control} must be visible after keyboard traversal"
            );
            assert!(
                target_edges.0 >= viewport_edges.0 && target_edges.1 <= viewport_edges.1,
                "{control} for {row:?} must fit inside the registry viewport: \
                 bounds={bounds:?}, viewport={viewport:?}, target_edges={target_edges:?}, \
                 viewport_edges={viewport_edges:?}, \
                 offset={:?}, scale={}",
                workbench.read(cx).registry.scroll.offset(),
                window.scale_factor(),
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn registry_keyboard_traversal_reveals_controls_without_resetting_manual_scroll(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        let keys = (1..=24)
            .map(|ix| DeviceKey::tracker_address(&format!("{ix:012X}")).unwrap())
            .collect::<Vec<_>>();
        let (handle, workbench, _runtime) =
            registry_fixture(cx, keys.iter().copied().map(record).collect(), Vec::new());
        cx.simulate_window_resize(handle, size(px(1100.), px(420.)));
        settle_registry(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.click("nav-home", cx);
            window.click("nav-settings", cx);
        })
        .unwrap();
        settle_registry(cx, handle);
        registry_key(cx, handle, "pagedown");
        let manual_offset = cx
            .update_window(handle, |_, window, cx| {
                assert!(!window.find("registry-back").visible());
                let offset = workbench.read(cx).registry.scroll.offset();
                assert!(offset.y < gpui_kit::Pixels::ZERO);
                workbench.update(cx, |_, cx| cx.notify());
                offset
            })
            .unwrap();
        settle_registry(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(
                workbench.read(cx).registry.scroll.offset(),
                manual_offset,
                "ordinary renders must preserve intentional scrolling away from the focus",
            );
            assert!(!window.find("registry-back").visible());
        })
        .unwrap();

        registry_key(cx, handle, "tab");
        assert_registry_focus_visible(cx, handle, &workbench, None, "registry-back");
        for key in &keys {
            for control in ["registry-edit", "registry-forget"] {
                registry_key(cx, handle, "tab");
                assert_registry_focus_visible(cx, handle, &workbench, Some(*key), control);
            }
        }
        for key in keys.iter().rev() {
            registry_key(cx, handle, "shift-tab");
            assert_registry_focus_visible(cx, handle, &workbench, Some(*key), "registry-edit");
            registry_key(cx, handle, "shift-tab");
            if *key == keys[0] {
                assert_registry_focus_visible(cx, handle, &workbench, None, "registry-back");
            } else {
                let previous = keys.iter().position(|candidate| candidate == key).unwrap() - 1;
                assert_registry_focus_visible(
                    cx,
                    handle,
                    &workbench,
                    Some(keys[previous]),
                    "registry-forget",
                );
            }
        }
        let wheel_offset = cx
            .update_window(handle, |_, window, cx| {
                let before = workbench.read(cx).registry.scroll.offset();
                window.scroll(
                    "registry-back",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-200.))),
                    cx,
                );
                let offset = workbench.read(cx).registry.scroll.offset();
                assert!(
                    offset.y < before.y,
                    "native wheel input must scroll the registry",
                );
                assert_eq!(window.find("registry-back").focused(), Some(true));
                assert!(!window.find("registry-back").visible());
                workbench.update(cx, |_, cx| cx.notify());
                offset
            })
            .unwrap();
        settle_registry(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(
                workbench.read(cx).registry.scroll.offset(),
                wheel_offset,
                "ordinary renders must not reveal a control scrolled away by the wheel",
            );
            assert_eq!(window.find("registry-back").focused(), Some(true));
            assert!(!window.find("registry-back").visible());
        })
        .unwrap();

        registry_key(cx, handle, "tab");
        registry_key(cx, handle, "enter");
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(window.find("registry-nickname").focused(), Some(true));
            window.input("Alpha", cx);
            let offset = workbench.read(cx).registry.scroll.offset();
            window.press("home", cx);
            window.input("Z", cx);
            window.press("end", cx);
            window.input("Q", cx);
            window.press("pagedown", cx);
            window.press("pageup", cx);
            assert_eq!(window.find("registry-nickname").value(), Some("ZAlphaQ"));
            assert_eq!(
                workbench.read(cx).registry.scroll.offset(),
                offset,
                "page navigation must not steal nickname editor keys",
            );
        })
        .unwrap();
    }

    fn canceled_invalid_nickname(cx: &mut TestAppContext, previous_storage_error: Option<&str>) {
        let invalid_nickname = "X".repeat(81);
        for inline in [false, true] {
            let editor_name = if inline {
                "current-device editor"
            } else {
                "registry manager"
            };
            let target = DeviceKey::receiver_serial("0011223344556677").unwrap();
            let observation = DeviceObservation {
                key: target,
                reported: ReportedMetadata::default(),
                last_receiver: None,
            };
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("registry.json");
            let mut store = RegistryStore::load(path.clone()).unwrap();
            store
                .apply(RegistryEdit::Observe {
                    observations: vec![observation.clone()],
                    absent: BTreeSet::new(),
                })
                .unwrap();
            let original = std::fs::read(&path).unwrap();
            let (handle, workbench, _runtime) =
                registry_fixture(cx, vec![record(target)], vec![observation]);
            cx.update_window(handle, |_, window, cx| {
                workbench.update(cx, |this, cx| {
                    this.registry.store = Some(store);
                    this.registry.error = previous_storage_error.map(str::to_owned);
                    if inline {
                        let endpoint = receiver("0011223344556677");
                        this.command_focus = Some(CommandFocus::Receiver(endpoint.key.clone()));
                        this.inventory.receivers = vec![endpoint];
                        this.set_feature_page(crate::ui::FeaturePage::DeviceSetup);
                    }
                    cx.notify();
                });
                window.render_frame(cx);
            })
            .unwrap();
            let (edit, input, save, cancel, feedback, retry) = if inline {
                (
                    "current-nickname-edit",
                    "current-nickname",
                    "current-nickname-save",
                    "current-nickname-cancel",
                    "current-nickname-error",
                    "current-nickname-retry",
                )
            } else {
                (
                    "registry-edit",
                    "registry-nickname",
                    "registry-save",
                    "registry-cancel-edit",
                    "registry-nickname-error",
                    "registry-retry",
                )
            };
            cx.update_window(handle, |_, window, cx| {
                window.click(edit, cx);
                let editor = workbench.read(cx).registry.editor.as_ref().unwrap();
                assert!(
                    editor.input.read(cx).focus_handle(cx).is_focused(window),
                    "{editor_name} must transfer native focus to the newly opened draft",
                );
            })
            .unwrap();
            // The first paint observes focus through the previous dispatch tree,
            // which does not contain this newly mounted input yet.
            settle_registry(cx, handle);
            cx.update_window(handle, |_, window, cx| {
                assert_eq!(
                    window.find(input).focused(),
                    Some(true),
                    "{editor_name} must expose focused input after its dispatch tree is published",
                );
                assert_eq!(
                    window.try_find(retry).is_some(),
                    previous_storage_error.is_some(),
                    "{editor_name}: opening a draft must not clear a genuine storage failure",
                );
                window.input(&invalid_nickname, cx);
                assert_eq!(
                    window.find(input).value(),
                    Some(invalid_nickname.as_str()),
                    "{editor_name} must submit the complete invalid draft",
                );
                let save_button = window.find(save);
                let input_bounds = window.find(input).bounds();
                assert!(
                    save_button.visible(),
                    "{editor_name} save must be visible: bounds={:?}, \
                     input_bounds={input_bounds:?}",
                    save_button.bounds(),
                );
                let editor_id = workbench
                    .read(cx)
                    .registry
                    .editor
                    .as_ref()
                    .unwrap()
                    .session
                    .id;
                window.click(save, cx);
                let registry = &workbench.read(cx).registry;
                assert!(
                    registry.saving == Some(editor_id)
                        || registry.editor.as_ref().is_some_and(|editor| {
                            editor.session.id == editor_id && editor.session.error.is_some()
                        }),
                    "{editor_name} save must submit the invalid draft: \
                     save_bounds={:?}, input_bounds={input_bounds:?}, saving={:?}, busy={}, \
                     storage_error={:?}",
                    save_button.bounds(),
                    registry.saving,
                    registry.io_join.is_some(),
                    registry.error,
                );
            })
            .unwrap();
            settle_registry(cx, handle);
            cx.update_window(handle, |_, window, cx| {
                let registry = &workbench.read(cx).registry;
                let editor = registry
                    .editor
                    .as_ref()
                    .expect("invalid nickname must retain the draft");
                assert!(
                    editor.session.error.is_some(),
                    "{editor_name} must retain draft-local validation feedback: \
                     draft={:?}, saving={:?}, busy={}, storage_error={:?}",
                    editor.input.read(cx).text(),
                    registry.saving,
                    registry.io_join.is_some(),
                    registry.error,
                );
                assert!(window.find(feedback).visible());
                assert_eq!(
                    window.find(input).value(),
                    Some(invalid_nickname.as_str()),
                    "{editor_name} must preserve the rejected invalid draft",
                );
                assert_eq!(
                    workbench.read(cx).registry.error.as_deref(),
                    previous_storage_error,
                    "nickname validation belongs to the draft, not storage recovery",
                );
                assert_eq!(
                    window.try_find(retry).is_some(),
                    previous_storage_error.is_some(),
                );
                window.click(cancel, cx);
                assert!(window.try_find(feedback).is_none());
                assert!(workbench.read(cx).registry.editor.is_none());
                assert_eq!(
                    workbench.read(cx).registry.error.as_deref(),
                    previous_storage_error
                );
                window.click("nav-home", cx);
                window.click("nav-settings", cx);
                assert!(window.find("registry-back").visible());
                assert!(window.try_find("registry-nickname-error").is_none());
                assert_eq!(
                    window.try_find("registry-retry").is_some(),
                    previous_storage_error.is_some(),
                    "reentry must not manufacture storage recovery for a canceled invalid draft",
                );
                assert_eq!(
                    workbench
                        .read(cx)
                        .registry
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .get(&target)
                        .unwrap()
                        .nickname,
                    None,
                );
            })
            .unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[gpui_kit::test]
    fn canceled_invalid_nickname_does_not_create_storage_failure_in_either_editor(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        canceled_invalid_nickname(cx, None);
    }

    #[gpui_kit::test]
    fn canceled_invalid_nickname_preserves_independent_storage_failure_in_either_editor(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        canceled_invalid_nickname(cx, Some("synthetic earlier atomic replacement denied"));
    }

    #[gpui_kit::test]
    fn registry_arrivals_preserve_button_focus_nickname_draft_and_forget_target(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        let target = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        let receiver = DeviceKey::receiver_serial("0011223344556677").unwrap();
        let tracker = key();
        let target_row = format!("registry-device-{target}");
        let (handle, workbench, _runtime) = registry_fixture(cx, vec![record(target)], Vec::new());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("tab", cx);
            assert_eq!(window.find("registry-back").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(
                window
                    .within(target_row.clone())
                    .find("registry-edit")
                    .focused(),
                Some(true),
            );

            workbench.update(cx, |this, cx| {
                this.registry.snapshot = Some(Arc::new(
                    DeviceRegistry::from_records(vec![record(receiver), record(target)]).unwrap(),
                ));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window
                    .within(target_row.clone())
                    .find("registry-edit")
                    .focused(),
                Some(true),
                "a newly inserted earlier receiver must not inherit the tracker's focus",
            );
            window.press("enter", cx);
            window.input("Unsaved nickname", cx);
            let input = workbench
                .read(cx)
                .registry
                .editor
                .as_ref()
                .unwrap()
                .input
                .clone();

            workbench.update(cx, |this, cx| {
                this.registry.snapshot = Some(Arc::new(
                    DeviceRegistry::from_records(vec![
                        record(receiver),
                        record(tracker),
                        record(target),
                    ])
                    .unwrap(),
                ));
                cx.notify();
            });
            window.render_frame(cx);
            let field = window.within(target_row.clone()).find("registry-nickname");
            assert_eq!(field.focused(), Some(true));
            assert_eq!(field.value(), Some("Unsaved nickname"));
            assert_eq!(
                workbench.read(cx).registry.editor.as_ref().unwrap().input,
                input,
            );
            window.input(" retained", cx);
            assert_eq!(
                window
                    .within(target_row.clone())
                    .find("registry-nickname")
                    .value(),
                Some("Unsaved nickname retained"),
            );
            window
                .within(target_row.clone())
                .click("registry-cancel-edit", cx);
            window
                .within(target_row.clone())
                .click("registry-forget", cx);

            workbench.update(cx, |this, cx| {
                this.registry.snapshot = Some(Arc::new(
                    DeviceRegistry::from_records(vec![record(target)]).unwrap(),
                ));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(
                window
                    .within(target_row)
                    .find("registry-confirm-forget")
                    .visible(),
                "the confirmation must remain on the same hardware identity",
            );
            assert_eq!(workbench.read(cx).registry.forget, Some(target));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn registry_observation_arrivals_do_not_transfer_remember_button_focus(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        let target = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        let observed = |key| DeviceObservation {
            key,
            reported: ReportedMetadata::default(),
            last_receiver: None,
        };
        let target_row = format!("registry-device-{target}");
        let (handle, workbench, _runtime) =
            registry_fixture(cx, Vec::new(), vec![observed(target)]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("tab", cx);
            window.press("tab", cx);
            assert_eq!(
                window
                    .within(target_row.clone())
                    .find("registry-remember")
                    .focused(),
                Some(true),
            );
            workbench.update(cx, |this, cx| {
                this.registry.observed.insert(key(), observed(key()));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window
                    .within(target_row)
                    .find("registry-remember")
                    .focused(),
                Some(true),
                "a new earlier observation must not inherit another device's control state",
            );
            assert!(workbench.read(cx).registry.editor.is_none());
            assert_eq!(workbench.read(cx).registry.forget, None);
        })
        .unwrap();
    }

    #[test]
    fn nickname_completion_closes_only_its_editor_instance() {
        let submitted = NicknameEditorId(1);
        let mut reopened = nickname_session(2, NicknameEditorScope::Registry);
        assert!(!reopened.complete(submitted, &Ok(())));
        assert_eq!(reopened.id, NicknameEditorId(2));
        assert_eq!(reopened.error, None);
        assert!(!reopened.complete(
            submitted,
            &Err(RegistryStoreError::Validation(
                RegistryValidationError::InvalidNickname,
            )),
        ));
        assert_eq!(reopened.error, None);
        assert!(reopened.complete(NicknameEditorId(2), &Ok(())));
    }

    #[test]
    fn nickname_validation_retains_session_and_error_until_successful_retry() {
        let mut editor = nickname_session(7, NicknameEditorScope::Registry);
        assert!(!editor.complete(
            NicknameEditorId(7),
            &Err(RegistryStoreError::Validation(
                RegistryValidationError::InvalidNickname,
            )),
        ));
        assert_eq!(editor.key, key());
        assert_eq!(editor.id, NicknameEditorId(7));
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| !error.is_empty()),
            "invalid nickname must retain validation feedback",
        );
        assert!(editor.complete(NicknameEditorId(7), &Ok(())));
    }

    #[test]
    fn nickname_persistence_failure_keeps_the_draft_without_local_validation_feedback() {
        let mut editor = nickname_session(7, NicknameEditorScope::Registry);
        editor.error = Some(crate::i18n::tr("devices.registry.error.nickname"));
        assert!(!editor.complete(
            NicknameEditorId(7),
            &Err(RegistryStoreError::Persistence(
                RegistryPersistenceError::Save {
                    path: "synthetic-registry.json".into(),
                    error: "atomic replacement denied".into(),
                }
            )),
        ));
        assert_eq!(editor.id, NicknameEditorId(7));
        assert_eq!(editor.error, None);
    }

    #[test]
    fn nickname_current_scope_requires_exact_focus_and_admitted_key() {
        let focus = CommandFocus::Tracker {
            receiver: "receiver-a".into(),
            slot: 2,
            address: Some("001122334455".into()),
        };
        let editor = nickname_session(1, NicknameEditorScope::CurrentDevice(focus.clone()));
        assert!(editor.matches_current_device(Some(&focus), Some(key())));
        assert!(!editor.matches_current_device(Some(&focus), None));
        assert!(!editor.matches_current_device(None, Some(key())));
        let replacement = CommandFocus::Tracker {
            receiver: "receiver-a".into(),
            slot: 2,
            address: Some("AABBCCDDEEFF".into()),
        };
        assert!(!editor.matches_current_device(Some(&replacement), Some(key())));
        let manager = nickname_session(2, NicknameEditorScope::Registry);
        assert!(!manager.matches_current_device(Some(&focus), Some(key())));
        let wrong_key = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        assert!(!editor.matches_current_device(Some(&focus), Some(wrong_key)));
    }

    #[test]
    fn receiver_replacement_at_same_path_proves_old_carrier_absence() {
        let original = receiver("0011223344556677");
        let sources = BTreeMap::from([(
            key(),
            BTreeSet::from([Carrier::Receiver(
                DeviceKey::receiver_serial(original.serial_number.as_deref().unwrap()).unwrap(),
            )]),
        )]);
        let inventory = Inventory {
            receivers: vec![receiver("8899AABBCCDDEEFF")],
            ..Default::default()
        };
        assert_eq!(
            absent_carrier_keys(&inventory, &sources, &BTreeSet::from([key()])),
            BTreeSet::from([key()])
        );
        assert!(absent_carrier_keys(&inventory, &sources, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn every_carrier_must_be_absent_and_partial_inventory_is_not_absence() {
        let receiver = receiver("0011223344556677");
        let sources = BTreeMap::from([(
            key(),
            BTreeSet::from([
                Carrier::Receiver(
                    DeviceKey::receiver_serial(receiver.serial_number.as_deref().unwrap()).unwrap(),
                ),
                Carrier::Serial {
                    serial: Some("tracker-usb".into()),
                    vid: Some(1),
                    pid: Some(2),
                },
            ]),
        )]);
        let forgotten = BTreeSet::from([key()]);
        let mut inventory = Inventory {
            receivers: vec![receiver],
            ..Default::default()
        };
        assert!(absent_carrier_keys(&inventory, &sources, &forgotten).is_empty());
        inventory.receivers.clear();
        inventory.serial_enumeration_failed = true;
        assert!(absent_carrier_keys(&inventory, &sources, &forgotten).is_empty());
        inventory.serial_enumeration_failed = false;
        inventory
            .warnings
            .push("Synthetic HID backend failure".into());
        assert!(absent_carrier_keys(&inventory, &sources, &forgotten).is_empty());
        inventory.warnings.clear();
        assert_eq!(
            absent_carrier_keys(&inventory, &sources, &forgotten),
            forgotten
        );
    }

    #[test]
    fn serial_carrier_uses_hardware_facts_not_a_reused_port_or_mutable_label() {
        let carrier = Carrier::Serial {
            serial: Some("tracker-usb".into()),
            vid: Some(1),
            pid: Some(2),
        };
        let mut inventory = Inventory {
            ports: vec![SerialEndpoint {
                port_name: "COM_SYNTHETIC".into(),
                label: "Changed native label".into(),
                manufacturer: None,
                product: None,
                kind: SerialPortKind::Usb,
                serial_number: Some("tracker-usb".into()),
                vid: Some(1),
                pid: Some(2),
                usb_interface: None,
            }],
            ..Default::default()
        };
        assert!(carrier.present(&inventory));
        inventory.ports[0].port_name = "COM_MOVED".into();
        assert!(carrier.present(&inventory));
        inventory.ports[0].serial_number = Some("different-tracker-usb".into());
        assert!(!carrier.present(&inventory));
    }

    #[test]
    fn receiver_hardware_move_and_serial_casing_do_not_release_suppression() {
        let identity = DeviceKey::receiver_serial("AABBCCDDEEFF0011").unwrap();
        let mut moved = receiver("aabbccddeeff0011");
        moved.path = b"different-path".to_vec();
        moved.key = "hid:moved".into();
        assert!(Carrier::Receiver(identity).present(&Inventory {
            receivers: vec![moved],
            ..Default::default()
        }));
    }

    #[test]
    fn queued_absence_commits_before_a_coalesced_return_observation() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = RegistryStore::load(directory.path().join("device-registry.json")).unwrap();
        let observation = DeviceObservation {
            key: key(),
            reported: ReportedMetadata::default(),
            last_receiver: None,
        };
        store
            .apply(RegistryEdit::Remember(observation.clone()))
            .unwrap();
        store
            .apply(RegistryEdit::Rename {
                key: key(),
                nickname: "Forgotten nickname".into(),
            })
            .unwrap();
        store.apply(RegistryEdit::Forget(key())).unwrap();
        let mut state = RegistryUiState {
            observed: BTreeMap::from([(key(), observation)]),
            absent: BTreeSet::from([key()]),
            pending_observe: true,
            ..Default::default()
        };
        store.apply(state.next_observation_edit()).unwrap();
        assert!(store.snapshot().get(&key()).is_none());
        assert!(state.pending_observe);
        store.apply(state.next_observation_edit()).unwrap();
        let snapshot = store.snapshot();
        let remembered = snapshot.get(&key()).unwrap();
        assert_eq!(remembered.nickname, None);
        assert_eq!(remembered.key, key());
        assert!(!state.pending_observe);
    }

    #[test]
    fn shutdown_joins_pending_store_and_drains_absence_before_return() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("device-registry.json");
        let mut store = RegistryStore::load(path.clone()).unwrap();
        let observation = DeviceObservation {
            key: key(),
            reported: ReportedMetadata::default(),
            last_receiver: None,
        };
        let other_key = DeviceKey::tracker_address("AABBCCDDEEFF").unwrap();
        store.apply(RegistryEdit::Remember(observation)).unwrap();
        store
            .apply(RegistryEdit::Remember(DeviceObservation {
                key: other_key,
                reported: ReportedMetadata::default(),
                last_receiver: None,
            }))
            .unwrap();
        store.apply(RegistryEdit::Forget(key())).unwrap();
        let (release, blocked) = std::sync::mpsc::sync_channel(0);
        let (finished, completion) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            blocked.recv().unwrap();
            store
                .apply(RegistryEdit::Rename {
                    key: other_key,
                    nickname: "Other retained".into(),
                })
                .unwrap();
            finished.send(store).unwrap();
        });
        let mut state = RegistryUiState::default();
        let receiver = receiver("0011223344556677");
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        let monitor = ReceiverMonitor::fixture(receiver, 3, "001122334455", true);
        let manager = SessionManager::new();
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false,
        }));
        let RegistryEdit::Observe { observations, .. } = state.next_observation_edit() else {
            panic!("observation batch required");
        };
        assert!(
            observations
                .iter()
                .any(|observation| observation.key == key())
        );
        state.forgotten.insert(key());
        let absent_inventory = Inventory::default();
        state.absent.extend(absent_carrier_keys(
            &absent_inventory,
            &state.carriers,
            &state.forgotten,
        ));
        assert_eq!(state.absent, BTreeSet::from([key()]));
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &absent_inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false,
        }));
        assert!(state.reconcile_sources(RegistrySources {
            inventory: &inventory,
            manager: &manager,
            monitor: &monitor,
            targets: &[],
            inventory_failed: false,
        }));
        let pending = state.next_observation_edit();
        let followup = state.pending_observe.then(|| state.next_observation_edit());
        let mut cleanup = Box::pin(drain_registry_store(
            async move { completion.await.ok() },
            Some(pending),
            followup,
        ));
        assert!(cleanup.as_mut().now_or_never().is_none());
        assert!(
            RegistryStore::load(path.clone())
                .unwrap()
                .snapshot()
                .get(&key())
                .is_none()
        );
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            RegistryStore::load(path.clone())
                .unwrap()
                .snapshot()
                .get(&key())
                .is_none()
        );
        assert!(cleanup.as_mut().now_or_never().is_some());
        let snapshot = RegistryStore::load(path).unwrap().snapshot();
        assert_eq!(snapshot.get(&key()).unwrap().nickname, None);
        assert_eq!(
            snapshot.get(&other_key).unwrap().nickname.as_deref(),
            Some("Other retained")
        );
    }

    #[test]
    fn frozen_serial_caption_requires_its_own_supported_receiver_facts() {
        let mut endpoint = SerialEndpoint {
            port_name: "COM_SYNTHETIC".into(),
            label: "Captured physical label".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some("0011223344556677".into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        };
        assert_eq!(
            frozen_serial_endpoint_key(&endpoint),
            DeviceKey::receiver_serial("0011223344556677")
        );
        endpoint.port_name = "COM_REUSED".into();
        assert_eq!(
            frozen_serial_endpoint_key(&endpoint),
            DeviceKey::receiver_serial("0011223344556677")
        );
        endpoint.pid = Some(0x7691);
        assert_eq!(frozen_serial_endpoint_key(&endpoint), None);
        endpoint.pid = Some(0x7690);
        endpoint.serial_number = None;
        assert_eq!(frozen_serial_endpoint_key(&endpoint), None);
    }

    #[test]
    fn receiver_history_label_tracks_rename_and_forget_without_recreating_receiver() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("device-registry.json");
        let mut store = RegistryStore::load(path.clone()).unwrap();
        let receiver = DeviceKey::receiver_serial("0011223344556677").unwrap();
        let physical = "Receiver · 0011223344556677";
        let tracker = DeviceObservation {
            key: key(),
            reported: ReportedMetadata::default(),
            last_receiver: Some(receiver),
        };
        store
            .apply(RegistryEdit::Remember(DeviceObservation {
                key: receiver,
                reported: ReportedMetadata {
                    name: Some("Recorded receiver".into()),
                    ..Default::default()
                },
                last_receiver: None,
            }))
            .unwrap();
        store
            .apply(RegistryEdit::Remember(tracker.clone()))
            .unwrap();
        assert_eq!(
            receiver_history_label(&store.snapshot(), receiver, physical),
            "Recorded receiver · Receiver · 0011223344556677"
        );
        for nickname in ["Desk", "Renamed receiver"] {
            store
                .apply(RegistryEdit::Rename {
                    key: receiver,
                    nickname: nickname.into(),
                })
                .unwrap();
            assert_eq!(
                receiver_history_label(&store.snapshot(), receiver, physical),
                format!("{nickname} · Receiver · 0011223344556677")
            );
        }
        store.apply(RegistryEdit::Forget(receiver)).unwrap();
        store
            .apply(RegistryEdit::Remember(tracker.clone()))
            .unwrap();
        let snapshot = RegistryStore::load(path).unwrap().snapshot();
        assert_eq!(
            receiver_history_label(&snapshot, receiver, physical),
            "Receiver · 0011223344556677"
        );
        assert_eq!(snapshot.records().len(), 1);
        assert_eq!(snapshot.get(&receiver), None);
        assert_eq!(
            snapshot.get(&tracker.key).unwrap().last_receiver,
            Some(receiver)
        );
    }
}
