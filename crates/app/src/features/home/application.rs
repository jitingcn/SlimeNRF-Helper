use super::{
    FeaturePage, Workbench,
    components::{action_card, context_button},
    page_scroll::page_scroll,
};
use crate::components::group_panel;
use crate::features::home::readiness::Status;
use crate::i18n::tr;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon,
    button::{Button, ButtonVariants},
};
use gpui_kit::{
    AnyElement, App, Context, Div, FontWeight, SharedString, Window, div, prelude::*, rems,
};

#[derive(Clone, Copy, Default)]
pub(super) enum HomeView {
    #[default]
    Tasks,
    Guide,
    Firmware,
    Help,
    Commands,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum GuideStep {
    #[default]
    Prepare,
    Receiver,
    Trackers,
}

impl GuideStep {
    const ALL: [Self; 3] = [Self::Prepare, Self::Receiver, Self::Trackers];

    fn title_key(self) -> &'static str {
        match self {
            Self::Prepare => "home.guide.prepare.title",
            Self::Receiver => "home.guide.receiver.title",
            Self::Trackers => "home.guide.trackers.title",
        }
    }
}

#[derive(Default)]
pub(super) struct HomeState {
    view: HomeView,
    step: GuideStep,
}

impl HomeState {
    #[cfg(test)]
    pub(super) fn view(&self) -> HomeView {
        self.view
    }
}

impl Workbench {
    pub(super) fn show_home_view(
        &mut self,
        view: HomeView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.set_feature_page(FeaturePage::Home, window, cx) {
            return false;
        }
        self.home.view = view;
        self.home_page_focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn home_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.home.view {
            HomeView::Tasks => self.home_tasks(cx),
            HomeView::Guide => self.home_guide(cx),
            HomeView::Firmware => self.home_firmware(cx),
            HomeView::Help => self.home_help(cx),
            HomeView::Commands => self.saved_commands_page(cx),
        };
        page_scroll(
            "home-page",
            &self.home_page_focus,
            div()
                .w_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .items_center()
                .p_4()
                .child(
                    div()
                        .w_full()
                        .max_w(rems(60.))
                        .min_w_0()
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .gap_5()
                        .child(content),
                ),
        )
        .into_any_element()
    }

