mod calibration;
mod capture;
mod command_panel;
mod components;
mod date_label;
mod device_registry;
mod devices;
mod flash;
#[cfg(debug_assertions)]
pub(crate) mod gallery;
mod hid_log;
mod home;
mod lifecycle;
mod navigation;
mod notices;
mod page_scroll;
#[cfg(test)]
mod polling_tests;
mod preferences;
mod replay;
mod rssi_scan;
mod settings_page;
mod shortcuts;
mod temperature;
mod terminal;
#[cfg(test)]
mod test_support;
mod theme;
mod tools;
mod tracker_cards;

use command_panel::{CommandFocus, CommandSidebar};
use components::clipped;
use devices::clear_tracker_selection;
use lifecycle::ShutdownHost;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use terminal::{PendingWrite, TabSelection, TerminalSession};

use futures_util::future::Shared;

use gpui_kit::base::{h_resizable, v_resizable};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, ResizableState, Selectable, Sizable, TitleBar,
    button::Button,
    checkbox::Checkbox,
    input::{Input, InputState},
    resizable_panel,
};
use gpui_kit::{
    self as gpui, AnyElement, AssetSource, Axis, Bounds, Context, Entity, FocusHandle, FontWeight,
    IntoElement, KeyBinding, PathBuilder, Pixels, Render, SharedString, Task, TitlebarOptions,
    Window, WindowBounds, WindowDecorations, WindowOptions, actions, canvas, div, point,
    prelude::*, px, rgb, size,
};

use crate::{
    app_runtime::Executor,
    discovery,
    domain::Inventory,
    i18n::tr,
    operations::BatchRun,
    sessions::SessionManager,
    settings::{self, Preferences},
};

actions!(slimenrf_helper, [Quit]);

// Keep component defaults and embed only the extra sidebar glyphs.
gpui::assets::icon_assets!(
    SidebarIconAssets,
    [
        Wifi,
        Activity,
        Gauge,
        Axis3d,
        Thermometer,
        Compass,
        Lightbulb,
        Power,
        Wrench,
        House,
        Plug,
        Usb,
        ArrowLeft,
        Pencil
    ]
);

struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match SidebarIconAssets.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui::assets::Assets.list(path)?;
        paths.extend(SidebarIconAssets.list(path)?);
        Ok(paths)
    }
}

pub fn run(async_io: Executor) {
    // The host retains cleanup even if thread creation fails or GPUI exits early.
    let shutdown_host = Arc::new(ShutdownHost::default());
    let ui_shutdown_host = shutdown_host.clone();
    gpui::application().with_assets(AppAssets).run(move |cx| {
        gpui::init(cx);
        cx.bind_keys([
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-q", Quit, None),
            #[cfg(target_os = "macos")]
            KeyBinding::new(
                "cmd-c",
                gpui_kit::component::input::Copy,
                Some("TerminalTranscript"),
            ),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new(
                "ctrl-c",
                gpui_kit::component::input::Copy,
                Some("TerminalTranscript"),
            ),
        ]);
        let bounds = Bounds::centered(None, size(px(1320.), px(820.)), cx);
        if let Err(error) = gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(concat!("SlimeNRF Helper ", env!("CARGO_PKG_VERSION")).into()),
                    ..TitleBar::title_bar_options()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(1100.), px(760.))),
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            },
            cx,
            move |window, cx| {
                let workbench = cx.new(|cx| Workbench::new(window, cx, async_io, ui_shutdown_host));
                let close_workbench = workbench.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    let _ = close_workbench.update(cx, |this, cx| this.begin_shutdown(cx));
                    false
                });
                let quit_workbench = workbench.downgrade();
                cx.on_action(move |_: &Quit, cx| {
                    let _ = quit_workbench.update(cx, |this, cx| this.begin_shutdown(cx));
                });
                let window_id = window.window_handle().window_id();
                let closed_workbench = workbench.clone();
                // Keep the owner alive even if another caller removes the window directly.
                cx.on_window_closed(move |cx, closed_id| {
                    if closed_id == window_id {
                        closed_workbench.update(cx, |this, cx| this.begin_shutdown(cx));
                    }
                })
                .detach();
                workbench
            },
        ) {
            eprintln!("{}", rust_i18n::t!("app.error.open_window", error = error));
            cx.quit();
        }
        cx.activate(true);
    });
    shutdown_host.finish();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FeaturePage {
    Home,
    DeviceSetup,
    Settings,
    Uf2,
    Ota,
    RawCapture,
    GyroCalibration,
    VqfReplay,
    HidLog,
    Tools,
}

