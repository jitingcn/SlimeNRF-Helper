use super::{FeaturePage, Workbench, components::context_button};
use crate::i18n::tr;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable,
    button::{Button, ButtonVariant, ButtonVariants},
};
use gpui_kit::{self as gpui, AnyElement, Context, Window, div, prelude::*};

impl Workbench {
    pub(super) fn feature_navigation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use gpui::assets::IconName;

        let collapsed = self.navigation_collapsed;
        // Keep navigation stable across pages and reserve the device pane's initial widths.
        // Root p_2 on both sides and gap_2 account for the remaining 1.5 rem.
        let available_width = window.viewport_size().width
            - window.rem_size()
                * (super::DEVICE_WORKSPACE_MIN_WIDTH_REM + super::COMMAND_SIDEBAR_WIDTH_REM + 1.5);
        let width = if collapsed {
            window.rem_size() * 3.25
        } else {
            available_width.clamp(window.rem_size() * 12., window.rem_size() * 14.5)
        };
        let navigation_focus = window
            .use_keyed_state("feature-navigation-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(false)
            })
            .read(cx)
            .clone();
        let item = |id: &'static str, label: &'static str, icon: IconName| {
            Button::new(id)
                .small()
                .h_8()
                .flex_shrink_0()
                .w_full()
                .ghost()
                .when(!cx.theme().is_dark(), |button| {
                    button.with_variant(ButtonVariant::Default)
                })
                .accessibility_label(tr(label))
                .tooltip(tr(label))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .when(collapsed, |row| row.justify_center())
                        .child(div().flex_shrink_0().child(Icon::new(icon).small()))
                        .when(!collapsed, |row| {
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_left()
                                    .truncate()
                                    .child(tr(label)),
                            )
                        }),
                )
        };
        let mut entries = div()
            .id("feature-navigation-scroll")
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_2()
            .child(
                item("nav-home", "app.navigation.home", IconName::House)
                    .map(|button| context_button(button, self.active_page == FeaturePage::Home, cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::Home);
                        this.home_page_focus.focus(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                item(
                    "nav-device-setup",
                    "app.navigation.device_setup",
                    IconName::SquareTerminal,
                )
                .map(|button| {
                    context_button(button, self.active_page == FeaturePage::DeviceSetup, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_feature_page(FeaturePage::DeviceSetup);
                    this.page_focus.focus(window, cx);
                    cx.notify();
                })),
            );
        entries = entries.child(
            div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .min_w_0()
                .gap_1()
                .child(
                    item(
                        "nav-uf2",
                        "app.navigation.firmware_update",
                        IconName::HardDrive,
                    )
                    .map(|button| context_button(button, self.active_page == FeaturePage::Uf2, cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::Uf2);
                        this.page_focus.focus(window, cx);
                        this.ensure_flash_catalog();
                        cx.notify();
                    })),
                )
                .child(
                    item(
                        "nav-esb-ota",
                        "app.navigation.wireless_update",
                        IconName::Wifi,
                    )
                    .map(|button| context_button(button, self.active_page == FeaturePage::Ota, cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::Ota);
                        this.page_focus.focus(window, cx);
                        this.ensure_flash_catalog();
                        cx.notify();
                    })),
                ),
        );
        entries = entries.child(
            div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .min_w_0()
                .gap_1()
                .child(
                    item(
                        "nav-raw-data",
                        "app.navigation.raw_capture",
                        IconName::Activity,
                    )
                    .map(|button| {
                        context_button(button, self.active_page == FeaturePage::RawCapture, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::RawCapture);
                        this.page_focus.focus(window, cx);
                        this.refresh_capture_sources(cx);
                        cx.notify();
                    })),
                )
                .child(
                    item(
                        "nav-vqf-replay",
                        "app.navigation.vqf_replay",
                        IconName::Play,
                    )
                    .map(|button| {
                        context_button(button, self.active_page == FeaturePage::VqfReplay, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::VqfReplay);
                        this.page_focus.focus(window, cx);
                        cx.notify();
                    })),
                )
                .child(
                    item(
                        "nav-hid-recording",
                        "app.navigation.hid_events",
                        IconName::FileText,
                    )
                    .map(|button| {
                        context_button(button, self.active_page == FeaturePage::HidLog, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_feature_page(FeaturePage::HidLog);
                        this.page_focus.focus(window, cx);
                        cx.notify();
                    })),
                ),
        );
        entries = entries.child(
            item("nav-tools", "app.navigation.tools", IconName::Wrench)
                .map(|button| context_button(button, self.active_page == FeaturePage::Tools, cx))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_feature_page(FeaturePage::Tools);
                    this.tools_page_focus.focus(window, cx);
                    cx.notify();
                })),
        );
        for items in [&[
            (
                "nav-temperature",
                "app.navigation.temperature_calibration",
                IconName::Thermometer,
                "app.navigation.temperature_calibration_unavailable",
            ),
            (
                "nav-gyroscope",
                "app.navigation.gyro_calibration",
                IconName::Gauge,
                "app.navigation.gyro_calibration_unavailable",
            ),
            (
                "nav-accelerometer",
                "app.navigation.accelerometer_calibration",
                IconName::Axis3d,
                "app.navigation.accelerometer_calibration_unavailable",
            ),
            (
                "nav-magnetometer",
                "app.navigation.magnetometer_calibration",
                IconName::Compass,
                "app.navigation.magnetometer_calibration_unavailable",
            ),
        ] as &[(&str, &str, IconName, &str)]]
        {
            let mut group = div().flex().flex_col().flex_shrink_0().min_w_0().gap_1();
            for (id, label, icon, tooltip) in items {
                group = group.child(item(id, label, *icon).map(|button| {
                    if *id == "nav-gyroscope" {
                        context_button(button, self.active_page == FeaturePage::GyroCalibration, cx)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_feature_page(FeaturePage::GyroCalibration);
                                this.calibration_page_focus.focus(window, cx);
                                cx.notify();
                            }))
                    } else {
                        button
                            .ghost()
                            .text_color(cx.theme().muted_foreground)
                            .tooltip(tr(tooltip))
                            .disabled(true)
                            .tab_stop(false)
                    }
                }));
            }
            entries = entries.child(group);
        }
        div()
            .w(width)
            .h_full()
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(super::page_scroll::page_scroll(
                "navigation-scroll",
                &navigation_focus,
                entries,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .min_w_0()
                    .px_2()
                    .pb_2()
                    .child(
                        item("nav-settings", "app.settings.title", IconName::Settings2)
                            .map(|button| {
                                context_button(
                                    button,
                                    self.active_page == FeaturePage::Settings,
                                    cx,
                                )
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_feature_page(FeaturePage::Settings);
                                this.focus_settings(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        item(
                            "nav-toggle",
                            if collapsed {
                                "app.navigation.expand"
                            } else {
                                "app.navigation.collapse"
                            },
                            if collapsed {
                                IconName::PanelLeftOpen
                            } else {
                                IconName::PanelLeftClose
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigation_collapsed = !this.navigation_collapsed;
                            cx.notify();
                        })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        AppContext as _, Bounds, Role, TestAppContext, WindowBounds, WindowOptions, px, size,
        test::TestWindowExt as _,
    };

    #[gpui_kit::test]
    fn sidebar_navigation_enters_the_page_before_tabbing_to_its_first_control(
        cx: &mut TestAppContext,
    ) {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        let executor = runtime.executor();

        let (handle, workbench) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1320.), px(820.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        crate::ui::test_support::offline_workbench(
                            window,
                            cx,
                            crate::sessions::SessionManager::new(),
                            executor,
                        )
                    })
                },
            )
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("nav-settings", cx);
            assert!(workbench.read(cx).page_focus.is_focused(window));
            window.press("tab", cx);
            assert_eq!(window.find("settings-search").focused(), Some(true));
            window.click("nav-device-setup", cx);
            assert!(workbench.read(cx).page_focus.is_focused(window));
            window.press("tab", cx);
            let manage = window.find("roster-manage-devices");
            assert_eq!(manage.focused(), Some(true));
            assert!(manage.visible());
            assert_eq!(manage.role(), Some(Role::Button));
            assert_eq!(
                manage.label(),
                Some(crate::i18n::tr("devices.registry.action.manage").as_str()),
            );
            assert!(window.try_find("clear-tracker-selection").is_none());
            window.press("tab", cx);
            let refresh = window.find("enumerate");
            assert_eq!(refresh.focused(), Some(true));
            assert_eq!(refresh.role(), Some(Role::Button));
            assert_eq!(
                refresh.label(),
                Some(crate::i18n::tr("devices.roster.refresh").as_str()),
            );
        })
        .unwrap();

        let receiver = crate::domain::ReceiverEndpoint {
            key: "synthetic-summary-receiver".into(),
            path: b"synthetic-summary-receiver".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some("synthetic-summary-serial".into()),
        };
        workbench.update(cx, |this, cx| {
            this.inventory.receivers.push(receiver.clone());
            this.monitor =
                crate::discovery::ReceiverMonitor::fixture(receiver, 0, "010203040506", true);
            this.selected
                .insert("tracker:synthetic-summary-receiver:0".into());
            this.selected.insert("port:synthetic-summary-port".into());
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("shift-tab", cx);
            let clear = window.find("clear-tracker-selection");
            assert_eq!(clear.focused(), Some(true));
            assert_eq!(clear.role(), Some(Role::Button));
            assert_eq!(
                clear.label(),
                Some(crate::i18n::tr("common.action.clear_selection").as_str()),
            );
            window.press("enter", cx);
            assert!(window.try_find("clear-tracker-selection").is_none());
        })
        .unwrap();
        workbench.read_with(cx, |this, _| {
            assert_eq!(
                this.selected,
                ["port:synthetic-summary-port".to_owned()]
                    .into_iter()
                    .collect(),
            );
        });
    }
}
