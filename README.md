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

Start on **Home** and choose **Connect your devices** to check your receiver and trackers. You can browse the steps in any order or open an existing feature from the sidebar. The guide shows observed device activity, not a setup-complete result. See [First use](docs/USER_GUIDE.md#first-use).

## Downloads and platform limits

The [Native no-firmware CI workflow](.github/workflows/ci.yml) is configured to publish Windows x64 EXE and Linux x64 AppImage packages. Its download location is **Actions → Native no-firmware CI → successful run → Artifacts**.

- Normal Windows EXE filenames end in `windows-x64-no-firmware.exe`. Separate optional UPX-compressed filenames end in `windows-x64-no-firmware-upx.exe`. The normal EXE is the fallback for UPX-related security software or compatibility problems.
- The Linux AppImage uses an Ubuntu 22.04 build baseline with glibc 2.35. It is not fully static and requires a compatible host graphics stack, fonts, desktop and device services, and USB, HID, and serial permissions.
- macOS has not undergone separate validation.

CI packages include the default configuration but omit firmware payloads. Full builds include bundled firmware. No-firmware builds can use local files, remote downloads, or explicitly selected external configuration.

[Installation and launch instructions](docs/USER_GUIDE.md#installation) include the AppImage commands and the option to run without FUSE. [Linux device requirements](docs/USER_GUIDE.md#linux-devices) cover permissions and UF2 mounts. [Development and packaging](docs/DEVELOPMENT.md) covers source builds.

## Firmware update safety

Updates require an explicit review and confirmation of the target, board model, role, firmware source, and risks. Compatible firmware, stable power, and a USB recovery path are required. Cancellation does not undo written data or activation. A completed transfer does not prove that the new firmware runs correctly.

The [user guide](docs/USER_GUIDE.md#firmware-updates) describes each update method, recovery limits, and version checks after reconnection.

## License

Application-owned code and assets are available under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option. Dependencies, vendored code, third-party assets, and external firmware retain their own terms. See [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) for retained notices and source-availability instructions.

## Documentation

- [First use](docs/USER_GUIDE.md#first-use) and [user guide](docs/USER_GUIDE.md): devices, terminals, updates, capture, and replay.
- [Development and packaging](docs/DEVELOPMENT.md): dependencies, source builds, checks, configuration, and distribution.
- [Architecture](docs/ARCHITECTURE.md) and [protocols and sources](docs/PROTOCOLS.md): module boundaries, formats, identity checks, and numerical limits.