/// Device setup keeps its console readable while the navigation yields at narrow widths.
const DEVICE_WORKSPACE_MIN_WIDTH_REM: f32 = 36.25;
/// The command pane starts above its resize minimum so either pane can regain space.
const COMMAND_SIDEBAR_WIDTH_REM: f32 = 18.75;
const COMMAND_SIDEBAR_MIN_WIDTH_REM: f32 = 17.5;

struct SerialPoll {
    redraw: bool,
    state_changed: bool,
}

/// Borrow the displayed terminal revision without allocating a canonical port key.
fn serial_visible_revision(
    manager: &SessionManager,
    terminal_visible: bool,
    active_port: Option<&str>,
) -> Option<(u64, u64)> {
    if !terminal_visible {
        return None;
    }
    let port = active_port?;
    manager
        .sessions()
        .find(|session| session.port == port)
        .map(|session| (session.generation, session.terminal.revision()))
}

fn serial_poll_result(
    changes: &crate::sessions::SessionChanges,
    before: Option<(u64, u64)>,
    after: Option<(u64, u64)>,
) -> SerialPoll {
    SerialPoll {
        redraw: changes.state || (changes.display && before != after),
        state_changed: changes.state,
    }
}

fn inventory_display_changed(before: &Inventory, after: &Inventory) -> bool {
    before.serial_enumeration_failed != after.serial_enumeration_failed
        || before.warnings != after.warnings
        || (!after.serial_enumeration_failed && before.ports.len() != after.ports.len())
        || before.receivers.len() != after.receivers.len()
        || (!after.serial_enumeration_failed
            && before.ports.iter().zip(&after.ports).any(|(a, b)| {
                a.port_name != b.port_name
                    || a.label != b.label
                    || a.manufacturer != b.manufacturer
                    || a.product != b.product
                    || a.kind != b.kind
                    || a.serial_number != b.serial_number
                    || a.vid != b.vid
                    || a.pid != b.pid
                    || a.usb_interface != b.usb_interface
            }))
        || before.receivers.iter().zip(&after.receivers).any(|(a, b)| {
            a.key != b.key
                || a.path != b.path
                || a.label != b.label
                || a.serial_number != b.serial_number
        })
}