    #[inline(never)]
    fn home_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(home_heading("home.title", "home.subtitle", cx))
                    .child(
                        Button::new("home-manage-commands")
                            .ghost()
                            .icon(IconName::SquareTerminal)
                            .label(tr("home.action.manage_commands"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_saved_commands(window, cx);
                            })),
                    )
                    .child(
                        Button::new("home-settings")
                            .ghost()
                            .icon(IconName::Settings2)
                            .label(tr("home.action.settings"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.set_feature_page(FeaturePage::Settings, window, cx) {
                                    this.focus_settings(window, cx);
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(self.home_device_summary(cx))
            .child(self.home_primary_tasks(cx))
            .when(self.has_pinned_saved_commands(), |page| {
                page.child(self.saved_home_commands(cx))
            })
            .child(self.home_more_tasks(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn home_primary_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .items_stretch()
            .gap_4()
            .child(
                div().flex_1().min_w_72().child(
                    home_task_card(
                        "home-connect",
                        "home.card.connect.title",
                        "home.card.connect.description",
                        IconName::Plug,
                        true,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_home_view(HomeView::Guide, window, cx);
                    })),
                ),
            )
            .child(
                div().flex_1().min_w_72().child(
                    home_task_card(
                        "home-firmware",
                        "home.card.firmware.title",
                        "home.card.firmware.description",
                        IconName::HardDrive,
                        true,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_home_view(HomeView::Firmware, window, cx);
                    })),
                ),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn home_more_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("home.section.more")),
            )
            .child(
                home_task_card(
                    "home-devices",
                    "home.card.devices.title",
                    "home.card.devices.description",
                    IconName::SquareTerminal,
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.set_feature_page(FeaturePage::DeviceSetup, window, cx) {
                        this.page_focus.focus(window, cx);
                        cx.notify();
                    }
                })),
            )
            .child(
                home_task_card(
                    "home-help",
                    "home.card.help.title",
                    "home.card.help.description",
                    IconName::BookOpen,
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.show_home_view(HomeView::Help, window, cx);
                })),
            )
            .into_any_element()
    }

    fn home_device_summary(&self, cx: &App) -> AnyElement {
        let readiness = self.home_readiness();
        let receiver_count = readiness.receiver_count();
        let active_trackers = readiness.active_trackers();
        let status = readiness.status();
        let status_key = match status {
            Status::Checking => "home.status.checking",
            Status::DiscoveryUnavailable => "home.status.unavailable",
            Status::NoReceiver => "home.status.no_receiver",
            Status::ReaderUnavailable => "home.status.reader_unavailable",
            Status::Busy => "home.status.busy",
            Status::WaitingForData => "home.status.waiting",
            Status::Receiving => "home.status.receiving",
        };
        let status_color = match status {
            Status::DiscoveryUnavailable | Status::ReaderUnavailable => cx.theme().warning,
            Status::Receiving => cx.theme().primary,
            _ => cx.theme().muted_foreground,
        };
        group_panel(cx)
            .gap_2()
            .child(
                div().flex().flex_wrap().gap_x_5().gap_y_1().children(
                    [
                        ("home.devices.receivers", receiver_count),
                        ("home.devices.trackers", active_trackers),
                    ]
                    .into_iter()
                    .map(|(label, count)| {
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .text_sm()
                            .child(tr(label))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                count.map_or_else(|| "—".to_owned(), |count| count.to_string()),
                            ))
                    }),
                ),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(status_color)
                    .child(tr(status_key)),
            )
            .into_any_element()
    }

    fn home_back_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("home-back-tasks")
            .ghost()
            .icon(IconName::ArrowLeft)
            .label(tr("home.action.back_home"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_home_view(HomeView::Tasks, window, cx);
            }))
    }

    fn home_devices_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .label(tr("home.action.open_devices"))
            .on_click(cx.listener(|this, _, window, cx| {
                if this.set_feature_page(FeaturePage::DeviceSetup, window, cx) {
                    this.page_focus.focus(window, cx);
                    cx.notify();
                }
            }))
    }

    #[inline(never)]
    fn home_guide(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut steps = div().flex().flex_wrap().gap_2().min_w_0();
        for (index, step) in GuideStep::ALL.into_iter().enumerate() {
            let title = format!("{}. {}", index + 1, tr(step.title_key()));
            steps = steps.child(
                context_button(
                    Button::new(("home-guide-step", index))
                        .h_auto()
                        .py_2()
                        .whitespace_normal()
                        .accessibility_label(title.clone())
                        .child(div().min_w_0().whitespace_normal().child(title)),
                    self.home.step == step,
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.show_home_view(HomeView::Guide, window, cx) {
                        this.home.step = step;
                    }
                })),
            );
        }
        let instruction_keys = match self.home.step {
            GuideStep::Prepare => [
                "home.guide.prepare.receiver",
                "home.guide.prepare.trackers",
                "home.guide.prepare.firmware",
            ],
            GuideStep::Receiver => [
                "home.guide.receiver.connect",
                "home.guide.receiver.explain",
                "home.guide.receiver.missing",
            ],
            GuideStep::Trackers => [
                "home.guide.trackers.power",
                "home.guide.trackers.inspect",
                "home.guide.trackers.pairing",
            ],
        };
        let body = group_panel(cx)
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(self.home.step.title_key())),
            )
            .children(
                instruction_keys
                    .into_iter()
                    .enumerate()
                    .map(|(index, key)| {
                        div()
                            .flex()
                            .items_start()
                            .min_w_0()
                            .gap_3()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{}.", index + 1)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_sm()
                                    .child(tr(key)),
                            )
                    }),
            );
        let forward = if self.home.step == GuideStep::Trackers {
            self.home_devices_button("home-guide-finish", cx)
        } else {
            Button::new("home-guide-next")
                .label(tr("home.action.next"))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.show_home_view(HomeView::Guide, window, cx) {
                        this.home.step = match this.home.step {
                            GuideStep::Prepare => GuideStep::Receiver,
                            GuideStep::Receiver | GuideStep::Trackers => GuideStep::Trackers,
                        };
                    }
                }))
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_4()
            .child(div().child(self.home_back_button(cx)))
            .child(home_heading("home.guide.title", "home.guide.subtitle", cx))
            .child(self.home_device_summary(cx))
            .child(steps)
            .child(body)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        Button::new("home-guide-back")
                            .icon(IconName::ArrowLeft)
                            .label(tr("common.action.back"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let (view, step) = match this.home.step {
                                    GuideStep::Prepare => (HomeView::Tasks, GuideStep::Prepare),
                                    GuideStep::Receiver => (HomeView::Guide, GuideStep::Prepare),
                                    GuideStep::Trackers => (HomeView::Guide, GuideStep::Receiver),
                                };
                                if this.show_home_view(view, window, cx) {
                                    this.home.step = step;
                                }
                            })),
                    )
                    .child(
                        forward
                            .h_12()
                            .min_w(rems(11.))
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .icon(IconName::ArrowRight)
                            .primary()
                            .when(!cx.theme().is_dark(), |button| button.outline()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when(self.home.step != GuideStep::Trackers, |row| {
                        row.child(self.home_devices_button("home-guide-open-devices", cx))
                    })
                    .child(
                        Button::new("home-guide-help")
                            .ghost()
                            .label(tr("home.action.help"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_home_view(HomeView::Help, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn home_firmware(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_4()
            .child(div().child(self.home_back_button(cx)))
            .child(home_heading(
                "home.firmware.title",
                "home.firmware.subtitle",
                cx,
            ))
            .child(
                home_task_card(
                    "home-firmware-wired",
                    "home.firmware.wired.title",
                    "home.firmware.wired.description",
                    IconName::Usb,
                    true,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.set_feature_page(FeaturePage::Uf2, window, cx) {
                        this.page_focus.focus(window, cx);
                        this.ensure_flash_catalog();
                        cx.notify();
                    }
                })),
            )
            .child(
                home_task_card(
                    "home-firmware-wireless",
                    "home.firmware.wireless.title",
                    "home.firmware.wireless.description",
                    IconName::Wifi,
                    true,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.set_feature_page(FeaturePage::Ota, window, cx) {
                        this.page_focus.focus(window, cx);
                        this.ensure_flash_catalog();
                        cx.notify();
                    }
                })),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("home.firmware.help")),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn home_help(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut topics = div().flex().flex_col().min_w_0().gap_3();
        for (title, description) in [
            ("home.help.receiver.title", "home.help.receiver.description"),
            ("home.help.trackers.title", "home.help.trackers.description"),
            ("home.help.serial.title", "home.help.serial.description"),
            ("home.help.direct.title", "home.help.direct.description"),
        ] {
            topics = topics.child(home_help_topic(title, description, cx));
        }
        #[cfg(target_os = "linux")]
        {
            topics = topics.child(home_help_topic(
                "home.help.permissions.title",
                "home.help.permissions.description",
                cx,
            ));
        }
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_4()
            .child(div().child(self.home_back_button(cx)))
            .child(home_heading("home.help.title", "home.help.subtitle", cx))
            .child(topics)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.home_devices_button("home-help-devices", cx))
                    .child(
                        Button::new("home-help-guide")
                            .label(tr("home.guide.title"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_home_view(HomeView::Guide, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn home_heading(title: &str, subtitle: &str, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w_64()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_normal()
                .child(tr(title)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(tr(subtitle)),
        )
}

fn home_help_topic(title: &str, description: &str, cx: &App) -> Div {
    group_panel(cx)
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_normal()
                .child(tr(title)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(tr(description)),
        )
}

#[inline(never)]
fn home_task_card(
    id: &'static str,
    title_key: &str,
    description_key: &str,
    icon: IconName,
    prominent: bool,
    cx: &App,
) -> Button {
    let title: SharedString = tr(title_key).into();
    action_card(id.into(), true, cx)
        .accessibility_label(title.clone())
        .when(prominent, |button| button.p_5().min_h_40())
        .child(
            div()
                .w_full()
                .min_w_0()
                .h_auto()
                .flex()
                .items_start()
                .gap_3()
                .whitespace_normal()
                .when(prominent, |row| row.flex_col().gap_4())
                .child(
                    div().flex_shrink_0().text_color(cx.theme().primary).child(
                        Icon::new(icon)
                            .size_5()
                            .when(prominent, |icon| icon.size_7()),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_auto()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_left()
                        .whitespace_normal()
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .font_weight(FontWeight::SEMIBOLD)
                                .when(prominent, |title| title.text_lg())
                                .when(!prominent, |title| title.text_sm())
                                .child(title),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr(description_key)),
                        ),
                ),
        )
}
