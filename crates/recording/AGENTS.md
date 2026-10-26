# Recording/replay core

Inherits [application guidance](../../AGENTS.md). This unpublished internal library is consumed by the desktop and must also work in an independent non-GUI program.

## Allowed responsibilities

- Capture data/protocol models, raw decoding and normalization, native recording/CSV exchange, replay/calibration computation, analysis and preset serialization/storage.
- Explicit file I/O and caller-provided paths are allowed. Desktop config-directory choice, user dialogs, localization, device discovery, hardware ownership and thread/job scheduling policy stay in the application.
- Do not depend on the desktop package, GPUI, HID/serial/BLE libraries, desktop-directory helpers, firmware bundles or application environment variables. Keep public inputs/outputs data-oriented; cancellation may use the existing shared atomic flag.
- Preserve file/schema compatibility and bounded archive reads. Do not silently reinterpret timestamps, units, receiver-local tracker IDs, calibration modes or missing measurements.

## Numerical and native boundary

- Firmware-compatible VQF behavior is the contract, not agreement with an arbitrary upstream Rust VQF implementation. Preserve precision, operation order, sample clocks, parameter defaults and calibration preprocessing unless the task explicitly changes numerical behavior.
- `vendor/vqf` contains the pinned firmware kernel and scalar CMSIS subset with provenance/licenses. This package's `build.rs` owns their static compilation. Do not add a runtime DLL dependency or move native build responsibility back to the desktop.
- The library denies unsafe code by default. Keep FFI exceptions inside the private filter adapter, with explicit unsafe operations and documented ownership/lifetime assumptions. Never expose raw native pointers in the public API.
- Frozen numerical checkpoints are regression protection. Do not re-record expected values merely to make an unrelated refactor pass. The full external trace oracle requires its real source input; an ignored/unavailable oracle is not a passing numerical proof.

## Verification

From the workspace root, with the native C compiler available:

```text
cargo fmt --all -- --check
cargo clippy -p slimenrf-recording --all-targets --locked -- -D warnings
cargo test -p slimenrf-recording --all-targets --locked
```

For public API, manifest or build changes, also compile/run a temporary external consumer without desktop/config/firmware prerequisites, and exercise actual recording/replay APIs. Remove the temporary consumer and its build artifacts afterward. Run desktop consumers through the workspace gate when the contract changes. Library `cfg(test)` internals are not available to the consuming application's unit tests; test numerical details here and UI behavior in the desktop.

See [the development guide's oracle procedure](../../docs/DEVELOPMENT.md#firmware-oracle) and `src/replay/reference_oracle.py` for full-trace validation; do not duplicate or invent an alternative numerical oracle.