struct Workbench {
    async_io: Executor,
    shutdown_host: Arc<ShutdownHost>,
    inventory: Inventory,
    manager: SessionManager,
    selected: BTreeSet<String>,
    monitor: discovery::ReceiverMonitor,
    serial_notifications: discovery::SerialNotifications,
    scanning_inventory: bool,
    inventory_generation: u64,
    next_enumeration: Instant,
    layout_state: Entity<ResizableState>,
    sidebar_layout_state: Entity<ResizableState>,
    resize_handle_appearance: gpui_kit::base::ResizeHandleRenderer,
    navigation_collapsed: bool,
    active_page: FeaturePage,
    home: home::HomeState,
    home_page_focus: FocusHandle,
    page_focus: FocusHandle,
    tools_page_focus: FocusHandle,
    calibration_page_focus: FocusHandle,
    preferences: Preferences,
    settings: settings_page::SettingsUiState,
    registry: device_registry::RegistryUiState,
    preferences_io: preferences::PreferenceIo,
    sidebar: CommandSidebar,
    command_focus: Option<CommandFocus>,
    command_error: Option<String>,
    inventory_error: Option<String>,
    error: Option<String>,
    terminals: HashMap<String, TerminalSession>,
    tabs: TabSelection,
    pending_writes: Vec<PendingWrite>,
    batch_input: Entity<InputState>,
    batch_history: VecDeque<BatchRun>,
    custom_commands: shortcuts::CustomCommandState,
    flash: flash::FlashState,
    capture: capture::CaptureUiState,
    calibration: calibration::CalibrationUiState,
    replay: replay::ReplayUiState,
    hid_log: hid_log::HidLogUiState,
    temperature: temperature::TemperatureWindowState,
    rssi: rssi_scan::RssiWindowState,
    notices: notices::NoticesWindowState,
    shutting_down: bool,
    shutdown_cleanup: Option<Shared<Task<()>>>,
    _shutdown_task: Option<Task<()>>,
    _poll_task: Task<()>,
    _serial_poll_task: Task<()>,
    inventory_task: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InventoryScanIntent {
    Scheduled,
    Notify,
    Rediscover,
}

impl InventoryScanIntent {
    fn rediscover_bindings(
        self,
        monitor: &mut discovery::ReceiverMonitor,
        previous_generation: u64,
    ) -> bool {
        if self != Self::Rediscover || previous_generation == 0 {
            return false;
        }
        monitor.rediscover_bindings();
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
enum InventoryScanTrigger {
    Scheduled,
    NativeChange,
}

fn inventory_scan_trigger(
    scanning: bool,
    next_enumeration: Instant,
    now: Instant,
    take_native_change: impl FnOnce() -> bool,
) -> Option<InventoryScanTrigger> {
    if scanning {
        return None;
    }
    let native_change = take_native_change();
    if native_change {
        Some(InventoryScanTrigger::NativeChange)
    } else if now >= next_enumeration {
        Some(InventoryScanTrigger::Scheduled)
    } else {
        None
    }
}

impl Workbench {
    fn poll_sessions(&mut self) -> SerialPoll {
        let terminal_visible = self.active_page == FeaturePage::DeviceSetup;
        let before =
            serial_visible_revision(&self.manager, terminal_visible, self.tabs.active.as_deref());
        let changes = self.manager.poll();
        self.observe_flash_identities(&changes);
        if changes.refresh_inventory {
            self.next_enumeration = self.next_enumeration.min(Instant::now());
        }
        let binding_changed = self.poll_receiver_binding_changes();
        let mut polled = serial_poll_result(
            &changes,
            before,
            serial_visible_revision(&self.manager, terminal_visible, self.tabs.active.as_deref()),
        );
        polled.redraw |= binding_changed;
        polled.state_changed |= binding_changed;
        polled
    }

    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        async_io: Executor,
        shutdown_host: Arc<ShutdownHost>,
    ) -> Self {
        let (preferences, preferences_load_error) = match settings::load() {
            Ok(preferences) => (preferences, None),
            Err(error) => (Preferences::default(), Some(error)),
        };
        theme::apply(&preferences, cx);
        let batch_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("serial.terminal.command_placeholder.batch"))
        });
        cx.on_app_quit(|this, cx| {
            // A committed platform quit cannot keep GPUI alive indefinitely.
            // Start retained cleanup without blocking the UI; run() joins its
            // independent thread after the event loop returns.
            drop(this.shutdown_cleanup(cx));
            async {}
        })
        .detach();
        let serial_timer = cx.background_executor().clone();
        let serial_poll_task = cx.spawn(async move |this, cx| {
            loop {
                serial_timer.timer(Duration::from_millis(16)).await;
                if !this
                    .update(cx, |this, cx| {
                        if this.shutting_down {
                            return false;
                        }
                        let polled = this.poll_sessions();
                        if polled.state_changed {
                            this.sync_registry_observations(cx);
                        }
                        if polled.redraw {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        });
        let timer = cx.background_executor().clone();
        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                timer.timer(Duration::from_millis(50)).await;
                if !this
                    .update(cx, |this, cx| {
                        if this.shutting_down {
                            return false;
                        }
                        let changed = this.monitor.poll()
                            | this.poll_receiver_binding_changes()
                            | this.poll_writes()
                            | this.poll_custom_commands()
                            | this.poll_serial_dfu(cx)
                            | this.poll_flash()
                            | this.poll_capture(cx)
                            | this.poll_calibration(cx)
                            | this.poll_replay(cx)
                            | this.poll_hid_log()
                            | this.poll_temperature(cx)
                            | this.poll_rssi_scans(cx);
                        if changed {
                            this.sync_registry_observations(cx);
                        }
                        let now = Instant::now();
                        if let Some(trigger) = inventory_scan_trigger(
                            this.scanning_inventory,
                            this.next_enumeration,
                            now,
                            || this.serial_notifications.take_dirty(),
                        ) {
                            if trigger == InventoryScanTrigger::NativeChange {
                                this.manager.note_native_change(Instant::now());
                            }
                            this.next_enumeration = now + Duration::from_secs(3);
                            this.start_inventory_scan(cx, InventoryScanIntent::Scheduled);
                        }
                        if changed {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        });
        let mut this = Self {
            async_io,
            shutdown_host,
            inventory: Inventory::default(),
            manager: SessionManager::new(),
            selected: BTreeSet::new(),
            monitor: discovery::ReceiverMonitor::new(),
            serial_notifications: discovery::SerialNotifications::new(),
            scanning_inventory: false,
            inventory_generation: 0,
            next_enumeration: Instant::now() + Duration::from_secs(3),
            layout_state: cx.new(|_| ResizableState::default()),
            sidebar_layout_state: cx.new(|_| ResizableState::default()),
            resize_handle_appearance: workbench_resize_handle(),
            navigation_collapsed: false,
            active_page: if preferences_load_error.is_some() {
                FeaturePage::Settings
            } else {
                FeaturePage::Home
            },
            page_focus: cx.focus_handle().tab_stop(false),
            tools_page_focus: cx.focus_handle().tab_stop(false),
            calibration_page_focus: cx.focus_handle().tab_stop(false),
            home: home::HomeState::default(),
            home_page_focus: cx.focus_handle().tab_stop(false),
            preferences,
            settings: settings_page::SettingsUiState::new(window, cx),
            registry: device_registry::RegistryUiState::default(),
            preferences_io: preferences::PreferenceIo::new(preferences_load_error),
            sidebar: CommandSidebar::default(),
            command_focus: None,
            command_error: None,
            inventory_error: None,
            error: None,
            terminals: HashMap::new(),
            tabs: TabSelection::default(),
            pending_writes: Vec::new(),
            batch_input,
            batch_history: VecDeque::new(),
            custom_commands: shortcuts::CustomCommandState::default(),
            flash: flash::FlashState::default(),
            capture: capture::CaptureUiState::default(),
            calibration: calibration::CalibrationUiState::default(),
            replay: replay::ReplayUiState::default(),
            hid_log: hid_log::HidLogUiState::default(),
            temperature: temperature::TemperatureWindowState::default(),
            rssi: rssi_scan::RssiWindowState::default(),
            notices: notices::NoticesWindowState::default(),
            shutting_down: false,
            shutdown_cleanup: None,
            _shutdown_task: None,
            _poll_task: poll_task,
            _serial_poll_task: serial_poll_task,
            inventory_task: None,
        };
        // Enumeration and read-only HID monitoring never open a serial session.
        this.scan_inventory(cx);
        this.initialize_saved_commands(cx);
        this.initialize_device_registry(cx);
        if this.active_page == FeaturePage::Home {
            this.home_page_focus.focus(window, cx);
        }
        this
    }

    fn scan_inventory(&mut self, cx: &mut Context<Self>) {
        self.start_inventory_scan(cx, InventoryScanIntent::Notify);
    }

    fn rediscover_inventory(&mut self, cx: &mut Context<Self>) {
        self.start_inventory_scan(cx, InventoryScanIntent::Rediscover);
    }

    fn start_inventory_scan(&mut self, cx: &mut Context<Self>, intent: InventoryScanIntent) {
        if self.shutting_down || self.scanning_inventory {
            return;
        }
        if intent.rediscover_bindings(&mut self.monitor, self.inventory_generation) {
            self.poll_receiver_binding_changes();
        }
        self.inventory_generation += 1;
        let generation = self.inventory_generation;
        let notify_scan = intent != InventoryScanIntent::Scheduled
            || generation == 1
            || matches!(self.active_page, FeaturePage::Uf2 | FeaturePage::Ota);
        self.scanning_inventory = true;
        let scan_started_at = Instant::now();
        let scan = cx.background_executor().spawn(async { discovery::scan() });
        self.inventory_task = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                let readiness = this.home_readiness();
                let Some(changed) =
                    this.apply_inventory_result(generation, scan_started_at, result)
                else {
                    return;
                };
                let redraw = notify_scan || changed;
                this.sync_registry_observations(cx);
                if redraw || readiness != this.home_readiness() {
                    cx.notify();
                }
            });
        }));
        if notify_scan {
            cx.notify();
        }
    }

    fn apply_inventory_result(
        &mut self,
        generation: u64,
        scan_started_at: Instant,
        result: Result<Inventory, String>,
    ) -> Option<bool> {
        if self.shutting_down || self.inventory_generation != generation {
            return None;
        }
        self.scanning_inventory = false;
        let mut redraw = false;
        match result {
            Ok(mut inventory) => {
                redraw |= self
                    .flash
                    .reconcile_receiver_result_presence(&inventory, scan_started_at);
                if inventory.serial_enumeration_failed {
                    self.manager.invalidate_inventory();
                    inventory.ports = std::mem::take(&mut self.inventory.ports);
                } else {
                    self.manager.update_inventory(&inventory, scan_started_at);
                }
                redraw |= self.inventory_error.is_some()
                    || inventory_display_changed(&self.inventory, &inventory);
                self.inventory_error = None;
                self.monitor.reconcile(&inventory.receivers);
                self.inventory = inventory;
                self.registry_inventory_arrived();
            }
            Err(error) => {
                // Retain the roster for display, never stale USB retry eligibility.
                self.manager.invalidate_inventory();
                let error = rust_i18n::t!("devices.discovery.error.roster_retained", error = error)
                    .to_string();
                redraw |= self.inventory_error.as_deref() != Some(error.as_str());
                self.inventory_error = Some(error);
            }
        }
        Some(redraw)
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let title_bar = TitleBar::new()
            .text_sm()
            .on_close_window(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.begin_shutdown(cx));
            })
            .child(concat!("SlimeNRF Helper ", env!("CARGO_PKG_VERSION")));
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(title_bar)
            .child(self.render_content(window, cx))
    }
}

