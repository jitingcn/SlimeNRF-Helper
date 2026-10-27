# Development and packaging

[简体中文](DEVELOPMENT.zh-CN.md)

[Home](../README.md) · [User guide](USER_GUIDE.md) · [Architecture](ARCHITECTURE.md) · [Protocol sources](PROTOCOLS.md)

Run all commands from the repository root. Follow the engineering rules in the [root AGENTS](../AGENTS.md) and [recording-core AGENTS](../crates/recording/AGENTS.md). This page maintains procedures, not historical test counts or workstation measurements.

When commands, behavior, or links change, update both the default English document and its same-directory `.zh-CN.md` counterpart. Keep navigation and cross-document links in the selected language.

## Toolchain and build modes

For native development, use the stable Rust toolchain pinned in [rust-toolchain.toml](../rust-toolchain.toml), including rustfmt, Clippy, rust-analyzer, and rust-src. rustup selects this toolchain automatically in this directory. Do not change your global default. The manifest's MSRV declares a compatibility floor, not a verified minimum version. [Cargo.lock](../Cargo.lock) fixes dependency versions. The GUI uses the official Fast feature through exports from the same `gpui_kit` source.

Application builds require Python 3.11+ with standard-library `tomllib` to generate license notices. This build-only prerequisite applies to scripts, direct Cargo, tests, checks, and editor analysis in both full and no-firmware modes. Running the packaged application and building only `slimenrf-recording` do not require Python.

Optionally set `SLIMENRF_PYTHON` to one interpreter executable name or path, not a shell command with arguments. An invalid or empty explicit selection fails without fallback. Without it, application builds probe `py -3` on Windows, then `python3` and `python`; Linux packaging probes `python3` and `python`. If those candidates fail, either entry point can ask `uv` on `PATH` for an already installed Python 3.11+ interpreter. The lookup ignores projects and virtual environments, configuration and network access; it never downloads Python. Every selected interpreter must pass a version and `tomllib` probe. Make it available in the editor's environment too.

CLI builds keep the default `embed-firmware` feature enabled. If a standalone checkout has no firmware payloads, explicitly add `-NoFirmware` or `--no-firmware` to every CLI build, run, check, and packaging command. For direct Cargo commands, use `--no-default-features`. Full builds require the intended firmware inputs. A no-firmware package does not satisfy full-package acceptance.

The scripts support build, run, test, check, clippy, fmt, and clean. Build defaults to Release. Run, test, check, and clippy default to the development profile. Explicit Debug and Release options are mutually exclusive. By default, build and run select the desktop application, test, check, and clippy cover the workspace, and fmt covers every member. Explicit Cargo package selection takes precedence. Development and release optimizations are separate. Packaging uses the actual Release artifact, never a development build.

<a id="windows"></a>
## Windows

Install Visual Studio 2022 with the "Desktop development with C++" workload, the Windows SDK, and CMake. Install LLVM if a dependency needs libclang. The PowerShell entry points initialize and restore the caller's environment and working directory. They use VS CMake, an explicit `x86_64-pc-windows-msvc` target, and the static MSVC CRT, while preserving existing Rust flags. For direct Cargo commands, configure these conditions yourself. Do not add crt-static only to the host configuration, where it can affect procedural macros.

```powershell
./build.ps1 -Action run -NoFirmware           # Build and launch the development version
./build.ps1 -Debug -NoFirmware                # Build the development version without launching
./build.ps1 -Action check -NoFirmware
./build.ps1 -Action test -NoFirmware
./build.ps1 -Action run -Release -NoFirmware  # Explicitly run Release
./package.ps1 -NoFirmware                    # Single-file release package
./quality.ps1 -NoFirmware                    # Format check -> strict Clippy -> tests
./quality.ps1 -FormatOnly                    # Check formatting without changes; no VS needed
./build.ps1 -Action fmt                      # Explicitly apply formatting
```

