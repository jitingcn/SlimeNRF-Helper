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

fn main() {
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
