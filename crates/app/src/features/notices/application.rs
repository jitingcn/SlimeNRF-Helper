use super::Workbench;
use crate::features::notices::window::NoticesWindow;
use crate::i18n::tr;
use gpui_kit::component::TitleBar;
use gpui_kit::{
    self as gpui, AnyWindowHandle, AppContext as _, Bounds, Context, Entity, Subscription,
    TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px, size,
};

#[derive(Default)]
pub(super) struct NoticesWindowState {
    open: Option<OpenNoticesWindow>,
}

struct OpenNoticesWindow {
    handle: AnyWindowHandle,
    content: Entity<NoticesWindow>,
    _close_observer: Subscription,
}

impl Workbench {
    pub(super) fn open_notices_window(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(open) = &self.notices.open {
            if open
                .handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.notices.open = None;
        }
        let bounds = Bounds::centered(None, size(px(960.), px(720.)), cx);
        match gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(tr("notices.window.title").into()),
                    ..TitleBar::title_bar_options()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(320.))),
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            },
            cx,
            |_, cx| cx.new(|_| NoticesWindow::new()),
        ) {
            Ok((handle, content)) => {
                let parent = cx.weak_entity();
                let close_observer = cx.on_window_closed(move |cx, closed_id| {
                    let parent = parent.clone();
                    cx.defer(move |cx| {
                        let _ = parent.update(cx, |this, _| {
                            if this
                                .notices
                                .open
                                .as_ref()
                                .is_some_and(|open| open.handle.window_id() == closed_id)
                            {
                                this.notices.open = None;
                            }
                        });
                    });
                });
                self.notices.open = Some(OpenNoticesWindow {
                    handle,
                    content,
                    _close_observer: close_observer,
                });
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(error) => {
                self.error = Some(format!("{}: {error}", tr("notices.error.open")).into());
                cx.notify();
            }
        }
    }

    pub(super) fn refresh_notices_locale(&self, cx: &mut Context<Self>) {
        if let Some(open) = &self.notices.open {
            let _ = open.handle.update(cx, |_, window, _| {
                window.set_window_title(&tr("notices.window.title"));
            });
            open.content.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn shutdown_notices(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.notices.open.take() {
            let _ = open
                .handle
                .update(cx, |_, window, _| window.remove_window());
        }
    }
}
