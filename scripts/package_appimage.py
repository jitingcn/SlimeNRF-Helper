#!/usr/bin/env python3
"""Stage one Cargo-selected ELF using checksum-pinned, locally supplied AppImage tools."""

import contextlib
import hashlib
import os
from pathlib import Path
import platform
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile


# Digests published by the official GitHub release APIs, not learned from local downloads:
# https://api.github.com/repos/linuxdeploy/linuxdeploy/releases/tags/1-alpha-20251107-1
# https://api.github.com/repos/AppImage/appimagetool/releases/tags/1.9.1
# Changing a pin is an explicit source change. Never resolve "latest" or "continuous" here.
TOOLS = {
    "linuxdeploy": {
        "repository": "linuxdeploy/linuxdeploy", "release": "1-alpha-20251107-1",
        "environment": "SLIMENRF_LINUXDEPLOY",
        "assets": {
            "x64": ("x86_64", "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"),
            "arm64": ("aarch64", "620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff"),
            "x86": ("i386", "a2a88d142aac42db779483ca07c10dbf318b27f514691107fc88a202faae17b5"),
            "arm": ("armhf", "e359161979fa4bee50b92ce7102fb510299caebf34f711d983fba7a8f4bb1c2e"),
        },
    },
    "appimagetool": {
        "repository": "AppImage/appimagetool", "release": "1.9.1",
        "environment": "SLIMENRF_APPIMAGETOOL",
        "assets": {
            "x64": ("x86_64", "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"),
            "arm64": ("aarch64", "f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158"),
            "x86": ("i686", "7ad9ff47c203aae0149b18f6df9e3018b2e2f470ea644a0413e3ded39e9e3bdb"),
            "arm": ("armhf", "42b61cba5495d8aaf418a5c9a015a49b85ad92efabcbd3c341f1540440e4e23d"),
        },
    },
}

# dlopen dependencies are not all visible in DT_NEEDED. Force these client libraries
# into the bundle, together with the closure linuxdeploy discovers. Font configuration,
# fonts and XKB data remain host data; do not override FONTCONFIG_PATH or XKB_CONFIG_ROOT.
CLIENT_LIBRARIES = (
    "libxkbcommon.so.0", "libxkbcommon-x11.so.0", "libfontconfig.so.1",
    "libfreetype.so.6", "libdbus-1.so.3",
)

# Keep the host loader/libc, graphics stack, Wayland client ABI and udev coherent.
# Modern host Mesa can require Wayland symbols absent from build-host client libraries.
# WGPU loads host Vulkan/EGL; their driver dependencies must also resolve on the host.
HOST_LIBRARIES = (
    "ld-linux*.so*", "ld-musl*.so*", "libc.so*", "libm.so*", "libpthread.so*",
    "libdl.so*", "librt.so*", "libresolv.so*", "libnss_*.so*", "libutil.so*",
    "libanl.so*", "libBrokenLocale.so*", "libudev.so*", "libsystemd.so*",
    "libEGL.so*", "libGL.so*", "libGLX*.so*", "libGLES*.so*", "libOpenGL.so*",
    "libwayland*.so*",
    "libGLdispatch.so*", "libgbm.so*", "libdrm*.so*", "libvulkan*.so*",
    "libnvidia*.so*", "libcuda*.so*", "*_dri.so",
)


def host_architecture():
    machine = platform.machine().lower()
    architecture = {
        "x86_64": "x64", "amd64": "x64", "aarch64": "arm64", "arm64": "arm64",
        "i386": "x86", "i486": "x86", "i586": "x86", "i686": "x86",
        "armv7l": "arm", "armv8l": "arm",
    }.get(machine)
    if architecture is None:
        raise ValueError(f"No pinned AppImage build tools for this host architecture: {machine}")
    return architecture


def tool_spec(name, architecture):
    tool = TOOLS[name]
    asset_architecture, digest = tool["assets"][architecture]
    filename = f"{name}-{asset_architecture}.AppImage"
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    if not cache.is_absolute():
        raise ValueError("XDG_CACHE_HOME must be absolute for AppImage tool discovery.")
    override = os.environ.get(tool["environment"])
    path = Path(override) if override else (
        cache / "slimenrf-helper" / "appimage-tools" / tool["release"] / filename)
    if not path.is_absolute():
        raise ValueError(f"{tool['environment']} must name an absolute regular-file path.")
    url = (f"https://github.com/{tool['repository']}/releases/download/"
           f"{tool['release']}/{filename}")
    return path, digest, url


