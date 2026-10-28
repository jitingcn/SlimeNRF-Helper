# Changelog

[简体中文](CHANGELOG.zh-CN.md)

All notable changes to SlimeNRF Helper are documented in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
Version numbers follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-28

### Added

- A debug-only developer gallery with isolated synthetic controls and terminal scenes, plus documented device diagnosis and native UI smoke workflows.
- Terminal tabs can filter displayed lines by log level while retaining untagged command output; Copy all and log export remain unfiltered.
- The mouse wheel can switch log levels over the terminal filter control and recall command history over the input without sending commands.
- All platforms can check GitHub releases and open a newer version's release page. Official Windows portable builds can also download a matching integrity-verified update and restart to install it. Stable and prerelease channels are selectable; automatic checks do not download or install updates.

### Changed

- RSSI analysis uses a bar chart with an automatically scaled dBm axis, subtle **No sample** markers, and immediate hover details. Actual channel positions and current/recommended markers are preserved. The chart fits the available window height so its explanation remains visible at normal window sizes.
- The displayed application version includes the checkout's short commit ID and marks tracked local changes as dirty; source archives fall back to the base version or a validated GitHub Actions SHA. Release versions, tags, asset names, and update comparisons remain unchanged.
- Device lists use a flatter, more compact layout with a rounded summary bar: receiver serial numbers and online/total tracker counts share a wrapping metadata row, with the redundant command-console heading and outer list border and padding removed.
- Device-list toolbar actions use compact icon buttons with localized hover descriptions.
- Direct tracker commands now show the **Information** group first; the remaining command groups keep their existing order.
- The terminal Send button uses a larger return-key icon button aligned with the command input. Send all appears directly below it in the existing lower toolbar only when multiple eligible command ports are connected, without reserving extra vertical space.
- Selected terminal text is copied with keyboard shortcuts instead of a separate toolbar button.
- The root is now a virtual Cargo workspace, with the desktop application's source and build support in `crates/app` alongside the existing recording core. Root development commands and resource path semantics are unchanged.
- The terminal model and firmware-image parsing and content validation are isolated in internal workspace libraries. GUI presentation, hardware access, device authorization, and application build resources remain in the desktop app; the new libraries can be built and tested independently of its build script. This is an internal boundary change, not a new user-facing feature.
- Developer test scripts use cargo-nextest, with doctests available as a separate action; quality scripts run both. Windows MSVC builds use the Rust toolchain's bundled LLD linker, and Linux builds can opt into mold.
- Release builds use optimization level 2, fat LTO, one codegen unit, and symbol stripping.
- Main, license notices, RSSI analysis, and temperature calibration analysis windows now use theme-aware GPUI Kit title bars instead of system title-bar styling, while preserving window controls and shutdown cleanup.
- Application builds, development checks, packaging, CI, and the independent firmware oracle no longer require Python. One internal Rust tools package shares the frozen-input notice renderer with application builds; its maintenance CLI remains separate from the desktop runtime.
- Repeated Windows build-environment setup no longer accumulates static-CRT flags or Visual Studio tool and compiler search paths, avoiding unnecessary Cargo cache invalidation from nested developer terminals.
- Debug builds, runs, and tests can continue when license notices need review, with a Cargo warning and a pending-review message in the copyright page instead of a stale inventory. Release builds and packaging still require current reviewed notices.
- Windows packaging and CI now publish only the UPX-compressed executable under the normal filename, without a separate uncompressed download or `-upx` suffix. Compression uses `--best --lzma` instead of exhaustive `--ultra-brute` search.

### Fixed

- Moving the pointer after moving or resizing the window no longer unexpectedly selects terminal text; an existing selection is preserved.
- Resuming terminal Follow immediately shows the latest line even without new output, while preserving deliberate scrolling and selection.
- Tracker cards retain equal widths at fractional display scaling, and their content no longer shifts when activity changes.
- Settings navigation remains scrollable and keyboard-accessible when the available window height is reduced or interface text is enlarged.
- Settings and other scrollable pages keep scrollbar tracks and pointer hit areas fixed to their viewports, so thumb positions and drag ranges remain aligned with the visible content.

## [0.1.1] - 2026-10-26

### Added

- Project repository links in the **About** section of **Settings** for SlimeNRF Helper, the receiver firmware, the tracker firmware, and the bootloader.
- CI packaging and prerelease automation for separate normal and UPX-compressed Windows x64 executables and a Linux x64 AppImage, all without bundled firmware.

### Changed

- License notices are generated during application compilation and package staging instead of tracked as a generated source file. Application builds, checks, tests, editor analysis, and packaging now require Python 3.11+; running the application and building only the recording core do not.

### Fixed

- Existing terminal text now follows dark and light theme changes for default and basic ANSI colors. Extended indexed colors and RGB colors retain their specified values.
- Windows UPX packaging no longer passes the unsupported `--exact` option. Maximum compression remains enabled.

## [0.1.0]

Initial standalone application version, documented from the repository's initial source snapshot.

### Added

- Receiver and tracker discovery, independent serial sessions, tabbed terminals, command history, log export, and confirmation for destructive device commands.
- UF2, OpenDFU, receiver-based ESB OTA, and advanced legacy BLE DFU updates, with local, bundled, and supported remote firmware sources.
- HID diagnostics, raw sensor capture, offline VQF analysis, synchronized plots, and an orientation preview.
- Gyroscope calibration guides, temperature calibration analysis with SVG export, and RSSI channel analysis.
- English and Simplified Chinese interfaces, light and dark themes, and Windows and Linux packaging with full-firmware and no-firmware build modes.

[Unreleased]: https://github.com/jitingcn/SlimeNRF-Helper/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jitingcn/SlimeNRF-Helper/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v0.1.1
[0.1.0]: https://github.com/jitingcn/SlimeNRF-Helper/tree/c1534d42a08b71e2bf9acdf0c33892766493080e
