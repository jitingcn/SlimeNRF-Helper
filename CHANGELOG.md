# Changelog

[简体中文](CHANGELOG.zh-CN.md)

All notable changes to SlimeNRF Helper are documented in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
Version numbers follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Main, license notices, RSSI analysis, and temperature calibration analysis windows now use theme-aware GPUI Kit title bars instead of system title-bar styling, while preserving window controls and shutdown cleanup.

### Fixed

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

[Unreleased]: https://github.com/jitingcn/SlimeNRF-Helper/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v0.1.1