If you need execution-policy isolation, use `powershell -NoProfile -ExecutionPolicy Bypass -File ./build.ps1 ...`. The same applies to the other scripts. Remove `-NoFirmware` when firmware inputs are available. Pass extra Cargo arguments with `-CargoArguments`, for example `./build.ps1 -Action test -CargoArguments @('-p', 'slimenrf-recording')`.

A running EXE can lock the output. First, exit your own instance normally. If you must keep it running, set `$env:CARGO_TARGET_DIR='target/verified'`. Relative paths resolve from the application directory. Outputs are in its `x86_64-pc-windows-msvc/{debug,release}/` subdirectories. Do not create a separate editor cache.

<a id="checks-and-editor"></a>
## Checks and editor setup

`quality.ps1` and `quality.sh` use locked dependencies, stable rustfmt, strict Clippy across the workspace and all targets with `-D warnings`, and tests. They stop on failure. Cover both full and no-firmware modes for changes to shared boundaries, builds, configuration, or resources. Do not add `--all-features`, which re-enables embedded firmware.

After configuring native dependencies, use these equivalent Cargo commands:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

For no-firmware mode, add `--no-default-features` to the last two commands. To check only the core, use `cargo test --locked -p slimenrf-recording`. The core still needs a C compiler, but not Python, GPUI, hardware interfaces, application YAML, or firmware. External updater fixtures and the full firmware oracle are separate checks that require real additional inputs. Do not count ignored tests as passed.

Before creating a `Workbench` GUI fixture, acquire one `ui::test_support::UiTestSession` at the outer test scope. Reuse that session for all fixtures in the same `TestAppContext`. Keep it alive through callbacks and assertions to isolate the process-wide locale.

For contexts created outside `#[gpui_kit::test]`, follow the standalone ownership in [the GUI test fixtures](../src/ui/test_support.rs). Keep the `TestDispatcher` alive until the context is dropped, then drain its pending tasks. Keep `AppRuntime` alive until UI cleanup finishes.

`.vscode` contains only project-level settings. rust-analyzer runs normal `check` with `rust-analyzer.cargo.noDefaultFeatures` enabled, so editor analysis needs no firmware payloads. This editor-only default does not change CLI features. Run strict Clippy through the explicit quality entry points. On Windows, close all VS Code instances before running `./scripts/open-editor.ps1` to reuse the MSVC and CRT environment and normal cache. The script refuses to reuse an existing Code process. It does not close other people's windows or change global settings.

Keep the pinned stable toolchain, normal rust-analyzer `check`, and existing native cache. On Windows, tools that invoke Cargo need a consistent inherited host MSVC, C, and C++ environment. Environment differences can invalidate native-build fingerprints even when sources are unchanged. Fingerprint invalidation alone does not prove stale source or justify routine `clean`.

The optional `./scripts/install-hooks.ps1` installs a non-mutating formatting hook only when the application itself is an independent Git root. It rejects parent repositories, preserves other hooks, and does not overwrite a different pre-commit hook. It never stages or commits automatically. The hook does not replace the quality entry points or native GUI validation. For partial staging, ensure that the checked working tree matches the staged content yourself.

## Update dependencies

