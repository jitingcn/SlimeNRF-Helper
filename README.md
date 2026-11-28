# SlimeNRF Helper

[简体中文](README.zh-CN.md)

SlimeNRF Helper is a desktop application for SlimeNRF receivers and trackers. It provides device management, serial terminals, firmware updates, and sensor data capture and analysis.

## Features

- A default Home with a three-step connection guide, live device status, connection help, and direct routes to device tools and firmware updates.
- Receiver groups and tracker discovery, independent serial sessions, tabbed terminals, command history, and log snapshot export.
- Role-specific command descriptions in English and Simplified Chinese, parameter forms, multi-target previews, and confirmation for dangerous operations.
- UF2 updates, Holyiot updates with OpenDFU, and ESB OTA through receiver HID. Legacy BLE DFU is available through advanced settings.
- Local firmware files, bundled firmware in full builds, and supported remote Release and CI artifacts.
- HID data and event diagnostics, raw packet recording, raw sensor capture, and offline VQF analysis with synchronized plots and an orientation preview.
- [Stationary gyro calibration and per-axis sensitivity guides](docs/USER_GUIDE.md#gyro-calibration) under one **Gyro calibration** entry, for a direct USB tracker or an online tracker through a receiver.
- Temperature calibration plots, fit diagnostics, SVG export, and RSSI channel plots, with serial response display and saved-log import.
- English and Simplified Chinese, light and dark themes, and theme color settings.
- [GitHub release checks on all platforms](docs/USER_GUIDE.md#app-updates); official Windows portable builds additionally support verified downloads and user-confirmed restart to install.

Start on **Home** and choose **Connect your devices** to check your receiver and trackers. You can browse the steps in any order or open an existing feature from the sidebar. The guide shows observed device activity, not a setup-complete result. See [First use](docs/USER_GUIDE.md#first-use).

## Downloads and platform limits

Download the package for your platform from [Releases](https://github.com/jitingcn/SlimeNRF-Helper/releases). New standard tagged releases require five no-firmware packages: one Windows x64 EXE, two Linux AppImages (x64 and ARM64), and two macOS DMGs (Apple Silicon and Intel). Development builds for the same five targets are available under **Artifacts** in successful runs of the current [Native no-firmware CI](https://github.com/jitingcn/SlimeNRF-Helper/actions/workflows/ci.yml).

The Linux ARM64 AppImage is not part of v0.7.0 or earlier releases. The [v0.6.0 release](https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v0.6.0) keeps its original macOS DMGs marked `preview`.

- New Windows releases and current CI builds ship the uncompressed Cargo executable. Its filename still ends in `windows-x64-no-firmware.exe`.
- On Linux, choose `linux-x64-no-firmware.AppImage` if `uname -m` reports `x86_64`, or `linux-arm64-no-firmware.AppImage` if it reports `aarch64`. Both use an Ubuntu 22.04 build baseline with glibc 2.35. They are not fully static and require a compatible host graphics stack, fonts, desktop and device services, and USB, HID, and serial permissions.
- The macOS DMGs require macOS 15 or later. Choose `arm64` for Apple Silicon or `x64` for Intel. The app is ad hoc signed, without Developer ID signing or notarization. See [macOS installation](docs/USER_GUIDE.md#macos-installation) for Gatekeeper and manual updates.

Release and CI packages include the default configuration but omit firmware payloads. Full builds include bundled firmware. No-firmware builds can use local files, remote downloads, or explicitly selected external configuration.

[Installation and launch instructions](docs/USER_GUIDE.md#installation) include the AppImage commands and the option to run without FUSE. [Linux device requirements](docs/USER_GUIDE.md#linux-devices) cover permissions and UF2 mounts. [Development and packaging](docs/DEVELOPMENT.md) covers source builds.

## Firmware update safety

By default, updates require an explicit review and confirmation of the target, board model, role, firmware source and risks. Optional minimal confirmation skips the extra confirmation only for matching selections; unknown or conflicting compatibility hints still require explicit risk acceptance. Image and device safety checks remain enforced. Use stable power and keep a USB recovery path available. Cancellation does not undo written data or activation. A completed transfer does not prove that the new firmware runs correctly.

The [user guide](docs/USER_GUIDE.md#firmware-updates) describes each update method, recovery limits, and version checks after reconnection.

## License

Application-owned code and assets are available under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option. Dependencies, vendored code, third-party assets, and external firmware retain their own terms. The application generates and embeds `THIRD_PARTY_NOTICES.txt` during compilation using the shared Rust notice library; the generated file is not tracked. Read the retained notices and source-availability instructions offline in **Settings > View licenses and notices**, or follow the [notice-maintenance procedure](docs/DEVELOPMENT.md#license-notices).

## Documentation

- [First use](docs/USER_GUIDE.md#first-use) and [user guide](docs/USER_GUIDE.md): devices, terminals, updates, capture, and replay.
- [Development and packaging](docs/DEVELOPMENT.md): dependencies, source builds, checks, configuration, and distribution.
- [Architecture](docs/ARCHITECTURE.md) and [protocols and sources](docs/PROTOCOLS.md): module boundaries, formats, identity checks, and numerical limits.
- [Changelog](CHANGELOG.md) lists notable changes by version.
