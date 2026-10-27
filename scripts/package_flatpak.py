#!/usr/bin/env python3
"""Build optional local Flatpak bundles from vendored source in the Freedesktop SDK."""

import contextlib
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request


APP_ID = "io.github.jitingcn.SlimeNRFHelper"
RUNTIME_REPO = "https://dl.flathub.org/repo/flathub.flatpakrepo"
ASSETS = Path(__file__).resolve().parent / "flatpak"
# Explicit source inputs, not a workspace copy: never stage native caches or user data.
SOURCE_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
                "LICENSE-MIT", "LICENSE-APACHE",
                "scripts/generate_notices.py", "scripts/notices-inputs.json",
                "crates/app/Cargo.toml", "crates/app/build.rs",
                "crates/recording/Cargo.toml", "crates/recording/build.rs")
SOURCE_TREES = ("crates/app/src", "crates/app/build_support", "locales",
                "crates/recording/src", "crates/recording/vendor/vqf")
EXCLUDED = {"target", "dist", ".git", ".flatpak-builder", "__pycache__", ".cache"}
RUST_DIST = "https://static.rust-lang.org/dist"
RUST_PREFIX = "/app/lib/slimenrf-build-rust"
RUST_TARGETS = {"x86_64": "x86_64-unknown-linux-gnu", "aarch64": "aarch64-unknown-linux-gnu"}
MANIFEST_LIMIT = 4 * 1024 * 1024


def toml_parser():
    try:
        import tomllib
    except ImportError as error:
        raise ValueError("Flatpak packaging requires Python 3.11+ (standard-library tomllib).") from error
    return tomllib


