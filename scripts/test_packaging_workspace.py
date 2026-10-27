"""Exercise relocated workspace packaging without invoking Cargo or Flatpak."""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parent
if sys.platform.startswith("linux"):
    spec = importlib.util.spec_from_file_location("package_flatpak", SCRIPTS / "package_flatpak.py")
    flatpak = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(flatpak)


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux package helpers require Linux file APIs")
class WorkspacePackagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="workspace packaging ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def put(self, relative, content=b"fixture\n"):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def package(self, manifest_path, members, *, include_decoy=False):
        executable = self.put("target/release/slimenrf-helper", bytes(120))
        header = bytearray(120)
        header[:8] = b"\x7fELF\x02\x01\x01\x00"
        struct.pack_into("<HH", header, 16, 2, 62)
        struct.pack_into("<Q", header, 32, 64)
        struct.pack_into("<HH", header, 54, 56, 1)
        struct.pack_into("<I", header, 64, 1)
        executable.write_bytes(header)
        executable.chmod(0o755)
        app = {"name": "slimenrf-helper", "version": "1.2.3", "id": "app-id",
               "manifest_path": str(self.root / manifest_path)}
        packages = [app]
        if include_decoy:
            packages.append({"name": "slimenrf-helper", "version": "9.9.9", "id": "other-id",
                             "manifest_path": str(self.root / "Cargo.toml")})
        metadata = self.put("metadata.json", json.dumps({"packages": packages,
                            "workspace_members": members}).encode())
        artifact = {"reason": "compiler-artifact", "package_id": "app-id",
                    "target": {"name": "slimenrf-helper", "kind": ["bin"]},
                    "profile": {"test": False}, "executable": str(executable)}
        artifacts = self.put("artifacts.jsonl", (json.dumps(artifact) + "\n").encode())
        return subprocess.run([sys.executable, str(SCRIPTS / "package-artifact.py"), str(self.root),
                               str(metadata), str(artifacts), "1", "0", "elf"],
                              capture_output=True, text=True)

    def test_package_selects_relocated_workspace_app_and_keeps_artifact_name(self):
        result = self.package("crates/app/Cargo.toml", ["app-id"], include_decoy=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = self.root / "dist/slimenrf-helper-1.2.3-linux-x64-no-firmware"
        self.assertEqual(artifact.read_bytes(), (self.root / "target/release/slimenrf-helper").read_bytes())
        self.assertTrue(artifact.stat().st_mode & 0o111)

    def test_package_rejects_old_location_and_nonmember(self):
        for manifest, members in (("Cargo.toml", ["app-id"]), ("crates/app/Cargo.toml", [])):
            with self.subTest(manifest=manifest, members=members):
                result = self.package(manifest, members)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("workspace slimenrf-helper application", result.stderr)
                self.assertFalse((self.root / "dist").exists())

    def test_flatpak_stages_real_workspace_inputs_without_empty_examples(self):
        for relative in (
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "LICENSE-MIT", "LICENSE-APACHE",
            "scripts/generate_notices.py", "scripts/notices-inputs.json",
            "crates/app/Cargo.toml", "crates/app/build.rs", "crates/app/src/main.rs",
            "crates/app/build_support/notices.rs", "locales/en.yml",
            "crates/recording/Cargo.toml", "crates/recording/build.rs",
            "crates/recording/src/lib.rs", "crates/recording/vendor/vqf/LICENSE",
            "scripts/appimage/slimenrf-helper.svg",
            f"scripts/flatpak/{flatpak.APP_ID}.desktop",
        ):
            self.put(relative)
        self.put("crates/app/src/.cache/private.txt", b"excluded\n")
        self.put("config/selected.yaml", b"firmware_root: ignored\n")
        self.put("firmware/test.UF2", b"firmware\n")
        self.put("firmware/nested/not-staged.uf2", b"nested\n")
        self.assertFalse((self.root / "examples").exists())

        environment = {"SLIMENRF_CONFIG_PATH": "config/selected.yaml", "SLIMENRF_FIRMWARE_ROOT": "firmware"}
        for no_firmware in (True, False):
            with self.subTest(no_firmware=no_firmware):
                stage = self.root / ("no-firmware-stage" if no_firmware else "full-stage")
                stage.mkdir()
                with (mock.patch.dict(os.environ, environment),
                      mock.patch.object(flatpak, "ASSETS", self.root / "scripts/flatpak"),
                      mock.patch.object(flatpak.subprocess, "run", return_value=subprocess.CompletedProcess(
                          [], 0, stdout="[source.crates-io]\nreplace-with = 'vendored-sources'\n"))):
                    source = flatpak.stage_sources(self.root, stage, no_firmware,
                                                   lambda path: open(path, "rb"))
                for relative in ("Cargo.toml", "crates/app/Cargo.toml", "crates/app/build.rs",
                                 "crates/app/src/main.rs", "crates/app/build_support/notices.rs",
                                 "crates/recording/src/lib.rs", "crates/recording/vendor/vqf/LICENSE",
                                 "locales/en.yml", ".cargo/config.toml"):
                    self.assertTrue((source / relative).is_file(), relative)
                self.assertEqual((source / "slimenrf-helper.yaml").read_bytes(), b"firmware_root: ignored\n")
                for absent in ("examples", "src", "build_support", "crates/app/src/.cache"):
                    self.assertFalse((source / absent).exists(), absent)
                firmware = source / "flatpak-firmwares"
                self.assertEqual(firmware.exists(), not no_firmware)
                if not no_firmware:
                    self.assertEqual([path.name for path in firmware.iterdir()], ["test.UF2"])
                    self.assertEqual((firmware / "test.UF2").read_bytes(), b"firmware\n")

    @unittest.skipUnless(importlib.util.find_spec("yaml"), "PyYAML is needed for full Flatpak YAML selection")
    def test_flatpak_yaml_firmware_root_is_relative_to_selected_configuration(self):
        self.put("config/selected.yaml", b"firmware_root: payloads\n")
        with mock.patch.dict(os.environ, {"SLIMENRF_CONFIG_PATH": "config/selected.yaml"}, clear=True):
            selected = flatpak.config_path(self.root)
            self.assertEqual(selected, self.root / "config/selected.yaml")
            self.assertEqual(flatpak.firmware_root(self.root, selected, selected.read_bytes()),
                             self.root / "config/payloads")


if __name__ == "__main__":
    unittest.main()
