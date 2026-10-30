use super::{CommandScope, Workbench};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, Render, Role, TestAppContext, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size, test::TestWindowExt as _,
};

struct SidebarHarness {
    workbench: Entity<Workbench>,
}

impl Render for SidebarHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workbench.update(cx, |this, cx| {
            div()
                .size_full()
                .track_focus(&this.page_focus)
                .tab_group()
                .flex()
                .flex_col()
                .p_3()
                .gap_2()
                .child(gpui_kit::component::button::Button::new("sidebar-start").label("Commands"))
                .child(this.command_sidebar(cx))
        })
    }
}

#[gpui_kit::test]
fn sidebar_destinations_accept_tab_enter_and_space_without_replacing_owner_state(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated runtime");
    let executor = runtime.executor();
    let (window, harness) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(560.), px(720.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let workbench = cx.new(|cx| {
                    let mut this = crate::application::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.sidebar.scope = CommandScope::SerialBatch;
                    this.sidebar.toggle_group("commands.group.information");
                    this
                });
                let focus = workbench.read(cx).page_focus.clone();
                focus.focus(window, cx);
                cx.new(|_| SidebarHarness { workbench })
            },
        )
        .expect("open production command sidebar")
    });
    let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
    let search = cx.update(|cx| workbench.read(cx).command_search.entity_id());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("command-tab-commands").role(),
            Some(Role::Button)
        );
        assert_eq!(window.find("command-tab-commands").checked(), Some(true));
        assert_eq!(window.find("command-tab-history").checked(), Some(false));
        window.press("tab", cx);
        assert_eq!(window.find("sidebar-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("command-tab-commands").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("command-tab-history").focused(), Some(true));
        window.press("enter", cx);
    })
    .expect("activate history through its keyboard path");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(workbench.read(cx).sidebar.history);
        assert_eq!(window.find("command-tab-history").checked(), Some(true));
        window.press("shift-tab", cx);
        assert_eq!(window.find("command-tab-commands").focused(), Some(true));
        window.press("space", cx);
    })
    .expect("return to commands through its keyboard path");
    cx.run_until_parked();
    workbench.read_with(cx, |this, _| {
        assert!(!this.sidebar.history);
        assert_eq!(this.sidebar.scope, CommandScope::SerialBatch);
        assert!(
            this.sidebar
                .collapsed_groups
                .contains("commands.group.information")
        );
        assert_eq!(this.command_search.entity_id(), search);
        assert!(this.batch_history.is_empty());
        assert_eq!(this.manager.sessions().count(), 0);
    });
}
