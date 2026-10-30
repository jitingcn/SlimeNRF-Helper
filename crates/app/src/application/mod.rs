mod admission;
mod bootstrap;
mod device_coordinator;
mod lifecycle;
mod navigation;
mod preferences;
mod pump;
#[cfg(test)]
mod test_support;
mod tools;

// These application-owned adapters are colocated with the capability they integrate.
#[path = "../features/app_update/application.rs"]
mod app_update;
#[path = "../features/calibration/application.rs"]
mod calibration;
#[path = "../features/capture/application.rs"]
mod capture;
#[path = "../features/commands/panel.rs"]
mod command_panel;
#[path = "../features/device_registry/application.rs"]
mod device_registry;
#[path = "../features/device_setup/devices.rs"]
mod devices;
#[path = "../features/firmware/application.rs"]
mod flash;
#[cfg(debug_assertions)]
#[path = "../developer/gallery.rs"]
pub(crate) mod gallery;
#[path = "../features/hid_log/application.rs"]
mod hid_log;
#[path = "../features/home/application.rs"]
mod home;
#[path = "../features/notices/application.rs"]
mod notices;
#[path = "../features/settings/preferences.rs"]
mod preference_io;
#[path = "../features/rssi/application.rs"]
mod rssi_scan;
#[path = "../features/commands/saved.rs"]
mod saved_commands;
#[path = "../features/settings/application.rs"]
mod settings_page;
#[path = "../features/commands/execution.rs"]
mod shortcuts;
#[path = "../features/temperature/application.rs"]
mod temperature;
#[path = "../features/device_setup/terminal.rs"]
mod terminal;

use crate::components::{self, date_label, page_scroll, tracker_cards};
use crate::features::{replay::view as replay, settings::theme};
#[cfg(any(debug_assertions, test))]
use bootstrap::AppAssets;
pub(crate) use bootstrap::run;

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
    ActiveTheme, Disableable, IconName, ResizableState, Sizable, TitleBar,
    button::Button,
    input::{InputEvent, InputState},
    resizable_panel,
};
use gpui_kit::{
    self as gpui, AnyElement, Axis, Context, Entity, FocusHandle, FontWeight, IntoElement,
    PathBuilder, Pixels, Render, SharedString, Subscription, Task, Window, actions, canvas, div,
    point, prelude::*, px, rgb,
};

use crate::{
    app_runtime::Executor,
    discovery,
    domain::Inventory,
    features::{
        commands::batch::BatchRun,
        settings::store::{self as settings, Preferences},
    },
    i18n::tr,
    sessions::SessionManager,
};

actions!(slimenrf_helper, [Quit]);

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
    saved_commands: saved_commands::SavedCommandsState,
    home_page_focus: FocusHandle,
    page_focus: FocusHandle,
    tools_page_focus: FocusHandle,
    calibration_page_focus: FocusHandle,
    preferences: Preferences,
    settings: settings_page::SettingsUiState,
    registry: device_registry::RegistryUiState,
    app_update: app_update::AppUpdateState,
    preferences_io: preference_io::PreferenceIo,
    sidebar: CommandSidebar,
    command_focus: Option<CommandFocus>,
    command_error: Option<SharedString>,
    inventory_error: Option<SharedString>,
    error: Option<SharedString>,
    terminals: HashMap<String, TerminalSession>,
    tabs: TabSelection,
    pending_writes: Vec<PendingWrite>,
    batch_input: Entity<InputState>,
    command_search: Entity<InputState>,
    firmware_search: Entity<InputState>,
    _command_search_subscription: Subscription,
    _firmware_search_subscription: Subscription,
    _firmware_workflow_subscription: Subscription,
    batch_history: VecDeque<BatchRun>,
    custom_commands: shortcuts::CustomCommandState,
    flash: flash::FlashState,
    capture: capture::CaptureUiState,
    calibration: calibration::CalibrationUiState,
    replay: Entity<replay::ReplayFeature>,
    hid_log: hid_log::HidLogUiState,
    temperature: crate::features::temperature::window::TemperatureWindowState,
    rssi: rssi_scan::RssiWindowState,
    notices: notices::NoticesWindowState,
    shutting_down: bool,
    shutdown_cleanup: Option<Shared<Task<()>>>,
    _shutdown_task: Option<Task<()>>,
    _poll_task: Task<()>,
    _serial_poll_task: Task<()>,
    inventory_task: Option<Task<()>>,
}

