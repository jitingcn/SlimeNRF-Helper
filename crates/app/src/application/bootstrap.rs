use super::{Quit, Workbench, lifecycle::ShutdownHost};
use crate::app_runtime::Executor;
use gpui_kit::component::TitleBar;
use gpui_kit::{
    self as gpui, AppContext as _, AssetSource, Bounds, KeyBinding, SharedString, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use std::sync::Arc;

// Keep component defaults and embed additional application glyphs.
gpui::assets::icon_assets!(
    ExtraIconAssets,
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
        BatteryFull,
        Usb,
        ArrowLeft,
        Pencil,
        List,
        ListX,
        CornerDownLeft
    ]
);

pub(super) struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match ExtraIconAssets.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui::assets::Assets.list(path)?;
        paths.extend(ExtraIconAssets.list(path)?);
        Ok(paths)
    }
}

pub(crate) fn run(async_io: Executor) {
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
                    title: Some(
                        concat!("SlimeNRF Helper ", env!("SLIMENRF_BUILD_DISPLAY_VERSION")).into(),
                    ),
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