1. Review upstream release notes and the manifest's MSRV before changing version requirements. Keep the normal and test `gpui-kit` requirements identical.
2. Update the manifest and `Cargo.lock` together with targeted `cargo update -p PACKAGE --precise VERSION` commands, not a blanket update. For GPUI updates, inspect the published Kit manifests and update the complete Kit and Fast families together; the current requirements use Kit 0.8.1 and the lockfile selects Fast 0.1.12. Keep a single GPUI type universe through `gpui_kit` exports. Review upstream API changes, especially modifier handling and custom `Element` layout/prepaint/paint lifecycles, before running the gates below.
3. Run the platform [quality commands](#checks-and-editor) in both full and no-firmware modes.
4. Run the native application. Check terminal scrolling, wrapping, text selection, plot interaction, and remote firmware loading.
5. Refresh the [license notices](#license-notices) from both locked target graphs.

The application explicitly supplies bundled Mozilla root certificates to reqwest. Preserve this policy when changing TLS dependencies. Offline startup must not depend on a system certificate store. HTTPS still verifies certificate chains and hostnames.

<a id="translations"></a>
## Change UI translations

Keep existing keys when editing text. To add text, follow these steps.

1. Choose a key by feature and purpose, such as `firmware.serial_dfu.enter`. Do not use a full English sentence as a key. Use separate contextual keys when identical English text means different things.
2. Add the same YAML hierarchy to [locales/en.yml](../locales/en.yml) and [locales/zh-CN.yml](../locales/zh-CN.yml). Each hierarchy segment uses lowercase letters, digits, and underscores, and starts with a letter. Store text in leaf nodes. Do not split the catalogs into feature files.
3. Keep `%{name}` placeholder names consistent across languages. Preserve meaningful line breaks.
4. Read ordinary text with `tr("firmware.serial_dfu.enter")`. Use `rust_i18n::t!` when replacing placeholders.
5. Display command values, device output, device names, and user preset names directly. Do not pass these data to translation functions. To translate recording-core enum errors, map them to keys in the application layer. Do not add a translation dependency to `slimenrf-recording`.
6. Rebuild the application. YAML-only changes trigger compilation. You do not need to touch Rust files or clear caches.
7. Run [quality checks](#checks-and-editor) in full and no-firmware modes. Then switch between English and Simplified Chinese. Check the changed UI, input hints, and open tool windows, and confirm that entered values remain unchanged.

Translation-resource tests check matching language keys, duplicate nested keys, consistent placeholders, compiled lookup, and English fallback. See [translation ownership](ARCHITECTURE.md#translation-ownership) for the format and responsibilities.

## Configuration, resources, and source hygiene

[slimenrf-helper.yaml](../slimenrf-helper.yaml) is the authoritative configuration. Maintain `schema_version`, boards, exact resources and aliases, OpenDFU parameters, and remote application and bootloader namespaces there. Do not copy them into another configuration table.

- The development default is `firmware_root: ../firmwares`. This external directory is not included in a checkout. Resources resolve relative to the selected YAML file's directory.
- At build time, explicitly select a YAML file with `SLIMENRF_CONFIG_PATH` and supply the local firmware root with `SLIMENRF_FIRMWARE_ROOT`. Keep public and embedded YAML paths portable and relative. A no-firmware build does not read the firmware root, but still validates configuration.
- At runtime, the application looks for `slimenrf-helper.yaml` in the executable directory, then the working directory, then uses embedded configuration. It does not read old Python configuration or search parent directories. External configuration is authoritative. Invalid or missing resources cause errors, without silently mixing in embedded sources. Build-input variables do not mean that arbitrary private YAML files load automatically at runtime.
- `firmware` and `bootloader` name exact default resources. Missing resources do not trigger a version substitution. Explicit aliases allow only restricted version-fragment matching. Directories are scanned only at the top level when loading. Candidates are not automatically selected by newest version and do not override user choices.
- Full packages embed only configuration and supported UF2, HEX, BIN, and ZIP files at the top level of the firmware root. They do not include the development workspace, logs, or backups. At runtime, resources use bounded single-member extraction and size, CRC, and SHA checks. The application does not extract a whole directory beside itself. Do not conflate file and embedded-member source identities by basename.

Store local configuration, recordings, logs, credentials, and historical execution material in ignored `.local/` storage. You can also use the specifically ignored `slimenrf-helper.local.yaml`, but must select it explicitly. The application and scripts do not automatically read `.local/`, `.env`, or `*.local.yaml`. Do not commit real USB serial numbers, RF addresses, tokens, private keys, or private absolute paths. Sanitize samples separately for public issues. Do not move or overwrite platform user preferences for publication cleanup. Ignore rules do not remove already tracked files from Git. Maintainers must separately review actual release contents and history.

<a id="public-preflight"></a>
## Check source before publication

Install Git and Python 3.10+ for the [public checker](../scripts/check-public.py). The application must be its own Git root, either a standalone repository or a regular linked worktree. A parent repository is rejected. On Windows, use `python` instead of `python3` if that is your installed interpreter and it meets the version requirement.

```bash
python3 scripts/check-public.py
python3 -B -m unittest discover -s scripts -p 'test_check_public.py'
```

The checker inventories tracked files, nonignored untracked files, and tracked files that match ignore rules. It checks working-tree contents, not staged bytes or Git history. Forbidden publication paths are reported without reading their contents. Text reads are limited to 8 MiB per file and use UTF-8 with an optional BOM. Binary, undecodable, oversized, unreadable, or otherwise unreviewed candidates fail the check, as do symlinks and reparse points.

Exit code `0` means no findings, `1` means findings need review, and `2` means invalid arguments or a root, Git, or setup failure. Findings contain path, line, and category, not matched secret values. The finite known patterns and documentation-link checks do not prove that credentials, hardware identities, history, or release artifacts are clear. Review those separately. The checker does not stage, publish, delete files, or alter user state.

<a id="license-notices"></a>
## Maintain license notices

[generate_notices.py](../scripts/generate_notices.py) uses Python 3.11+ and standard-library `tomllib`. Validate the tracked inputs and rendering without reading or writing a generated document, then run its tests:

```bash
python3 scripts/generate_notices.py --check
python3 -B -m unittest discover -s scripts -p 'test_generate_notices.py'
```

Interpreter discovery has isolated subprocess tests. They compile small native Rust fixtures and require the configured Rust toolchain and native linker; Linux also exercises `package.sh`. On Windows, run from an x64 Visual Studio developer shell and use your Python executable instead of `python3`:

```bash
python3 -B -m unittest discover -s scripts -p 'test_python_discovery.py'
```

[scripts/notices-inputs.json](../scripts/notices-inputs.json) freezes portable, reviewed package metadata, target graphs, original notice bytes, source origins, and SHA-256 hashes. These inputs, their provenance, and repository license sources remain tracked. The generator checks input hashes, locked package checksums, and package coverage against those frozen graphs. It renders `THIRD_PARTY_NOTICES.txt` deterministically offline, without Cargo, a registry cache, or a network. It does not discover dependencies or collect license texts.

When Cargo inputs, vendored sources, or license materials change:

1. Review the changed inputs. For dependency changes, capture both locked target graphs with the commands below. Follow normal and build edges from `slimenrf-helper`, excluding dev edges. Mark build and proc-macro paths as compile tools, and preserve the conservative union and feature unification described in the generator header.

   ```bash
   cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
   cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-unknown-linux-gnu
   ```

2. Review each added or changed package and source. Update the frozen graphs, package and material entries, source-availability instructions, and input hashes as needed. Preserve existing original source bytes. Add reviewed exact license and NOTICE bytes with their origins, encoding, byte length, and hashes in the input schema. Do not fix a stale-input error by blindly replacing hashes.
3. Generate a local review copy outside the public source inventory. Review its changes, then repeat `--check` and the generator tests:

   ```bash
   python3 scripts/generate_notices.py --output .local/THIRD_PARTY_NOTICES.txt
   python3 scripts/generate_notices.py --check --output .local/THIRD_PARTY_NOTICES.txt
   ```

Writing requires an explicit `--output` path. `--check` alone validates inputs and rendering without an output file. With `--output`, it compares the explicit artifact byte-for-byte and never rewrites it.

The root application build script invokes this same renderer for both full and no-firmware builds, writes to `OUT_DIR/THIRD_PARTY_NOTICES.txt`, and embeds those bytes. Cargo tracks the generator, frozen inputs, and repository-origin files. Generated notices are untracked; do not commit a root copy or duplicate the renderer in Rust. AppImage and Flatpak packaging use the same renderer for their installed copies. AppImage installs `LICENSE-MIT`, `LICENSE-APACHE`, and `THIRD_PARTY_NOTICES.txt` under `usr/share/licenses/slimenrf-helper` inside the AppDir. Flatpak installs the same files under `/app/share/licenses/slimenrf-helper`.

Keep MPL-2.0 covered source available separately as the notice document instructs. Retain the exact checksum-verified source archives with each release or provide an equivalent stable location. If covered files change, provide the actual modified source and update the instructions. Notice generation does not clear redistribution rights for firmware or the native libraries, DLLs, runtimes, and toolchain materials in final artifacts. Review those for each release.

## Package Windows releases and UPX variants

`package.ps1` creates `dist/slimenrf-helper-<version>-windows-x64.exe`. `-NoFirmware` creates a separate `-no-firmware.exe` that still embeds default configuration. Do not publish the whole target directory, PDB files, or test programs. A static CRT does not prove acceptance on a fresh Windows installation or acceptance of signing.

Create the regular package first. Then use its actual versioned filename to create a separate compressed package:

```powershell
./scripts/package-upx.ps1 -InputPath ./dist/slimenrf-helper-0.1.1-windows-x64-no-firmware.exe
```

The [script](../scripts/package-upx.ps1) pins the tool version and hash. It creates a separate `-upx.exe` without changing the input. Compression can add startup overhead or antivirus false positives. Prefer the regular package for compatibility problems. Compression changes EXE bytes and invalidates any existing Authenticode signature. If you sign, sign after final compression and repeat acceptance checks. CI does not sign packages.

The script uses `--best --ultra-brute --strip-relocs=0` without `--exact`. Before publication, it runs UPX's integrity check and decompresses to a separate temporary file. Decompression may produce different executable bytes. The original package remains unchanged.

Both scripts use the [shared atomic publisher](../scripts/publish-file.ps1). An identical existing output is kept. Different content requires explicit `-Force`. The publisher stages on the destination volume, then moves or replaces the destination instead of overwriting its bytes. This preserves the input and other hardlinks to the old output. It refuses symlinks and reparse points in publication paths. File locks allow readers but deny writers, and input locks also deny deletion. If a required lock cannot be acquired, publication fails without replacing the output. These checks do not promise safety against every hostile filesystem race.

<a id="linux"></a>
## Linux

Install C and C++, Clang and libclang, CMake, pkg-config, udev, D-Bus, xkbcommon with X11 support, Wayland, X11 and XCB, fontconfig, and FreeType. Use the local rustup toolchain. Do not inject the Windows target or CRT settings.

Ensure that the selected Python is 3.11+ before building. The distribution's `python3` package alone may be older, including on Ubuntu 22.04. Install a supported interpreter separately when needed and select it with `SLIMENRF_PYTHON`.

```bash
# Arch
sudo pacman -S --needed base-devel clang cmake pkgconf systemd-libs dbus \
  libxkbcommon libxkbcommon-x11 wayland libx11 libxcb fontconfig freetype2 \
  vulkan-icd-loader python
# Debian / Ubuntu
sudo apt update
sudo apt install build-essential clang cmake pkg-config libudev-dev libdbus-1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libx11-xcb-dev \
  libxcb1-dev libfontconfig1-dev libfreetype6-dev libvulkan1 python3

bash ./build.sh run --no-firmware
bash ./build.sh check --no-firmware
bash ./quality.sh --no-firmware
bash ./quality.sh --format-only
bash ./build.sh fmt
bash ./package.sh --no-firmware
```

The [workflow](../.github/workflows/ci.yml) is the authoritative full CI dependency list. See the upstream [GPUI Kit installation instructions](https://gpui-kit.com/docs/installation). Runtime needs a real X11 or Wayland session, Vulkan GPU drivers and an ICD, user D-Bus, and fonts. You can install DejaVu, Noto, and CJK fonts. File choosers need `xdg-desktop-portal` and the corresponding desktop backend. A minimal session can use the gtk backend. BLE needs BlueZ and an adapter. See the [user guide](USER_GUIDE.md#linux-devices) for device permissions, real UF2 mounts, and WSL boundaries.

Pass extra Cargo arguments with `--cargo-arguments`, for example `bash ./build.sh test --cargo-arguments -p slimenrf-recording`. `package.sh` defaults to ELF. It selects `SLIMENRF_PYTHON` or `python3`, checks Python 3.11+ and `tomllib`, and exports the selected executable so Cargo and packaging agree. Python package helpers invoke the notice generator with `sys.executable`. Packaging selects the actual Release application ELF from Cargo JSON and names it by version and ELF architecture, with a suffix for no-firmware builds. Same-name conflicts require an explicit `--force`. An ELF is not a universal static package. Build on the oldest glibc baseline you plan to support, then validate on target distributions.

The native cache defaults to target. Relative `CARGO_TARGET_DIR` paths resolve from the application root. An explicit `--target-dir` takes precedence. Do not share compiled caches between Windows, Linux, and the SDK. Under WSL, you can keep a single Linux cache at `$HOME/.cache/slimenrf-helper/target` and share it between the editor and scripts.

### AppImage

In addition to native dependencies, install curl, `file`, and `desktop-file-utils`. The [packager](../scripts/package_appimage.py) pins linuxdeploy and appimagetool versions and SHA-256 hashes and verifies them before execution. Mutable latest versions are not accepted. For one-time x86-64 setup:

```bash
mkdir -p "$HOME/.cache/slimenrf-helper/appimage-tools/1-alpha-20251107-1" \
  "$HOME/.cache/slimenrf-helper/appimage-tools/1.9.1"
export SLIMENRF_LINUXDEPLOY="$HOME/.cache/slimenrf-helper/appimage-tools/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage"
export SLIMENRF_APPIMAGETOOL="$HOME/.cache/slimenrf-helper/appimage-tools/1.9.1/appimagetool-x86_64.AppImage"
curl -fL https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage -o "$SLIMENRF_LINUXDEPLOY"
curl -fL https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage -o "$SLIMENRF_APPIMAGETOOL"
chmod +x "$SLIMENRF_LINUXDEPLOY" "$SLIMENRF_APPIMAGETOOL"
bash ./package.sh --format appimage --no-firmware
```

Both downloaded tools and the generated application support `--appimage-extract-and-run`. The AppImage includes selected desktop client libraries. It does not bundle glibc, the loader, GPU or graphics loaders, Wayland, libudev, fonts, or services. Wayland must match the host driver ABI. Some X11 Mesa paths load Wayland too. Excluding driver files while bundling an old Wayland version is not sufficient. The build baseline still determines the minimum glibc version. The entry point preserves the working directory and arguments. linuxdeploy's second strip pass is disabled. The application's own Release stripping remains enabled.

<a id="flatpak"></a>
### Flatpak

Flatpak is an optional local source-built package, not a wrapper around a host ELF. Install flatpak, flatpak-builder, and host Python 3.11+. For full packages, resolving the firmware root from YAML needs python-yaml or python3-yaml in that interpreter. Explicit `SLIMENRF_FIRMWARE_ROOT` avoids this additional Python dependency.

```bash
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.freedesktop.Platform//26.08 org.freedesktop.Sdk//26.08
bash ./package.sh --format flatpak --no-firmware
flatpak install --user ./dist/slimenrf-helper-0.1.1-linux-x86_64-no-firmware.flatpak
flatpak run io.github.jitingcn.SlimeNRFHelper
```

The [build entry point](../scripts/package_flatpak.py) uses the same Rust pin and official release manifests to generate SHA-verified build-only rustc, cargo, and std modules. It compiles with `--offline --locked` inside the SDK without network access. A missing version or architecture fails instead of falling back to the SDK compiler. The toolchain is removed before export. Full and no-firmware packages use the same application ID, so installing one replaces the other.

The SDK build uses its own `python3` with Python 3.11+ and `tomllib`, not the host's `SLIMENRF_PYTHON` path or host uv discovery. It generates embedded and installed notices from the staged, tracked inputs without network access. Python is a build tool, not an application runtime dependency.

This is a broad-permission compatibility package, not a strict sandbox. To support hidraw, tty, BLE, and arbitrary native UF2 mounts, it grants access to devices, host files, read-only udev, the specified BlueZ bus access, networking, and graphics. Host ACLs and block-device identity checks still apply. Validate special paths and hot-plug propagation on target machines. Do not weaken UF2 checks. The bundle excludes the runtime. First installation may download it, so this is not an offline single-file distribution. The project has not applied for Flathub listing. Refer to the official [permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html) and [bundle limitations](https://docs.flatpak.org/en/latest/single-file-bundles.html).

The SDK cache defaults to `${XDG_CACHE_HOME:-$HOME/.cache}/slimenrf-helper/flatpak/<architecture>`. Change its location with `SLIMENRF_FLATPAK_CACHE` and parallelism with `SLIMENRF_FLATPAK_JOBS`, which defaults to 2. Do not mix it with native caches. If disk space is low, first move the cache to a Linux filesystem with more space. OSTree's reserved space can reject exports even when df reports free space. Do not automatically reduce this protection or clear other people's caches. For a single WSLg X11 diagnostic run, use `flatpak run --nosocket=wayland --socket=x11 io.github.jitingcn.SlimeNRFHelper`. Do not save a global override.

## CI and release acceptance

[Native no-firmware CI](../.github/workflows/ci.yml) runs on pushes, PRs, and manual triggers. The `ubuntu-24.04` source job checks publication boundaries, frozen notice inputs, and release-note preparation. It also runs their Python tests. After it passes, the `windows-2022` and `ubuntu-22.04` x64 jobs run independently. Both run the isolated interpreter-discovery tests and maintained quality entry points before packaging; the Linux discovery tests also exercise `package.sh`.

Windows uploads separate normal and maximum-compression UPX executables. Linux uploads an AppImage. All three omit firmware, use the Cargo version in their filenames, and remain available as Actions artifacts for 14 days, subject to repository policy. CI does not build Flatpak or upload caches as release assets.

A `v<version>` tag push also runs the prerelease job after all three prerequisite jobs succeed. The tag must match the root application's Cargo version and the dated entry in [CHANGELOG.md](../CHANGELOG.md). Release notes use that entry and link to the Chinese changelog at the same commit.

The publisher downloads the exact artifacts from that run without rebuilding them. It checks the remote tag's commit and refuses any existing release for the tag, including a draft. SHA-pinned `softprops/action-gh-release` creates a draft and uploads the files with overwrites disabled. The final step verifies all three uploaded names and sizes before publishing a prerelease that is not marked latest. A failed upload or verification can leave a draft. Review that draft manually before retrying. The workflow does not overwrite or delete it.

Repository permissions remain read-only except for `contents: write` on the prerelease job. Publication uses the workflow token and needs no additional secret. The workflow never pushes commits or tags.

### Publish a prerelease

1. Update the workspace version, lockfile, frozen notice input hashes, and both changelogs. Use the UTC publication date.
2. Run the quality and packaging checks for the release changes.
3. Commit the release changes on `main`.
4. Create an annotated tag that matches the Cargo version.
5. Push `main` and the tag together.

For version `0.1.1`, the final commands are:

```bash
	git tag -a v0.1.1 -m "SlimeNRF Helper 0.1.1"
	git push --atomic origin main v0.1.1
```

Wait for the tag's workflow to finish. Check the prerelease notes and all three downloads on [Releases](https://github.com/jitingcn/SlimeNRF-Helper/releases). Do not move a published tag or replace its assets.

Successful CI confirms only the configured checks and uploaded artifacts. Local builds, WSLg rendering, native GUI behavior, USB permissions, and real device updates are separate evidence. Target distributions, macOS, signing, performance, and physical hardware still require independent acceptance. The application-owned MIT OR Apache-2.0 license does not replace the separate third-party, source-availability, firmware, and artifact reviews described [above](#license-notices). Successful packaging does not resolve them.

## Clean and troubleshoot

`./build.ps1 -Action clean` and `bash ./build.sh clean` remove all profiles and nested caches in the selected target. Debug and Release options do not narrow the scope. Preview with `-CargoArguments '--dry-run'` or `--cargo-arguments --dry-run`, respectively, or specify `--target-dir` explicitly. These commands do not delete dist, configuration, firmware, global Cargo downloads, or Rust toolchains. First, exit your own programs launched from target. Do not clean routinely.

To diagnose Fast drawing or scrolling, set `GPUI_VIEW_RETENTION=0` or `GPUI_SCROLL_LAYERS=0` for a single launch, then clear the setting. These diagnostic switches do not change the GUI backend and are not defaults. Software Vulkan is only for compatibility diagnosis, not a performance guarantee. For macOS, configure native dependencies according to the upstream installation instructions and validate separately. The Linux package entry point does not generate macOS packages.

## Use upstream bootloader fixtures

Download the following assets from [0.11.0-slimenrf](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.0-slimenrf) and [0.11.1-slimenrf](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.1-slimenrf). Put them in `.local/updater-fixtures`, not in the development firmware directory:

```text
update-slimenrf_promicro_bootloader-0.11.0-slimenrf_nosd.uf2
update-slimenrf_promicro_bootloader-0.11.1-slimenrf_nosd.uf2
update-mochaelec_mochi_bootloader-0.11.1-slimenrf_nosd.uf2
```

```powershell
$env:SLIMENRF_UPDATER_FIXTURES = (Resolve-Path '.local/updater-fixtures').Path
./build.ps1 -Action test -NoFirmware -CargoArguments @('-p', 'slimenrf-helper', 'upstream_nosd_updater_payloads', '--', '--ignored')
```

```bash
SLIMENRF_UPDATER_FIXTURES="$PWD/.local/updater-fixtures" cargo test --locked --no-default-features -p slimenrf-helper upstream_nosd_updater_payloads -- --ignored
```

The test checks pinned upstream SHA values, UF2 content, and admission for correct and incorrect combinations of MCU and purpose. It does not access the network or write devices, and does not replace physical update validation.

<a id="firmware-oracle"></a>
## Run the independent firmware oracle

See [protocol documentation](PROTOCOLS.md#numerics) for sources and numerical limits. Ordinary `cargo test --locked -p slimenrf-recording` uses committed frozen external checkpoints. It does not depend on a developer checkout, WSL, or an SDK. The full event-by-event test is ignored by default. First, use Linux or WSL with Python 3 and GCC, real firmware including the matching vqf-c, and the full CMSIS SDK to generate a trace:

```bash
mkdir -p .local
python3 crates/recording/src/replay/reference_oracle.py \
  --firmware /path/to/SlimeVR-Tracker-nRF \
  --cmsis-dsp /path/to/sdk/modules/lib/cmsis-dsp \
  --output .local/replay-oracle.csv
export SLIME_REPLAY_ORACLE="$(pwd)/.local/replay-oracle.csv"
cargo test --locked -p slimenrf-recording replay::tests::firmware_reference_trace -- --ignored --nocapture
```

The generator extracts the actual wrapper and temperature MLS implementation and links the external engine and scalar SDK sources. It does not link the `replay_bridge.c` under test. It also outputs provenance with hashes of the source and trace and logical-root parameters, without publishing local absolute source directories. Omit `--cmsis-dsp` to generate a portable baseline. Use `--compare baseline.csv` to compare against a specified trace.

If WSL generates the trace and Windows runs the test, both must access the same file. Use `wslpath -w "$(realpath .local/replay-oracle.csv)"` to convert the path. Do not use a same-named file from another checkout. After configuring the native Windows environment:

```powershell
$env:SLIME_REPLAY_ORACLE = (Resolve-Path ./.local/replay-oracle.csv).Path
cargo test --locked -p slimenrf-recording replay::tests::firmware_reference_trace -- --ignored --nocapture
```

This is a controlled host-side algorithm comparison, not proof of bit-for-bit ARM equivalence or reproduction of live state. Do not regenerate historical expected values because new inputs drift.
