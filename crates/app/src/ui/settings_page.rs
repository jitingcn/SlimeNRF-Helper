use super::{
    Workbench,
    command_panel::{CommandPreparation, PendingCommand, PrepareAction},
    page_scroll::{ControlFocus, FocusReveal, reveal_on_focus},
    theme,
};
use crate::{
    i18n::tr,
    settings::{AccentColor, Appearance, Language, Preferences},
};
use gpui_kit::base::{ElementExt as _, Link, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, ResizableState, Selectable, Sizable, Size,
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_resizable,
    input::{Input, InputEvent, InputState, MovePageDown, MovePageUp},
    resizable_panel,
    scroll::Scrollbar,
    switch::Switch,
};
use gpui_kit::{
    AnyElement, App, Context, Entity, Focusable as _, FontWeight, ScrollAnchor, ScrollHandle,
    Subscription, Window, div, prelude::*, rems,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Section {
    #[default]
    All,
    General,
    Appearance,
    Devices,
    About,
}

impl Section {
    const GROUPS: [Self; 4] = [Self::General, Self::Appearance, Self::Devices, Self::About];

    fn id(self) -> &'static str {
        match self {
            Self::All => "settings-section-all",
            Self::General => "settings-section-general",
            Self::Appearance => "settings-section-appearance",
            Self::Devices => "settings-section-devices",
            Self::About => "settings-section-about",
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            Self::All => "app.settings.all",
            Self::General => "app.settings.section.general",
            Self::Appearance => "app.settings.section.appearance",
            Self::Devices => "app.settings.section.devices",
            Self::About => "app.settings.section.about",
        }
    }

    fn group_ix(self) -> Option<usize> {
        Self::GROUPS.iter().position(|section| *section == self)
    }
}

pub(super) struct SettingsUiState {
    search: Entity<InputState>,
    selected: Section,
    scroll: ScrollHandle,
    navigation_scroll: ScrollHandle,
    anchors: [ScrollAnchor; 4],
    layout: Entity<ResizableState>,
    control_focus: [Option<ControlFocus>; 8],
    repository_focus: [ControlFocus; 4],
    _search_subscription: Subscription,
}

impl SettingsUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workbench>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("app.settings.search.placeholder"))
        });
        let subscription = cx.subscribe_in(&search, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.settings.scroll.set_offset(Default::default());
                cx.notify();
            }
        });
        let scroll = ScrollHandle::new();
        Self {
            search,
            selected: Section::All,
            anchors: std::array::from_fn(|_| ScrollAnchor::for_handle(scroll.clone())),
            control_focus: std::array::from_fn(|ix| {
                (!matches!(
                    Setting::ALL[ix],
                    Setting::Language
                        | Setting::Appearance
                        | Setting::Accent
                        | Setting::Version
                        | Setting::Repositories
                ))
                .then(|| ControlFocus::new(window, cx))
            }),
            repository_focus: std::array::from_fn(|_| ControlFocus::new(window, cx)),
            navigation_scroll: ScrollHandle::new(),
            scroll,
            layout: cx.new(|_| ResizableState::default()),
            _search_subscription: subscription,
        }
    }
}

#[derive(Clone, Copy)]
enum Setting {
    Language,
    Appearance,
    Accent,
    Registry,
    Advanced,
    Version,
    Repositories,
    Notices,
}

impl Setting {
    const ALL: [Self; 8] = [
        Self::Language,
        Self::Appearance,
        Self::Accent,
        Self::Registry,
        Self::Advanced,
        Self::Version,
        Self::Repositories,
        Self::Notices,
    ];