fn search_input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<Workbench>,
) -> (Entity<InputState>, Subscription) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(placeholder)));
    let subscription = cx.subscribe_in(&input, window, |_, _, event, _, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    });
    (input, subscription)
}

impl Workbench {
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
        let (command_search, command_search_subscription) =
            search_input("commands.search.placeholder", window, cx);
        let (firmware_search, firmware_search_subscription) =
            search_input("firmware.remote.search.placeholder", window, cx);
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
                    .update_in(cx, |this, window, cx| {
                        if this.shutting_down {
                            return false;
                        }
                        if this.advance_serial_sessions(window, cx) {
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
                    .update_in(cx, |this, window, cx| {
                        if this.shutting_down {
                            return false;
                        }
                        if this.advance_background_work(window, cx) {
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
            saved_commands: Default::default(),
            home_page_focus: cx.focus_handle().tab_stop(false),
            preferences,
            settings: settings_page::SettingsUiState::new(window, cx),
            registry: device_registry::RegistryUiState::default(),
            app_update: app_update::AppUpdateState::new(),
            preferences_io: preference_io::PreferenceIo::new(preferences_load_error),
            sidebar: CommandSidebar::default(),
            command_focus: None,
            command_error: None,
            inventory_error: None,
            error: None,
            terminals: HashMap::new(),
            tabs: TabSelection::default(),
            pending_writes: Vec::new(),
            batch_input,
            command_search,
            firmware_search,
            _command_search_subscription: command_search_subscription,
            _firmware_search_subscription: firmware_search_subscription,
            _firmware_workflow_subscription: Self::observe_firmware_workflow(cx),
            batch_history: VecDeque::new(),
            custom_commands: shortcuts::CustomCommandState::default(),
            flash: flash::FlashState::default(),
            capture: capture::CaptureUiState::default(),
            calibration: calibration::CalibrationUiState::default(),
            replay: cx.new(|_| replay::ReplayFeature::new(window, workbench_resize_handle())),
            hid_log: hid_log::HidLogUiState::default(),
            temperature: Default::default(),
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
        if this.active_page == FeaturePage::HidLog {
            this.activate_hid_log(cx);
        }
        if this.active_page == FeaturePage::VqfReplay {
            this.replay
                .update(cx, |replay, cx| replay.activate(window, cx));
        }
        this
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
            .child(concat!(
                "SlimeNRF Helper ",
                env!("SLIMENRF_BUILD_DISPLAY_VERSION")
            ));
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
                        .child(self.replay.clone()),
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
                    .rounded_md()
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
                    .child({
                        let label = SharedString::from(tr("devices.registry.action.manage"));
                        Button::new("roster-manage-devices")
                            .small()
                            .icon(gpui_kit::assets::IconName::List)
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .disabled(self.app_update_navigation_locked())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_device_registry(window, cx);
                            }))
                    })
                    .when(
                        self.selected.iter().any(|key| key.starts_with("tracker:")),
                        |row| {
                            let label = SharedString::from(tr("common.action.clear_selection"));
                            row.child(
                                Button::new("clear-tracker-selection")
                                    .small()
                                    .icon(gpui_kit::assets::IconName::ListX)
                                    .tooltip(label.clone())
                                    .accessibility_label(label)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        clear_tracker_selection(&mut this.selected);
                                        this.selection_changed();
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .child({
                        let label = SharedString::from(tr("devices.roster.refresh"));
                        Button::new("enumerate")
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .on_click(cx.listener(|this, _, _, cx| this.rediscover_inventory(cx)))
                    }),
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