def setup_message(name, path, digest, url):
    variable = TOOLS[name]["environment"]
    return (f"{name} requires the pinned official {TOOLS[name]['release']} AppImage.\n"
            f"  Download: {url}\n  SHA256: {digest}\n"
            f"  Save as: {path}\n  Or set {variable} to an absolute regular-file path.\n"
            "  No automatic download or system installation is performed.\n"
            f"  Example: curl --fail --location --proto '=https' --tlsv1.2 "
            f"{shlex.quote(url)} --output {shlex.quote(str(path))}\n"
            "  Create the parent directory first; packaging checks SHA256 before executing anything.")


def copy_verified_tool(name, architecture, destination=None):
    path, digest, url = tool_spec(name, architecture)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as source:
            if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                raise ValueError(f"Not a regular tool file: {path}")
            checksum = hashlib.sha256()
            # Verify the bytes actually staged, so changing the cache during a build cannot
            # replace a verified binary between hashing and execution.
            with contextlib.ExitStack() as stack:
                output = stack.enter_context(open(destination, "xb")) if destination else None
                while chunk := source.read(1024 * 1024):
                    checksum.update(chunk)
                    if output:
                        output.write(chunk)
            if checksum.hexdigest() != digest:
                raise ValueError(f"SHA256 mismatch for {path}; refusing to execute unpinned bytes.")
    except (OSError, ValueError) as error:
        raise ValueError(f"{error}\n{setup_message(name, path, digest, url)}") from error
    if destination:
        destination.chmod(0o755)
    return path


def client_libraries():
    ldconfig = shutil.which("ldconfig")
    if ldconfig is None:
        for candidate in ("/sbin/ldconfig", "/usr/sbin/ldconfig"):
            if Path(candidate).is_file():
                ldconfig = candidate
                break
    if ldconfig is None:
        raise ValueError("AppImage dependency discovery requires ldconfig (from the host libc tools).")
    output = subprocess.run([ldconfig, "-p"], check=True, text=True, capture_output=True).stdout
    candidates = {}
    for line in output.splitlines():
        fields = line.split()
        if len(fields) >= 4 and fields[-2] == "=>":
            candidates.setdefault(fields[0], []).append(Path(fields[-1]))
    # Match the actual host ELF machine/class rather than assuming ldconfig's first
    # cache entry is native on a multilib host. Do not load libraries into Python.
    with open("/proc/self/exe", "rb") as executable:
        header = executable.read(20)
    native = header[4:6], header[18:20]
    libraries = []
    missing = []
    for name in CLIENT_LIBRARIES:
        for path in candidates.get(name, ()):
            with open(path, "rb") as library:
                header = library.read(20)
            if header[:4] == b"\x7fELF" and (header[4:6], header[18:20]) == native:
                libraries.append(path)
                break
        else:
            missing.append(name)
    if missing:
        raise ValueError("AppImage client-library prerequisites missing from the native ldconfig cache: "
                         + ", ".join(missing)
                         + ". Install the corresponding Wayland, xkbcommon, fontconfig, FreeType "
                           "and D-Bus packages using your distribution's package manager.")
    return libraries


def check_tools():
    if sys.platform != "linux":
        raise ValueError("AppImage packaging requires a native Linux environment.")
    architecture = host_architecture()
    errors = []
    for name in TOOLS:
        try:
            copy_verified_tool(name, architecture)
        except ValueError as error:
            errors.append(str(error))
    if shutil.which("desktop-file-validate") is None:
        errors.append("desktop-file-validate is required (the desktop-file-utils package).")
    if shutil.which("file") is None:
        errors.append("file is required by appimagetool (the file package).")
    try:
        client_libraries()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        errors.append(str(error))
    if errors:
        raise ValueError("\n\n".join(errors))