    fn section(self) -> Section {
        match self {
            Self::Language => Section::General,
            Self::Appearance | Self::Accent => Section::Appearance,
            Self::Registry | Self::Advanced => Section::Devices,
            Self::Version | Self::Repositories | Self::Notices => Section::About,
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            Self::Language => "app.settings.language.title",
            Self::Appearance => "app.settings.appearance.title",
            Self::Accent => "app.settings.accent.title",
            Self::Registry => "devices.registry.title",
            Self::Advanced => "app.settings.advanced.title",
            Self::Version => "app.settings.version.title",
            Self::Repositories => "app.settings.repositories.title",
            Self::Notices => "app.settings.notices.title",
        }
    }

    fn hint_key(self, locked: bool) -> &'static str {
        match self {
            Self::Language => "app.settings.language.hint",
            Self::Appearance => "app.settings.appearance.hint",
            Self::Accent => "app.settings.accent.hint",
            Self::Registry => "devices.registry.hint",
            Self::Advanced if locked => "app.settings.advanced.locked_hint",
            Self::Advanced => "app.settings.advanced.hint",
            Self::Version => "app.settings.version.hint",
            Self::Repositories => "app.settings.repositories.hint",
            Self::Notices => "app.settings.notices.hint",
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Language => &["language", "english", "chinese", "中文", "简体"],
            Self::Appearance => &["theme", "light", "dark", "主题", "浅色", "深色"],
            Self::Accent => &[
                "color", "neutral", "blue", "green", "purple", "amber", "sakura", "颜色", "中性",
                "蓝色", "绿色", "紫色", "琥珀", "樱花",
            ],
            Self::Registry => &["registry", "nickname", "usb", "receiver", "tracker", "昵称"],
            Self::Advanced => &[
                "advanced",
                "bluetooth",
                "sha-256",
                "bootloader",
                "高级",
                "蓝牙",
            ],
            Self::Version => &["version", "build", "slimenrf", "版本"],
            Self::Repositories => &[
                "github",
                "repository",
                "source",
                "application",
                "receiver",
                "tracker",
                "firmware",
                "bootloader",
                "仓库",
                "源码",
                "应用",
                "接收器",
                "追踪器",
                "固件",
                "引导程序",
            ],
            Self::Notices => &[
                "license",
                "licence",
                "notices",
                "third-party",
                "许可证",
                "声明",
            ],
        }
    }

    fn matches(self, query: &str, locked: bool) -> bool {
        query.is_empty()
            || tr(self.title_key()).to_lowercase().contains(query)
            || tr(self.hint_key(locked)).to_lowercase().contains(query)
            || tr(self.section().title_key())
                .to_lowercase()
                .contains(query)
            || self
                .keywords()
                .iter()
                .any(|keyword| keyword.contains(query))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsChoice {
    Language(Language),
    Appearance(Appearance),
    Accent(AccentColor),
}

impl SettingsChoice {
    fn label_key(self) -> &'static str {
        match self {
            Self::Language(value) => value.label_key(),
            Self::Appearance(value) => value.label_key(),
            Self::Accent(value) => value.label_key(),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Language(Language::English) => "settings-language-english",
            Self::Language(Language::SimplifiedChinese) => "settings-language-simplified-chinese",
            Self::Appearance(Appearance::Light) => "settings-appearance-light",
            Self::Appearance(Appearance::Dark) => "settings-appearance-dark",
            Self::Accent(AccentColor::Neutral) => "settings-accent-neutral",
            Self::Accent(AccentColor::Blue) => "settings-accent-blue",
            Self::Accent(AccentColor::Green) => "settings-accent-green",
            Self::Accent(AccentColor::Purple) => "settings-accent-purple",
            Self::Accent(AccentColor::Amber) => "settings-accent-amber",
            Self::Accent(AccentColor::Sakura) => "settings-accent-sakura",
        }
    }

    fn apply(self, preferences: &mut Preferences) {
        match self {
            Self::Language(value) => preferences.language = value,
            Self::Appearance(value) => preferences.appearance = value,
            Self::Accent(value) => preferences.accent = value,
        }
    }
}

