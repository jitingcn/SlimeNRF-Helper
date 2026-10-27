#!/usr/bin/env python3
"""Select Cargo's app executable, inspect its ELF, and publish without following links."""

import contextlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import stat
import struct
import subprocess
import sys


APP = "slimenrf-helper"


def regular_file(path, *, directory=None):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    stream = os.fdopen(descriptor, "rb")
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        stream.close()
        raise ValueError(f"Not a regular file: {path}")
    return stream


def elf_identity(stream):
    header = stream.read(64)
    if len(header) != 64 or header[:4] != b"\x7fELF":
        raise ValueError("Cargo's application executable is not an ELF file.")
    elf_class, encoding, abi = header[4], header[5], header[7]
    if elf_class not in (1, 2) or encoding not in (1, 2) or abi not in (0, 3):
        raise ValueError("Unsupported or non-Linux ELF identity.")
    order = "<" if encoding == 1 else ">"
    elf_type, machine = struct.unpack_from(order + "HH", header, 16)
    if elf_type not in (2, 3):
        raise ValueError("Cargo's ELF is not an executable or position-independent executable.")
    architectures = {
        (3, 1): "x86", (62, 2): "x64", (40, 1): "arm", (183, 2): "arm64",
        (20, 1): "ppc", (21, 2): "ppc64le" if encoding == 1 else "ppc64",
        (22, 2): "s390x", (243, 1): "riscv32", (243, 2): "riscv64",
        (258, 2): "loongarch64",
    }
    architecture = architectures.get((machine, elf_class))
    if architecture is None:
        raise ValueError(f"Unsupported Linux ELF architecture: machine={machine}, class={elf_class}")
    if elf_class == 2:
        offset = struct.unpack_from(order + "Q", header, 32)[0]
        entry_size, count = struct.unpack_from(order + "HH", header, 54)
        minimum_entry_size = 56
    else:
        offset = struct.unpack_from(order + "I", header, 28)[0]
        entry_size, count = struct.unpack_from(order + "HH", header, 42)
        minimum_entry_size = 32
    size = os.fstat(stream.fileno()).st_size
    if not count or entry_size < minimum_entry_size or offset + entry_size * count > size:
        raise ValueError("Invalid ELF program-header table.")
    dynamic = False
    for index in range(count):
        stream.seek(offset + index * entry_size)
        dynamic |= struct.unpack(order + "I", stream.read(4))[0] in (2, 3)
    stream.seek(0)
    return architecture, dynamic


def same_bytes(left, right):
    left.seek(0)
    right.seek(0)
    while True:
        chunk = left.read(1024 * 1024)
        if chunk != right.read(1024 * 1024):
            return False
        if not chunk:
            return True


def file_identity(status):
    return status.st_dev, status.st_ino, status.st_size, status.st_mtime_ns, status.st_ctime_ns


def publish(source, directory, name, force, *, executable=True):
    try:
        existing = regular_file(name, directory=directory)
    except FileNotFoundError:
        existing = None
    previous = None
    if existing is not None:
        with existing:
            previous = file_identity(os.fstat(existing.fileno()))
            if same_bytes(source, existing):
                if executable and not os.fstat(existing.fileno()).st_mode & 0o111:
                    raise ValueError("Identical package is not executable; fix its permissions before packaging.")
                return "Identical package already exists"
        if not force:
            raise ValueError(f"Refusing to overwrite a different existing package: {name} (use --force)")

    temporary = f".{name}.{secrets.token_hex(12)}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory)
    try:
        with os.fdopen(descriptor, "wb") as output:
            source.seek(0)
            shutil.copyfileobj(source, output)
            mode = os.fstat(source.fileno()).st_mode & 0o777
            os.fchmod(output.fileno(), mode | 0o111 if executable else mode & ~0o111)
        if previous is None:
            # An exclusive hard link publishes atomically without overwriting a new collision.
            os.link(temporary, name, src_dir_fd=directory, dst_dir_fd=directory,
                    follow_symlinks=False)
            return "Packaged single executable" if executable else "Packaged Flatpak bundle"
        current = os.stat(name, dir_fd=directory, follow_symlinks=False)
        if not stat.S_ISREG(current.st_mode) or file_identity(current) != previous:
            raise ValueError(f"Package destination changed while publishing: {name}")
        os.replace(temporary, name, src_dir_fd=directory, dst_dir_fd=directory)
        return "Replaced package executable" if executable else "Replaced Flatpak bundle"
    finally:
        try:
            os.unlink(temporary, dir_fd=directory)
        except FileNotFoundError:
            pass