def rust_toolchain_module(root, architecture):
    tomllib = toml_parser()
    try:
        with open(root / "rust-toolchain.toml", "rb") as source:
            pin = source.read(65537)
        if len(pin) > 65536:
            raise ValueError("rust-toolchain.toml exceeds 64 KiB.")
        version = tomllib.loads(pin.decode("utf-8"))["toolchain"]["channel"]
    except (OSError, UnicodeError, ValueError, KeyError, TypeError) as error:
        raise ValueError(f"Cannot read the Flatpak Rust pin: {error}") from error
    if not isinstance(version, str) or not re.fullmatch(r"[1-9][0-9]*\.[0-9]+\.[0-9]+", version):
        raise ValueError("Flatpak needs an explicit numeric stable toolchain.channel in rust-toolchain.toml.")
    target = RUST_TARGETS.get(architecture)
    if target is None:
        raise ValueError(f"No official native Flatpak Rust target for {architecture!r}.")
    url = f"{RUST_DIST}/channel-rust-{version}.toml"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            if response.url != url:
                raise ValueError(f"Unexpected Rust release manifest redirect: {response.url}")
            metadata = response.read(MANIFEST_LIMIT + 1)
        if len(metadata) > MANIFEST_LIMIT:
            raise ValueError("Rust release manifest exceeds 4 MiB.")
        release = tomllib.loads(metadata.decode("utf-8"))
        if release["manifest-version"] != "2":
            raise ValueError("Unsupported Rust release manifest format.")
        date = release["date"]
        if not isinstance(date, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", date):
            raise ValueError("Invalid Rust release date.")
        sources = []
        commands = []
        for component in ("rustc", "cargo", "rust-std"):
            archive = release["pkg"][component]["target"][target]
            if archive["available"] is not True:
                raise ValueError(f"{component} is unavailable for {target}.")
            expected_url = f"{RUST_DIST}/{date}/{component}-{version}-{target}.tar.xz"
            checksum = archive["xz_hash"]
            if archive["xz_url"] != expected_url:
                raise ValueError(f"Unexpected official {component} archive URL.")
            if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
                raise ValueError(f"Invalid SHA256 for {component}/{target}.")
            sources.append({"type": "archive", "url": expected_url, "sha256": checksum,
                            "dest": component})
            commands.append(f"./{component}/install.sh --prefix={RUST_PREFIX} --disable-ldconfig")
    except (OSError, urllib.error.URLError, UnicodeError, ValueError, KeyError, TypeError) as error:
        raise ValueError(f"Cannot resolve official Rust {version} for {target} from {url}: {error}") from error
    for executable in ("rustc", "cargo"):
        commands.append(f'test "$({RUST_PREFIX}/bin/{executable} --version | cut -d " " -f 2)" = "{version}"')
        commands.append(f"{RUST_PREFIX}/bin/{executable} --version")
    return {
        "name": "rust-toolchain",
        "buildsystem": "simple",
        # SDK eu-strip can corrupt the prebuilt LLVM ELF; keep official tool binaries
        # untouched. The entire prefix is build-only and removed during cleanup.
        "build-options": {"build-args": ["--unshare=network"], "no-debuginfo": True},
        "build-commands": commands,
        "sources": sources,
        "cleanup": ["/lib/slimenrf-build-rust"],
    }


def manifest_template():
    with open(ASSETS / f"{APP_ID}.json", encoding="utf-8") as source:
        return json.load(source)


def config_path(root):
    return root / os.environ.get("SLIMENRF_CONFIG_PATH", "slimenrf-helper.yaml")


def yaml_parser():
    try:
        import yaml
    except ImportError as error:
        raise ValueError("Full Flatpak staging needs PyYAML to resolve firmware_root from YAML. "
                         "Install python-yaml (Arch) or python3-yaml (Debian/Ubuntu), or set "
                         "SLIMENRF_FIRMWARE_ROOT explicitly. No automatic pip/install is performed.") from error
    return yaml


def check_tools(no_firmware=False):
    if sys.platform != "linux":
        raise ValueError("Flatpak packaging requires native Linux flatpak, flatpak-builder and Cargo.")
    toml_parser()
    missing = [tool for tool in ("cargo", "flatpak", "flatpak-builder") if not shutil.which(tool)]
    if missing:
        raise ValueError(f"Missing Flatpak build prerequisites: {', '.join(missing)}. "
                         "Install flatpak and flatpak-builder using your distribution package manager "
                         "(Arch: sudo pacman -S --needed flatpak flatpak-builder; "
                         "Debian/Ubuntu: sudo apt install flatpak flatpak-builder). Cargo must be on PATH.")
    manifest = manifest_template()
    architecture = subprocess.run(["flatpak", "--default-arch"], check=True, text=True,
                                  capture_output=True).stdout.strip()
    if architecture not in ("x86_64", "aarch64"):
        raise ValueError(f"Freedesktop SDK Flatpak packaging supports x86_64/aarch64, not {architecture!r}.")
    branch = manifest["runtime-version"]
    refs = [f"{name}/{architecture}/{branch}" for name in
            (manifest["runtime"], manifest["sdk"])]
    absent = [ref for ref in refs if subprocess.run(
        ["flatpak", "info", "--show-ref", f"runtime/{ref}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode]
    if absent:
        install = " ".join(shlex.quote(ref) for ref in refs)
        raise ValueError("Install the Freedesktop runtime and SDK first.\n"
                         f"  Missing: {', '.join(absent)}\n"
                         f"  flatpak remote-add --user --if-not-exists flathub {RUNTIME_REPO}\n"
                         f"  flatpak install --user flathub {install}\n"
                         "Packaging never installs runtimes or changes system permissions.")
    if not no_firmware and "SLIMENRF_FIRMWARE_ROOT" not in os.environ:
        yaml_parser()
    return architecture


def firmware_root(root, selected_config, config_bytes):
    override = os.environ.get("SLIMENRF_FIRMWARE_ROOT")
    if override is not None:
        return root / override
    yaml = yaml_parser()
    try:
        # BaseLoader retains scalar text (YAML 1.1 yes/on/0123 must not become bool/int).
        # The SDK's existing serde_yaml resource builder remains the config validator.
        document = yaml.compose(config_bytes, Loader=yaml.BaseLoader)
    except yaml.YAMLError as error:
        raise ValueError(f"Cannot read firmware_root in {selected_config}: {error}") from error
    if not isinstance(document, yaml.MappingNode):
        raise ValueError(f"Configuration must be a YAML mapping: {selected_config}")
    values = [value for key, value in document.value
              if isinstance(key, yaml.ScalarNode) and key.value == "firmware_root"]
    if len(values) != 1 or not isinstance(values[0], yaml.ScalarNode) or not values[0].value.strip():
        raise ValueError(f"Configuration needs one nonempty scalar firmware_root: {selected_config}")
    return selected_config.parent / values[0].value


def copy_file(source, destination, open_regular):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with open_regular(source) as input_file, open(destination, "xb") as output:
        shutil.copyfileobj(input_file, output)
        os.fchmod(output.fileno(), os.fstat(input_file.fileno()).st_mode & 0o777)


def copy_tree(source, destination, open_regular):
    # Do not follow directory symlinks into caches, parent firmware trees or user data.
    if source.is_symlink() or not source.is_dir():
        raise ValueError(f"Source tree must be a real directory: {source}")
    destination.mkdir(parents=True, exist_ok=True)
    for entry in sorted(source.iterdir()):
        if entry.name in EXCLUDED:
            continue
        status = entry.lstat()
        if stat.S_ISDIR(status.st_mode):
            copy_tree(entry, destination / entry.name, open_regular)
        elif stat.S_ISREG(status.st_mode):
            copy_file(entry, destination / entry.name, open_regular)
        else:
            raise ValueError(f"Unsupported source entry (symlinks are not staged): {entry}")


def stage_sources(root, stage, no_firmware, open_regular):
    source = stage / "source"
    for relative in SOURCE_FILES:
        copy_file(root / relative, source / relative, open_regular)
    for relative in SOURCE_TREES:
        copy_tree(root / relative, source / relative, open_regular)
    selected_config = config_path(root)
    # Explicit config overrides may be symlinks; retain their original parent for root resolution.
    with open_regular(selected_config.resolve(strict=True)) as configuration:
        config_bytes = configuration.read()
    (source / "slimenrf-helper.yaml").write_bytes(config_bytes)
    if not no_firmware:
        selected_root = firmware_root(root, selected_config, config_bytes)
        if not selected_root.is_dir():
            raise ValueError(f"Firmware directory is missing: {selected_root}; restore inputs or set "
                             "SLIMENRF_FIRMWARE_ROOT (relative to the application directory).")
        destination = source / "flatpak-firmwares"
        destination.mkdir()
        count = 0
        for entry in sorted(selected_root.iterdir()):
            # Match crates/app/build_support/resources.rs: regular top-level UF2/HEX/BIN/ZIP only,
            # case-insensitive extensions, no nested payloads or symlinked firmware.
            if stat.S_ISREG(entry.lstat().st_mode) and entry.suffix.lower() in (".uf2", ".hex", ".bin", ".zip"):
                copy_file(entry, destination / entry.name, open_regular)
                count += 1
        if not count:
            raise ValueError(f"{selected_root} contains no top-level UF2/HEX/BIN/ZIP firmware files.")
        print(f"Flatpak staging: {count} firmware inputs from {selected_root}", flush=True)
    copy_file(ASSETS / f"{APP_ID}.desktop", stage / f"{APP_ID}.desktop", open_regular)
    copy_file(root / "scripts/appimage/slimenrf-helper.svg", stage / f"{APP_ID}.svg", open_regular)
    # Cargo emits replacement configuration, including any locked git sources, on stdout.
    # Relative vendor destination keeps the resulting config relocatable inside the SDK.
    vendored = subprocess.run(["cargo", "vendor", "--locked", "--versioned-dirs", "vendor"],
                             cwd=source, check=True, text=True, stdout=subprocess.PIPE)
    (source / ".cargo").mkdir()
    (source / ".cargo/config.toml").write_text(vendored.stdout, encoding="utf-8")
    return source


def cache_path(root):
    override = os.environ.get("SLIMENRF_FLATPAK_CACHE")
    if override:
        cache = Path(override)
        if not cache.is_absolute():
            cache = root / cache
    else:
        base = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
        if not base.is_absolute():
            raise ValueError("XDG_CACHE_HOME must be absolute for Flatpak caching.")
        cache = base / "slimenrf-helper/flatpak"
    cache.mkdir(parents=True, exist_ok=True)
    return cache.resolve()


@contextlib.contextmanager
def build(root, no_firmware, version, *, open_regular):
    architecture = check_tools(no_firmware)
    cache = cache_path(root) / architecture
    cache.mkdir(parents=True, exist_ok=True)
    # The SDK target must never be mixed with the native Arch/Windows Cargo ABI cache.
    target = cache / "sdk-target"
    target.mkdir(exist_ok=True)
    jobs = os.environ.get("SLIMENRF_FLATPAK_JOBS", os.environ.get("CARGO_BUILD_JOBS", "2"))
    if not jobs.isdecimal() or int(jobs) < 1:
        raise ValueError("SLIMENRF_FLATPAK_JOBS/CARGO_BUILD_JOBS must be a positive integer.")
    variant = "no-firmware" if no_firmware else "full"
    # Serialize shared builder/OSTree/SDK Cargo caches, including across feature variants.
    with open(cache / "build.lock", "a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        with tempfile.TemporaryDirectory(prefix="stage-", dir=cache) as temporary:
            stage = Path(temporary)
            stage_sources(root, stage, no_firmware, open_regular)
            toolchain = rust_toolchain_module(stage / "source", architecture)
            manifest = manifest_template()
            module = manifest["modules"][0]
            options = module["build-options"]
            options["env"].update({"CARGO_TARGET_DIR": str(target), "CARGO_BUILD_JOBS": jobs})
            options["build-args"] = [f"--filesystem={target}", "--unshare=network"]
            if no_firmware:
                module["build-commands"][0] += " --no-default-features"
            manifest["modules"].insert(0, toolchain)
            # This is a local compatibility bundle, not a Flathub submission with AppStream metadata.
            manifest_path = stage / f"{APP_ID}.json"
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
            state = cache / "builder-state"
            output = cache / f"build-{variant}"
            repository = cache / f"repo-{variant}"
            print(f"Flatpak SDK build: {architecture}, {variant}; isolated cache: {cache}", flush=True)
            subprocess.run(["flatpak-builder", "--user", f"--arch={architecture}",
                            "--download-only", f"--state-dir={state}",
                            str(output), str(manifest_path)], check=True)
            subprocess.run(["flatpak-builder", "--user", f"--arch={architecture}",
                            "--force-clean", "--disable-download", f"--state-dir={state}",
                            f"--repo={repository}", str(output), str(manifest_path)], check=True)
            bundle = stage / f"{APP_ID}-{version}.flatpak"
            subprocess.run(["flatpak", "build-bundle", f"--arch={architecture}",
                            f"--runtime-repo={RUNTIME_REPO}", str(repository), str(bundle),
                            APP_ID, "master"], check=True)
            yield bundle, architecture


if __name__ == "__main__":
    try:
        if sys.argv[1:] not in (["--check-tools"], ["--check-tools", "--no-firmware"]):
            raise ValueError("Use package.sh --format flatpak [--no-firmware] [--force]. "
                             "This helper only exposes --check-tools [--no-firmware] directly.")
        check_tools("--no-firmware" in sys.argv[1:])
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Flatpak setup failed: {error}", file=sys.stderr)
        sys.exit(1)