impl Workbench {
    fn render_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.shutting_down || self.active_page != FeaturePage::VqfReplay {
            self.hide_replay(window, cx);
        }
        if self.shutting_down {
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .child(tr("app.status.closing"))
                .into_any_element();
        }
        self.sync_device_tracker_availability();
        self.sync_tabs(window, cx);
        self.sync_nickname_editor();
        self.complete_custom_terminal_clear(window, cx);
        self.resume_preparation();
        self.resume_terminal_command(window, cx);
        let root = div()
            .w_full()
            .flex_1()
            .flex()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_2()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.feature_navigation(window, cx));
        if self.active_page == FeaturePage::Home {
            return root.child(self.home_page(cx)).into_any_element();
        }
        if self.active_page == FeaturePage::Settings {
            return root
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.settings_page(window, cx)),
                )
                .into_any_element();
        }
        if matches!(self.active_page, FeaturePage::Uf2 | FeaturePage::Ota) {
            return root
                .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                    this.drop_firmware(paths, cx);
                }))
                .child(
                    div()
                        .track_focus(&self.page_focus)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.flash_page(cx)),
                )
                .into_any_element();
        }
        if self.active_page == FeaturePage::RawCapture {
            return root
                .child(
                    div()
                        .track_focus(&self.page_focus)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.capture_page(window, cx)),
                )
                .into_any_element();
        }
        if self.active_page == FeaturePage::GyroCalibration {
            return root
                .child(self.render_calibration(window, cx))
                .into_any_element();
        }
        if self.active_page == FeaturePage::VqfReplay {
            return root
                .child(
                    div()
                        .track_focus(&self.page_focus)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.replay_page(window, cx)),
                )
                .into_any_element();
        }
        if self.active_page == FeaturePage::HidLog {
            return root
                .child(
                    div()
                        .track_focus(&self.page_focus)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.hid_log_page(window, cx)),
                )
                .into_any_element();
        }
        if self.active_page == FeaturePage::Tools {
            return root.child(self.tools_page(cx)).into_any_element();
        }
        let mut devices = div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .bg(cx.theme().muted)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("devices.roster.title")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                rust_i18n::t!(
                                    "devices.roster.summary",
                                    serial = self.inventory.ports.len(),
                                    receivers = self.inventory.receivers.len(),
                                    selected = self.selected.len()
                                )
                                .to_string(),
                            ),
                    )
                    .child(
                        Button::new("roster-manage-devices")
                            .small()
                            .label(tr("devices.registry.action.manage"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_device_registry(window, cx);
                            })),
                    )
                    .when(
                        self.selected.iter().any(|key| key.starts_with("tracker:")),
                        |row| {
                            row.child(
                                Button::new("clear-tracker-selection")
                                    .small()
                                    .label(tr("common.action.clear_selection"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        clear_tracker_selection(&mut this.selected);
                                        this.selection_changed();
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .child(
                        Button::new("enumerate")
                            .small()
                            .label(tr("devices.roster.refresh"))
                            .icon(IconName::RefreshCw)
                            .on_click(cx.listener(|this, _, _, cx| this.rediscover_inventory(cx))),
                    ),
            )
            .child(self.roster(cx));
        if let Some(error) = &self.error {
            devices = devices.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(clipped("workbench-error".into(), error.clone())),
            );
        }
        if let Some(error) = &self.inventory_error {
            devices = devices.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(clipped("inventory-error".into(), error.clone())),
            );
        }
        let workspace = v_resizable("workbench-split")
            .with_state(&self.layout_state)
            .with_handle_appearance(self.resize_handle_appearance.clone())
            .child(
                resizable_panel()
                    .size(px(320.))
                    .size_range(px(180.)..Pixels::MAX)
                    .pb(px(5.))
                    .child(devices),
            )
            .child(
                resizable_panel()
                    .size_range(px(260.)..Pixels::MAX)
                    .pt(px(5.))
                    .child(self.workspace(window, cx)),
            );
        root.child(
            div()
                .track_focus(&self.page_focus)
                .flex_1()
                .h_full()
                .min_w_0()
                .min_h_0()
                .child(
                    h_resizable("sidebar-split")
                        .with_state(&self.sidebar_layout_state)
                        .with_handle_appearance(self.resize_handle_appearance.clone())
                        .child(
                            resizable_panel()
                                .size_range(
                                    window.rem_size() * DEVICE_WORKSPACE_MIN_WIDTH_REM..Pixels::MAX,
                                )
                                .pr(px(5.))
                                .child(workspace),
                        )
                        .child(
                            resizable_panel()
                                .size(window.rem_size() * COMMAND_SIDEBAR_WIDTH_REM)
                                .size_range(
                                    window.rem_size() * COMMAND_SIDEBAR_MIN_WIDTH_REM..Pixels::MAX,
                                )
                                .pl(px(5.))
                                .child(self.command_sidebar(cx)),
                        ),
                ),
        )
        .into_any_element()
    }
}

fn workbench_resize_handle() -> gpui_kit::base::ResizeHandleRenderer {
    std::rc::Rc::new(|handle, _, cx| {
        use gpui_kit::base::ResizeHandleState;
        let (length, color) = match handle.state() {
            ResizeHandleState::Idle => (24., cx.theme().muted_foreground),
            ResizeHandleState::Hovered => (32., cx.theme().primary),
            ResizeHandleState::Pressed | ResizeHandleState::Dragging => (44., cx.theme().primary),
        };
        // Padding on adjacent panels supplies the 10px visual gutter.
        // The library still owns the fixed hit area and drag behavior.
        Some(
            div()
                .flex_none()
                .flex()
                .map(|line| match handle.axis() {
                    Axis::Horizontal => line.w(px(1.)).h_full().items_center(),
                    _ => line.h(px(1.)).w_full().justify_center(),
                })
                .child(
                    div()
                        .flex_none()
                        .rounded_md()
                        .bg(color)
                        .map(|grip| match handle.axis() {
                            Axis::Horizontal => grip.w(px(3.)).h(px(length)).ml(px(-1.)),
                            _ => grip.h(px(3.)).w(px(length)).mt(px(-1.)),
                        }),
                )
                .into_any_element(),
        )
    })
}