def publish_package(root, source, name, force, *, executable=True):
    dist = root / "dist"
    try:
        dist.mkdir()
    except FileExistsError:
        pass
    # O_NOFOLLOW also rejects a symlinked dist directory, including dangling links.
    directory = os.open(dist, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        result = publish(source, directory, name, force, executable=executable)
    finally:
        os.close(directory)
    print(f"{result}: {dist / name}")


def main():
    root = Path(sys.argv[1]).resolve()
    with open(sys.argv[2], encoding="utf-8") as metadata_file:
        metadata = json.load(metadata_file)
    packages = [package for package in metadata["packages"]
                if package["name"] == APP
                and Path(package["manifest_path"]).resolve() == root / "crates/app/Cargo.toml"
                and package["id"] in metadata["workspace_members"]]
    if len(packages) != 1:
        raise ValueError("Cargo metadata must identify exactly one workspace slimenrf-helper application.")
    package = packages[0]
    version = package["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)*", version):
        raise ValueError(f"Unsafe package version: {version!r}")
    package_format = sys.argv[6] if len(sys.argv) > 6 else "elf"
    if package_format not in ("elf", "appimage", "flatpak"):
        raise ValueError(f"Unknown package format: {package_format}")
    if package_format == "flatpak":
        import package_flatpak
        no_firmware = sys.argv[4] == "1"
        with package_flatpak.build(root, no_firmware, version, open_regular=regular_file) as (bundle, architecture):
            variant = "-no-firmware" if no_firmware else ""
            name = f"{APP}-{version}-linux-{architecture}{variant}.flatpak"
            with regular_file(bundle) as source:
                publish_package(root, source, name, sys.argv[5] == "1", executable=False)
        print("Flatpak: source-built inside the Freedesktop SDK; runtimes are not included in the bundle.")
        print("Full and no-firmware variants use the same app ID/branch and replace each other.")
        print("Compatibility permissions include host files and all devices; host ACLs, udev, BlueZ "
              "and real writable FAT/block UF2 mounts remain required.")
        return
    executables = set()
    with open(sys.argv[3], encoding="utf-8") as artifacts:
        for line in artifacts:
            message = json.loads(line)
            if (message.get("reason") == "compiler-artifact"
                    and message.get("package_id") == package["id"]
                    and message["target"]["name"] == APP
                    and "bin" in message["target"]["kind"]
                    and not message["profile"]["test"]
                    and message.get("executable")):
                executables.add(message["executable"])
    if len(executables) != 1:
        raise ValueError("The successful Release build must report exactly one application executable.")
    source_path = Path(executables.pop())
    if not source_path.is_absolute():
        source_path = root / source_path
    with contextlib.ExitStack() as staging, regular_file(source_path) as source:
        architecture, dynamic = elf_identity(source)
        variant = "-no-firmware" if sys.argv[4] == "1" else ""
        name = f"{APP}-{version}-linux-{architecture}{variant}"
        if package_format == "appimage":
            import package_appimage
            image = staging.enter_context(
                package_appimage.build(root, source, architecture, version))
            source = staging.enter_context(regular_file(image))
            image_architecture, _ = elf_identity(source)
            if image_architecture != architecture:
                raise ValueError("AppImage architecture differs from Cargo's executable.")
            name += ".AppImage"
        publish_package(root, source, name, sys.argv[5] == "1")
        if package_format == "appimage":
            print("AppImage: bundled desktop/client libraries; host libc, graphics drivers/loaders, "
                  "fonts, libudev, and desktop/device services are required.")
            print("Without FUSE, run the AppImage with --appimage-extract-and-run.")
        elif dynamic:
            print("Dynamic Linux ELF: host system libraries/loader are required; no system libraries were copied.")
        else:
            print("Linux ELF has no dynamic program headers; no system libraries were copied.")
        print("This package does not promise compatibility with every Linux distribution or desktop session.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, struct.error, subprocess.CalledProcessError) as error:
        print(f"Packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