@contextlib.contextmanager
def build(root, source, architecture, version):
    if architecture != host_architecture():
        raise ValueError("AppImage dependency bundling requires a native build: "
                         f"Cargo ELF is {architecture}, host is {host_architecture()}.")
    check_tools()
    assets = root / "scripts" / "appimage"
    environment = os.environ.copy()
    environment["APPIMAGE_EXTRACT_AND_RUN"] = "1"
    environment["ARCH"] = TOOLS["appimagetool"]["assets"][architecture][0]
    environment["VERSION"] = version
    environment["LC_ALL"] = "C"
    # Cargo already strips the Release executable. The pinned bundler's older strip
    # cannot parse modern DT_RELR sections; preserve copied client-library bytes.
    environment["NO_STRIP"] = "1"
    # Never let inherited deployment settings silently defeat the declared policy.
    environment.pop("LINUXDEPLOY_EXCLUDED_LIBRARIES", None)
    environment.pop("DISABLE_COPYRIGHT_FILES_DEPLOYMENT", None)
    with tempfile.TemporaryDirectory(prefix="slimenrf-appimage-") as temporary:
        stage = Path(temporary)
        tools = {}
        images = {}
        for name in TOOLS:
            directory = stage / name
            directory.mkdir()
            image = directory / f"{name}.AppImage"
            copy_verified_tool(name, architecture, image)
            images[name] = image
            # Explicit extraction needs no /dev/fuse and does not share a mutable extraction cache.
            subprocess.run([str(image), "--appimage-extract"], cwd=directory,
                           env=environment, check=True, stdout=subprocess.DEVNULL)
            tools[name] = directory / "squashfs-root" / "AppRun"

        # Reuse the pinned appimagetool AppImage's own matching type-2 runtime. Otherwise
        # appimagetool downloads a mutable continuous runtime even with pinned tool bytes.
        offset = subprocess.run([str(images["appimagetool"]), "--appimage-offset"],
                                env=environment, check=True, capture_output=True, text=True).stdout.strip()
        if not offset.isdecimal() or not 0 < int(offset) < images["appimagetool"].stat().st_size:
            raise ValueError("Pinned appimagetool did not report a valid type-2 runtime offset.")
        runtime = stage / "runtime"
        with open(images["appimagetool"], "rb") as image, open(runtime, "xb") as output:
            remaining = int(offset)
            while remaining:
                chunk = image.read(min(remaining, 1024 * 1024))
                if not chunk:
                    raise ValueError("Truncated pinned AppImage runtime.")
                output.write(chunk)
                remaining -= len(chunk)

        appdir = stage / "SlimeNRF.AppDir"
        executable = stage / "slimenrf-helper"
        with open(executable, "xb") as output:
            source.seek(0)
            shutil.copyfileobj(source, output)
        executable.chmod(0o755)
        command = [str(tools["linuxdeploy"]), "--appdir", str(appdir),
                   "--executable", str(executable),
                   "--desktop-file", str(assets / "slimenrf-helper.desktop"),
                   "--icon-file", str(assets / "slimenrf-helper.svg"),
                   "--custom-apprun", str(assets / "AppRun")]
        for pattern in HOST_LIBRARIES:
            command += ["--exclude-library", pattern]
        for library in client_libraries():
            command += ["--library", str(library)]
        subprocess.run(command, cwd=stage, env=environment, check=True)
        # Check the dlopen payload too: successful direct DT_NEEDED collection alone is insufficient.
        for library in CLIENT_LIBRARIES:
            if not (appdir / "usr" / "lib" / library).is_file():
                raise ValueError(f"linuxdeploy omitted required client library: {library}")
        licenses = appdir / "usr" / "share" / "licenses" / "slimenrf-helper"
        licenses.mkdir(parents=True, exist_ok=True)
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.txt"):
            shutil.copyfile(root / name, licenses / name)
        subprocess.run(["desktop-file-validate", str(appdir / "slimenrf-helper.desktop")],
                       env=environment, check=True)
        image = stage / "application.AppImage"
        subprocess.run([str(tools["appimagetool"]), "--runtime-file", str(runtime),
                        "--no-appstream", str(appdir), str(image)],
                       cwd=stage, env=environment, check=True)
        yield image


if __name__ == "__main__":
    try:
        if sys.argv[1:] != ["--check-tools"]:
            raise ValueError("Usage: package_appimage.py --check-tools (build through package.sh)")
        check_tools()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"AppImage setup failed: {error}", file=sys.stderr)
        sys.exit(1)
