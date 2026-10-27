#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app_runtime;
mod ble_ota;
mod board_catalog;
#[cfg(test)]
mod build_resource_tests;
mod bundled_resources;
mod calibration;
mod capture_device;
mod capture_session;
mod commands;
mod device_registry;
mod discovery;
mod domain;
mod esb_ota;
mod file_export;
mod firmware;
mod firmware_source;
mod firmware_update;
mod hid_log;
mod i18n;
mod opendfu;
mod operations;
mod receiver_events;
mod remote_config;
mod rssi_scan;
mod serial_port;
mod sessions;
mod settings;
mod shortcuts;
mod terminal;
mod terminal_view;
mod uf2;
mod ui;
mod update;

rust_i18n::i18n!("../../locales", fallback = "en");

#[derive(Debug, PartialEq, Eq)]
enum StartupMode {
    Workbench,
    Gallery,
}

fn startup_mode(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<StartupMode, &'static str> {
    let mut gallery = false;
    let mut other = false;
    for arg in args {
        if arg == "--gallery" {
            other |= gallery;
            gallery = true;
        } else {
            other = true;
        }
    }
    match (gallery, other) {
        (false, _) => Ok(StartupMode::Workbench),
        (true, false) => Ok(StartupMode::Gallery),
        (true, true) => Err("--gallery cannot be combined with other arguments"),
    }
}

fn main() {
    match startup_mode(std::env::args_os().skip(1)) {
        Ok(StartupMode::Workbench) => {}
        Ok(StartupMode::Gallery) => {
            #[cfg(debug_assertions)]
            {
                ui::gallery::run();
                return;
            }
            #[cfg(not(debug_assertions))]
            {
                eprintln!("--gallery is available only in debug builds");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
    let runtime = match app_runtime::AppRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    ui::run(runtime.executor());
    // The native event loop has ended and UI-owned workers have joined.
    runtime.shutdown();
}

#[cfg(test)]
mod startup_tests {
    use super::{StartupMode, startup_mode};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Result<StartupMode, &'static str> {
        startup_mode(args.iter().map(|arg| OsString::from(*arg)))
    }

    #[test]
    fn gallery_is_an_exclusive_opt_in() {
        assert_eq!(parse(&[]), Ok(StartupMode::Workbench));
        assert_eq!(parse(&["other", "--unknown"]), Ok(StartupMode::Workbench));
        assert_eq!(parse(&["--gallery"]), Ok(StartupMode::Gallery));
        assert!(parse(&["--gallery", "--unknown"]).is_err());
        assert!(parse(&["--unknown", "--gallery"]).is_err());
        assert!(parse(&["--gallery", "--gallery"]).is_err());
    }
}