fn setting_row(
    setting: Setting,
    locked: bool,
    control: AnyElement,
    separated: bool,
    focus: Option<FocusReveal>,
    scroll: &ScrollHandle,
    cx: &App,
) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap_4()
        .when(separated, |row| {
            row.pb_4().border_b_1().border_color(cx.theme().border)
        })
        .child(
            div()
                .flex_1()
                .flex_basis(rems(18.))
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child(tr(setting.title_key())))
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(setting.hint_key(locked))),
                ),
        )
        .child(
            div()
                .relative()
                .when_some(focus, |control, focus| {
                    let scroll = scroll.clone();
                    control
                        .track_focus(focus.scope())
                        .on_prepaint(move |bounds, window, cx| {
                            focus.reveal(bounds, &scroll, window, cx)
                        })
                })
                .when(
                    matches!(
                        setting,
                        Setting::Language
                            | Setting::Appearance
                            | Setting::Accent
                            | Setting::Repositories
                    ),
                    |control| control.w(rems(24.)),
                )
                .when(
                    !matches!(
                        setting,
                        Setting::Language
                            | Setting::Appearance
                            | Setting::Accent
                            | Setting::Repositories
                    ),
                    |control| control.w_48(),
                )
                .max_w_full()
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(control),
        )
}

impl Workbench {
    fn refresh_input_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.search.update(cx, |input, cx| {
            input.set_placeholder(tr("app.settings.search.placeholder"), window, cx);
        });
        self.batch_input.update(cx, |input, cx| {
            input.set_placeholder(tr("serial.terminal.command_placeholder.batch"), window, cx);
        });
        for terminal in self.terminals.values() {
            terminal.input.update(cx, |input, cx| {
                input.set_placeholder(tr("serial.terminal.command_placeholder.single"), window, cx);
            });
        }
        let parameter = match &self.sidebar.pending {
            Some(PendingCommand::Parameter { preset, input, .. }) => Some((*preset, input)),
            Some(PendingCommand::Connecting(CommandPreparation {
                action:
                    PrepareAction::Preset {
                        preset,
                        input: Some(input),
                    },
                ..
            })) => Some((*preset, input)),
            _ => None,
        };
        if let Some((preset, input)) = parameter {
            input.update(cx, |input, cx| {
                input.set_placeholder(tr(preset.parameter.hint()), window, cx);
            });
        }
        self.refresh_replay_input_placeholders(window, cx);
    }

    fn refresh_pending_status(&mut self) {
        // Prepared target captions stay with their frozen command facts.
        for (port, terminal) in &mut self.terminals {
            if terminal
                .pending
                .as_ref()
                .is_some_and(|pending| pending.error.is_none())
            {
                terminal.status = Some(
                    rust_i18n::t!("serial.terminal.status.connecting", port = port).to_string(),
                );
            }
        }
    }

    fn update_preferences(
        &mut self,
        preferences: Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down || self.preferences == preferences {
            return;
        }
        let advanced_changed = self.preferences.advanced_features != preferences.advanced_features;
        if advanced_changed
            && !self
                .flash
                .prepare_advanced_features(preferences.advanced_features)
        {
            return;
        }
        let language_changed = self.preferences.language != preferences.language;
        self.preferences = preferences;
        if advanced_changed {
            self.sync_esb_page();
        }
        theme::apply(&self.preferences, cx);
        if language_changed {
            self.refresh_pending_status();
            self.refresh_input_placeholders(window, cx);
            self.refresh_temperature_locale(cx);
            self.refresh_rssi_locale(cx);
            self.refresh_notices_locale(cx);
            self.scan_inventory(cx);
        }
        self.save_preferences(cx);
    }

    pub(super) fn focus_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.registry_is_open() {
            self.open_device_registry(window, cx);
        } else {
            self.page_focus.focus(window, cx);
        }
    }

    pub(super) fn settings_page(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.registry_is_open() {
            return self.device_registry_page(cx);
        }
        let query = self.settings.search.read(cx).value().trim().to_lowercase();
        let locked = self.flash.advanced_features_locked();
        let matching = Setting::ALL.map(|setting| setting.matches(&query, locked));
        let mut navigation = div().w_full().min_w_0().flex().flex_col().gap_3().child(
            Input::new(&self.settings.search)
                .id("settings-search")
                .aria_label(tr("app.settings.search.placeholder"))
                .prefix(IconName::Search)
                .cleanable(true),
        );
        for section in [
            Section::All,
            Section::General,
            Section::Appearance,
            Section::Devices,
            Section::About,
        ] {
            let available = section == Section::All
                || Setting::ALL
                    .iter()
                    .enumerate()
                    .any(|(ix, setting)| setting.section() == section && matching[ix]);
            navigation = navigation.child(reveal_on_focus(
                section.id(),
                Button::new(section.id())
                    .label(tr(section.title_key()))
                    .w_full()
                    .ghost()
                    .justify_start()
                    .toggled(self.settings.selected == section)
                    .selected(self.settings.selected == section)
                    .disabled(!available)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.settings.selected = section;
                        if let Some(ix) = section.group_ix() {
                            this.settings.anchors[ix].scroll_to(window, cx);
                        } else {
                            this.settings.scroll.set_offset(Default::default());
                        }
                        this.page_focus.focus(window, cx);
                        cx.notify();
                    })),
                &self.settings.navigation_scroll,
            ));
        }
        let sidebar = div()
            .id("settings-sidebar")
            .size_full()
            .min_h_0()
            .relative()
            .bg(cx.theme().tokens.sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                div()
                    .id("settings-navigation-scroll")
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.settings.navigation_scroll)
                    .child(navigation.p_3()),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&self.settings.navigation_scroll)),
            );
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_6()
            .p_4();
        if matching.iter().any(|matched| *matched) {
            for (group_ix, section) in Section::GROUPS.into_iter().enumerate() {
                let mut items = Setting::ALL
                    .into_iter()
                    .enumerate()
                    .filter(|(ix, setting)| setting.section() == section && matching[*ix])
                    .peekable();
                if items.peek().is_none() {
                    continue;
                }
                let mut group = GroupBox::new().id(section.id()).fill().title(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(tr(section.title_key())),
                );
                while let Some((ix, setting)) = items.next() {
                    group = group.child(setting_row(
                        setting,
                        locked,
                        self.settings_control(setting, cx),
                        items.peek().is_some(),
                        self.settings.control_focus[ix]
                            .as_ref()
                            .map(ControlFocus::reveal),
                        &self.settings.scroll,
                        cx,
                    ));
                }
                content = content.child(
                    div()
                        .id((gpui_kit::ElementId::from(section.id()), "anchor"))
                        .role(gpui_kit::Role::Group)
                        .aria_label(tr(section.title_key()))
                        .test_support()
                        .anchor_scroll(Some(self.settings.anchors[group_ix].clone()))
                        .child(group),
                );
            }
        } else {
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_3()
                    .child(tr("app.settings.search.empty"))
                    .child(
                        Button::new("settings-clear-search")
                            .label(tr("app.settings.search.clear"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.settings
                                    .search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.settings.search.focus_handle(cx).focus(window, cx);
                            })),
                    ),
            );
        }
        let right = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("app.settings.all")),
                    )
                    .when(
                        self.preferences_io.load_error.is_none()
                            && self.preferences_io.save_error.is_none(),
                        |header| {
                            header.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr("app.settings.autosave_hint")),
                            )
                        },
                    ),
            )
            .child(
                super::page_scroll::page_scroll("settings-scroll", &self.page_focus, content)
                    .scroll_handle(&self.settings.scroll)
                    .focus_scope(false),
            );
        div()
            .id("settings-page")
            .track_focus(&self.page_focus)
            .on_action(|_: &MovePageUp, _, cx| cx.stop_propagation())
            .on_action(|_: &MovePageDown, _, cx| cx.stop_propagation())
            .on_key_down({
                let scroll = self.settings.scroll.clone();
                move |event, window, cx| {
                    super::page_scroll::handle_page_key(event, &scroll, window, cx)
                }
            })
            .border_1()
            .border_color(cx.theme().border.opacity(0.))
            .focus_visible(|style| style.border_color(cx.theme().ring))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .when(
                self.preferences_io.load_error.is_some()
                    || self.preferences_io.save_error.is_some(),
                |page| page.child(self.settings_save_status(cx)),
            )
            .child(
                h_resizable("settings-split")
                    .with_state(&self.settings.layout)
                    .with_handle_appearance(self.resize_handle_appearance.clone())
                    .child(
                        resizable_panel()
                            .size(window.rem_size() * 12.)
                            .size_range(window.rem_size() * 10. ..window.rem_size() * 16.)
                            .child(sidebar),
                    )
                    .child(resizable_panel().child(right)),
            )
            .into_any_element()
    }

    fn settings_choices<const N: usize>(
        &self,
        id: &'static str,
        title: &'static str,
        current: SettingsChoice,
        choices: [SettingsChoice; N],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // GPUI snaps fixed rem widths before layout, unlike fractional flex shares.
        // The shared width leaves room for three accent columns and their gaps.
        let slot_width = rems(if matches!(current, SettingsChoice::Language(_)) {
            11.
        } else {
            7.5
        });
        div()
            .id(id)
            .role(gpui_kit::Role::Group)
            .aria_label(tr(title))
            .test_support()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .children(choices.into_iter().map(|choice| {
                let selected = choice == current;
                let label = tr(choice.label_key());
                let button = Button::new(choice.id())
                    .with_size(Size::Medium)
                    .accessibility_label(label.clone())
                    .selected(selected)
                    .toggled(selected)
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when_some(
                                match choice {
                                    SettingsChoice::Accent(accent) => Some(theme::accent_primary(
                                        self.preferences.appearance,
                                        accent,
                                    )),
                                    SettingsChoice::Language(_) | SettingsChoice::Appearance(_) => {
                                        None
                                    }
                                },
                                |content, color| {
                                    content.child(
                                        div()
                                            .size_4()
                                            .flex_shrink_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .size_3()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(cx.theme().border)
                                                    .bg(color),
                                            ),
                                    )
                                },
                            )
                            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
                            .child(div().size_4().flex_shrink_0().when(selected, |slot| {
                                slot.child(Icon::new(IconName::Check).size_4())
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let mut preferences = this.preferences.clone();
                        choice.apply(&mut preferences);
                        this.update_preferences(preferences, window, cx);
                    }));
                div()
                    .w(slot_width)
                    .max_w_full()
                    .flex_shrink_0()
                    .min_w_0()
                    .child(reveal_on_focus(choice.id(), button, &self.settings.scroll))
            }))
            .into_any_element()
    }

    fn settings_control(&self, setting: Setting, cx: &mut Context<Self>) -> AnyElement {
        match setting {
            Setting::Language => self.settings_choices(
                "settings-language",
                setting.title_key(),
                SettingsChoice::Language(self.preferences.language),
                Language::ALL.map(SettingsChoice::Language),
                cx,
            ),
            Setting::Appearance => self.settings_choices(
                "settings-appearance",
                setting.title_key(),
                SettingsChoice::Appearance(self.preferences.appearance),
                Appearance::ALL.map(SettingsChoice::Appearance),
                cx,
            ),
            Setting::Accent => self.settings_choices(
                "settings-accent",
                setting.title_key(),
                SettingsChoice::Accent(self.preferences.accent),
                AccentColor::ALL.map(SettingsChoice::Accent),
                cx,
            ),
            Setting::Registry => Button::new("manage-devices")
                .label(tr("devices.registry.action.manage"))
                .w_full()
                .on_click(cx.listener(|this, _, window, cx| this.open_device_registry(window, cx)))
                .into_any_element(),
            Setting::Advanced => Switch::new("advanced-features")
                .accessibility_label(tr("app.settings.advanced.title"))
                .checked(self.preferences.advanced_features)
                .disabled(self.flash.advanced_features_locked())
                .on_change(cx.listener(|this, enabled, window, cx| {
                    let mut preferences = this.preferences.clone();
                    preferences.advanced_features = *enabled;
                    this.update_preferences(preferences, window, cx);
                }))
                .into_any_element(),
            Setting::Version => div()
                .text_sm()
                .child(env!("CARGO_PKG_VERSION"))
                .into_any_element(),
            Setting::Repositories => div()
                .id("settings-repositories")
                .role(gpui_kit::Role::Group)
                .aria_label(tr(setting.title_key()))
                .test_support()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .children(
                    [
                        (
                            "settings-repository-application",
                            "app.settings.repositories.application",
                            "https://github.com/jitingcn/SlimeNRF-Helper",
                        ),
                        (
                            "settings-repository-receiver",
                            "app.settings.repositories.receiver",
                            "https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver",
                        ),
                        (
                            "settings-repository-tracker",
                            "app.settings.repositories.tracker",
                            "https://github.com/jitingcn/SlimeVR-Tracker-nRF",
                        ),
                        (
                            "settings-repository-bootloader",
                            "app.settings.repositories.bootloader",
                            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader",
                        ),
                    ]
                    .into_iter()
                    .zip(&self.settings.repository_focus)
                    .map(|((id, label_key, url), focus)| {
                        let focus = focus.reveal();
                        let scroll = self.settings.scroll.clone();
                        let label = tr(label_key);
                        div()
                            .id(id)
                            .test_support()
                            .min_w_0()
                            .max_w_full()
                            .track_focus(focus.scope())
                            .on_prepaint(move |bounds, window, cx| {
                                focus.reveal(bounds, &scroll, window, cx)
                            })
                            .child(
                                Link::new("repository-link")
                                    .href(url)
                                    .accessibility_label(label.clone())
                                    .open_with(|url, _, _, cx| cx.open_url(url))
                                    .min_w_0()
                                    .max_w_full()
                                    .px_1()
                                    .py_1()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().link)
                                    .text_decoration_1()
                                    .text_decoration_color(cx.theme().link.opacity(0.5))
                                    .border_1()
                                    .border_color(cx.theme().border.opacity(0.))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style
                                            .text_color(cx.theme().link.opacity(0.8))
                                            .text_decoration_color(cx.theme().link)
                                    })
                                    .active(|style| style.text_color(cx.theme().link.opacity(0.6)))
                                    .focus_visible(|style| style.border_color(cx.theme().ring))
                                    .child(label),
                            )
                    }),
                )
                .into_any_element(),
            Setting::Notices => Button::new("open-notices")
                .label(tr("app.settings.notices.open"))
                .w_full()
                .on_click(cx.listener(|this, _, _, cx| this.open_notices_window(cx)))
                .into_any_element(),
        }
    }

    fn settings_save_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut status = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3();
        if let Some(error) = &self.preferences_io.load_error {
            status = status.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(
                        rust_i18n::t!("app.settings.error.load_unchanged", error = error)
                            .to_string(),
                    ),
            );
        }
        if let Some(error) = &self.preferences_io.save_error {
            let message =
                rust_i18n::t!("app.settings.error.session_only", error = error).to_string();
            status = status
                .child(
                    div()
                        .id("settings-save-error")
                        .role(gpui_kit::Role::Alert)
                        .test_support()
                        .aria_label(message.clone())
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
                .child(
                    Button::new("retry-save-preferences")
                        .label(tr("app.settings.action.retry_save"))
                        .on_click(cx.listener(|this, _, _, cx| this.save_preferences(cx))),
                );
        }
        status.into_any_element()
    }
}

#[cfg(test)]
mod tests;
