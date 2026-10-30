#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app_runtime;
mod application;
#[cfg(test)]
mod build_resource_tests;
mod components;
mod discovery;
mod domain;
mod features;
mod file_export;
mod hid_log;
mod i18n;
mod receiver_events;
mod rssi_scan;
mod serial_port;
mod sessions;
mod storage_path;
#[cfg(test)]
mod test_support;
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
    #[cfg(windows)]
    if let Some(code) = features::app_update::service::windows::dispatch(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
    ) {
        std::process::exit(code);
    }
    #[cfg(windows)]
    features::app_update::service::windows::freeze_running_image();
    #[cfg(windows)]
    let cleanup_worker = match features::app_update::service::windows::cleanup::startup_takeover() {
        Ok(features::app_update::service::windows::cleanup::StartupTakeover::None) => None,
        Ok(features::app_update::service::windows::cleanup::StartupTakeover::Continue(worker)) => {
            Some(worker)
        }
        Ok(features::app_update::service::windows::cleanup::StartupTakeover::CleanupOnly(
            worker,
        )) => {
            drop(worker);
            return;
        }
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    match startup_mode(std::env::args_os().skip(1)) {
        Ok(StartupMode::Workbench) => {}
        Ok(StartupMode::Gallery) => {
            #[cfg(debug_assertions)]
            {
                application::gallery::run();
                return;
            }
            #[cfg(not(debug_assertions))]
            {
                eprintln!("--gallery is available only in debug builds");
                #[cfg(windows)]
                drop(cleanup_worker);
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            #[cfg(windows)]
            drop(cleanup_worker);
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
    application::run(runtime.executor());
    // The native event loop has ended and UI-owned workers have joined.
    runtime.shutdown();
    #[cfg(windows)]
    drop(cleanup_worker);
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
