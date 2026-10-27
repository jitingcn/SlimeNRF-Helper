"""Behavioral tests for frozen notice inputs and byte-preserving output."""
from __future__ import annotations

import base64
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("generate_notices.py")
SPEC = importlib.util.spec_from_file_location("generate_notices", SCRIPT)
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class NoticeGeneratorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.root.joinpath("scripts").mkdir()
        self.put("scripts/generate_notices.py", SCRIPT.read_bytes())
        self.inputs = {}
        for name, content in {
            "Cargo.toml": b'[workspace.package]\nlicense = "MIT OR Apache-2.0"\n',
            "crates/app/Cargo.toml": b'[package]\nname = "slimenrf-helper"\nlicense.workspace = true\n',
            "crates/recording/Cargo.toml": b'[package]\nlicense.workspace = true\n',
            "Cargo.lock": b'[[package]]\nname = "example"\nversion = "1.2.3"\nchecksum = "abc123"\n',
            "LICENSE-MIT": b"Application MIT terms\n",
            "LICENSE-APACHE": b"Application Apache terms\n",
            **{name: f"Vendor grant {index}\n".encode()
               for index, name in enumerate(notices.VENDOR_LICENSES)},
        }.items():
            self.put(name, content)
            self.inputs[name] = notices.digest(content)
        self.document = {
            "schema": 1,
            "input_sha256": self.inputs,
            "manifest_semantic_sha256": {
                name: notices.semantic_manifest_hash((self.root / name).read_bytes())
                for name in notices.MANIFESTS
            },
            "sources": {},
            "graphs": [{"target": "windows", "packages": ["example@1.2.3"]},
                       {"target": "linux", "packages": ["example@1.2.3"]}],
            "packages": [{"name": "example", "version": "1.2.3", "license": "MIT AND BSD-3-Clause",
                          "checksum": "abc123", "source_archive": "https://example.invalid/example-1.2.3.crate",
                          "target_roles": {"windows": ["code_candidate"], "linux": ["compile_tool"]},
                          "notices": []}],
            "materials": [{"name": "Native kernels", "scope": "Both variants", "notices": []}],
            "scope": ["Fixture scope"], "mpl_source": [],
            "source_instructions": [], "maintainer_review": ["Fixture review boundary"],
        }
        self.upstream = b"Mixed upstream notice\r\nCopyright (c) Example Owner\r\nMIT and BSD terms\r\n"
        self.upstream_key = self.source(self.upstream, {"kind": "published_crate", "url": "https://example.invalid/LICENSE"})
        self.document["packages"][0]["notices"] = [self.upstream_key]
        for name in notices.VENDOR_LICENSES:
            data = (self.root / name).read_bytes()
            self.document["materials"][0]["notices"].append(self.source(data, {
                "kind": "repository", "path": name, "file_sha256": notices.digest(data),
            }))
        self.save()

    def put(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def source(self, data, origin, encoding="utf-8"):
        key = notices.digest(data)
        self.document["sources"][key] = {
            "sha256": key, "bytes": len(data), "encoding": encoding,
            "utf8_sha256": notices.digest(data.decode(encoding).encode("utf-8")),
            "base64": base64.b64encode(data).decode("ascii"), "origins": [origin],
        }
        return key

    def save(self):
        self.put(notices.INPUT, (json.dumps(self.document, ensure_ascii=False) + "\n").encode("utf-8"))

    def load(self):
        manifest, sources, _, _ = notices.load_inputs(self.root)
        return manifest, sources

    def invoke(self, *arguments):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = notices.main(["--root", str(self.root), *arguments])
        self.stdout, self.stderr = stdout.getvalue(), stderr.getvalue()
        return result

    def test_source_only_check_writes_nothing_without_a_root_artifact(self):
        before = {path.relative_to(self.root) for path in self.root.rglob("*") if path.is_file()}
        self.assertEqual(self.invoke("--check"), 0)
        self.assertFalse((self.root / notices.OUTPUT).exists())
        self.assertEqual(
            {path.relative_to(self.root) for path in self.root.rglob("*") if path.is_file()},
            before,
        )

    def test_source_only_check_does_not_consult_or_replace_a_root_artifact(self):
        self.put(notices.OUTPUT, b"unrelated old artifact\n")
        self.assertEqual(self.invoke("--check"), 0)
        self.assertEqual((self.root / notices.OUTPUT).read_bytes(), b"unrelated old artifact\n")

    def test_write_requires_an_explicit_destination(self):
        with self.assertRaises(SystemExit) as error:
            self.invoke()
        self.assertEqual(error.exception.code, 2)
        self.assertFalse((self.root / notices.OUTPUT).exists())

    def test_external_output_with_spaces_is_deterministic_and_byte_preserving(self):
        with tempfile.TemporaryDirectory(prefix="notices output ") as directory:
            output = Path(directory) / "generated notices.txt"
            self.assertEqual(self.invoke("--output", str(output)), 0)
            before = output.read_bytes()
            self.assertEqual(self.invoke("--output", str(output)), 0)
            self.assertEqual(output.read_bytes(), before)
            self.assertEqual(self.invoke("--check", "--output", str(output)), 0)
        self.assertFalse((self.root / notices.OUTPUT).exists())
        self.assertIn(self.upstream, before)
        self.assertEqual(before.count(self.upstream), 1)
        self.assertIn(b"Application MIT terms\n", before)
        self.assertIn(b"Application Apache terms\n", before)

    def test_explicit_check_rejects_missing_output_without_creating_it(self):
        with tempfile.TemporaryDirectory(prefix="notices output ") as directory:
            output = Path(directory) / "missing notices.txt"
            self.assertEqual(self.invoke("--check", "--output", str(output)), 1)
            self.assertFalse(output.exists())

    def test_explicit_check_rejects_stale_output_without_rewriting_it(self):
        with tempfile.TemporaryDirectory(prefix="notices output ") as directory:
            output = Path(directory) / "stale notices.txt"
            output.write_bytes(b"old document\n")
            self.assertEqual(self.invoke("--check", "--output", str(output)), 1)
            self.assertEqual(output.read_bytes(), b"old document\n")

    def test_stale_input_refuses_generation_and_both_check_modes_without_mutation(self):
        output = self.root / "explicit notices.txt"
        self.assertEqual(self.invoke("--output", str(output)), 0)
        before = output.read_bytes()
        self.put("Cargo.toml", b"changed dependency\n")
        for arguments in (
            ("--output", str(output)),
            ("--check",),
            ("--check", "--output", str(output)),
        ):
            with self.subTest(arguments=arguments):
                self.assertEqual(self.invoke(*arguments), 1)
                self.assertIn("Stale input hash: Cargo.toml", self.stderr)
                self.assertEqual(output.read_bytes(), before)
        self.assertFalse((self.root / notices.OUTPUT).exists())

    def test_profile_refresh_preserves_reviewed_content_and_renders_updated_snapshot(self):
        output = self.root / "notices.txt"
        self.assertEqual(self.invoke("--output", str(output)), 0)
        old_output = output.read_bytes()
        snapshot = self.root / notices.INPUT
        before = snapshot.read_bytes()
        for name in notices.MANIFESTS:
            path = self.root / name
            change = b'\n# formatting\n'
            if name == "Cargo.toml":
                change += b'[profile.dev]\nopt-level = 1\n'
            path.write_bytes(path.read_bytes() + change)
        self.assertEqual(self.invoke("--check"), 1)
        self.assertIn("--check --refresh-inputs", self.stderr)
        self.assertEqual(snapshot.read_bytes(), before)
        self.assertEqual(self.invoke("--check", "--refresh-inputs", "--output", str(output)), 1)
        self.assertEqual(snapshot.read_bytes(), before)
        self.assertEqual(output.read_bytes(), old_output)

        self.assertEqual(self.invoke("--refresh-inputs", "--output", str(output), "--cargo-rerun"), 0)
        self.assertIn("Warning: Refreshed", self.stderr)
        self.assertIn("cargo:warning=Refreshed", self.stdout)
        after = snapshot.read_bytes()
        expected = before
        for name in notices.MANIFESTS:
            expected = expected.replace(self.inputs[name].encode(), notices.digest((self.root / name).read_bytes()).encode())
        self.assertEqual(after, expected, "only manifest hash bytes should change")
        self.assertEqual(
            output.read_bytes(),
            old_output.replace(notices.digest(before).encode(), notices.digest(after).encode(), 1),
        )
        self.assertEqual(self.invoke("--check", "--output", str(output)), 0)
        self.assertEqual(snapshot.read_bytes(), after)

    def test_check_can_refresh_without_an_output(self):
        path = self.root / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n[profile.release]\nlto = true\n")
        snapshot = self.root / notices.INPUT
        before = snapshot.read_bytes()
        self.assertEqual(self.invoke("--check", "--refresh-inputs"), 0)
        self.assertNotEqual(snapshot.read_bytes(), before)
        self.assertEqual(self.invoke("--check"), 0)
        self.assertFalse((self.root / notices.OUTPUT).exists())

    def test_check_with_matching_candidate_commits_after_comparison(self):
        path = self.root / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n[profile.dev]\nopt-level = 2\n")
        manifest, sources, original, candidate = notices.load_inputs(self.root, refresh_inputs=True)
        output = self.root / "notices.txt"
        output.write_bytes(notices.render(self.root, manifest, sources, candidate))
        self.assertEqual((self.root / notices.INPUT).read_bytes(), original)
        self.assertEqual(self.invoke("--check", "--output", str(output), "--refresh-inputs"), 0)
        self.assertEqual((self.root / notices.INPUT).read_bytes(), candidate)
        self.assertEqual(self.invoke("--check", "--output", str(output)), 0)

    def test_manifest_fingerprint_uses_canonical_non_profile_toml(self):
        manifest = b'[package]\nname = "example"\n[profile.dev]\nopt-level = 3\n'
        canonical = b'{"package":{"name":"example"}}'
        self.assertEqual(notices.semantic_manifest_hash(manifest), notices.digest(canonical))
        self.assertNotEqual(notices.semantic_manifest_hash(manifest.replace(b"example", b"other")),
                            notices.digest(canonical))

    def test_refresh_rejects_dependency_package_lock_and_vendor_changes(self):
        snapshot = self.root / notices.INPUT
        output = self.root / "notices.txt"
        self.assertEqual(self.invoke("--output", str(output)), 0)
        before_snapshot = snapshot.read_bytes()
        before_output = output.read_bytes()
        for name, addition, message in (
            ("Cargo.toml", b'\n[workspace.dependencies]\nnew = "1"\n', "Manifest semantics changed"),
            ("Cargo.toml", b'\n[workspace.metadata]\nversion = "new"\n', "Manifest semantics changed"),
            ("crates/app/Cargo.toml", b'\n[dependencies]\nnew = "1"\n', "Manifest semantics changed"),
            ("crates/recording/Cargo.toml", b'version = "2.0.0"\n', "Manifest semantics changed"),
            ("Cargo.lock", b"\n# changed lock\n", "Stale input hash: Cargo.lock"),
            (notices.VENDOR_LICENSES[0], b"changed source\n", "Stale input hash:"),
        ):
            with self.subTest(name=name, addition=addition):
                path = self.root / name
                original = path.read_bytes()
                path.write_bytes(original + addition)
                self.assertEqual(self.invoke("--refresh-inputs", "--output", str(output)), 1)
                self.assertIn(message, self.stderr)
                self.assertIn(notices.REVIEW_GUIDE, self.stderr)
                self.assertEqual(snapshot.read_bytes(), before_snapshot)
                self.assertEqual(output.read_bytes(), before_output)
                path.write_bytes(original)

    def test_refresh_never_blesses_corruption_missing_metadata_or_coverage(self):
        path = self.root / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n[profile.dev]\nopt-level = 2\n")
        snapshot = self.root / notices.INPUT
        cases = (
            ("source", lambda: self.document["sources"][self.upstream_key].update(
                base64=base64.b64encode(b"corrupted source").decode()), "Notice source hash or length mismatch"),
            ("coverage", lambda: self.document["packages"].clear(), "Package coverage differs"),
            ("fingerprint", lambda: self.document.pop("manifest_semantic_sha256"), "Missing or invalid reviewed manifest semantic"),
            ("render", lambda: self.document.pop("scope"), "scope"),
        )
        original_document = json.dumps(self.document)
        for label, corrupt, message in cases:
            with self.subTest(case=label):
                self.document = json.loads(original_document)
                corrupt()
                self.save()
                before = snapshot.read_bytes()
                self.assertEqual(self.invoke("--check", "--refresh-inputs"), 1)
                self.assertIn(message, self.stderr)
                self.assertEqual(snapshot.read_bytes(), before)
        self.document = json.loads(original_document)

    def test_missing_or_corrupt_fingerprint_fails_strict_check_read_only(self):
        snapshot = self.root / notices.INPUT
        for fingerprint in (None, "not-a-sha256", "f" * 64):
            with self.subTest(fingerprint=fingerprint):
                if fingerprint is None:
                    self.document.pop("manifest_semantic_sha256", None)
                else:
                    self.document["manifest_semantic_sha256"] = {
                        name: notices.semantic_manifest_hash((self.root / name).read_bytes())
                        for name in notices.MANIFESTS
                    }
                    self.document["manifest_semantic_sha256"]["Cargo.toml"] = fingerprint
                self.save()
                before = snapshot.read_bytes()
                self.assertEqual(self.invoke("--check"), 1)
                self.assertIn("semantic", self.stderr)
                self.assertIn(notices.REVIEW_GUIDE, self.stderr)
                self.assertEqual(snapshot.read_bytes(), before)

    def test_failed_refresh_write_restores_output_and_development_emits_pending(self):
        output = self.root / "notices.txt"
        self.assertEqual(self.invoke("--output", str(output)), 0)
        before_output = output.read_bytes()
        path = self.root / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n[profile.dev]\nopt-level = 1\n")
        snapshot = self.root / notices.INPUT
        before_snapshot = snapshot.read_bytes()
        real_write = notices.write_atomic

        def fail_snapshot(destination, data):
            if destination == snapshot:
                raise PermissionError("fixture cannot refresh reviewed inputs")
            return real_write(destination, data)

        with mock.patch.object(notices, "write_atomic", side_effect=fail_snapshot):
            self.assertEqual(self.invoke("--refresh-inputs", "--output", str(output)), 1)
            self.assertEqual(snapshot.read_bytes(), before_snapshot)
            self.assertEqual(output.read_bytes(), before_output)
            self.assertEqual(self.invoke("--refresh-inputs", "--development", "--output", str(output)), 0)
        self.assertEqual(snapshot.read_bytes(), before_snapshot)
        self.assertIn(b"DEVELOPMENT LICENSE REVIEW PENDING", output.read_bytes())
        self.assertIn("not redistribution-ready", self.stderr)
        self.assertNotIn(self.upstream, output.read_bytes())

    def test_output_failure_never_refreshes_input(self):
        path = self.root / "Cargo.toml"
        path.write_bytes(path.read_bytes() + b"\n[profile.dev]\nopt-level = 1\n")
        snapshot = self.root / notices.INPUT
        before = snapshot.read_bytes()
        output = self.root / "missing directory" / "notices.txt"
        self.assertEqual(self.invoke("--development", "--refresh-inputs", "--output", str(output)), 1)
        self.assertEqual(snapshot.read_bytes(), before)
        self.assertFalse(output.exists())

    def test_development_diagnostic_is_not_a_notice_inventory(self):
        snapshot = self.root / notices.INPUT
        self.document["sources"][self.upstream_key]["base64"] = base64.b64encode(b"unreviewed").decode()
        self.save()
        before = snapshot.read_bytes()
        output = self.root / "notices.txt"
        self.assertEqual(self.invoke("--refresh-inputs", "--output", str(output)), 1)
        self.assertFalse(output.exists())
        self.assertEqual(self.invoke("--development", "--refresh-inputs", "--output", str(output), "--cargo-rerun"), 0)
        pending = output.read_bytes()
        self.assertIn(b"DEVELOPMENT LICENSE REVIEW PENDING", pending)
        self.assertIn(b"not redistribution-ready", pending)
        self.assertIn(b"notice inventory is unavailable", pending)
        self.assertNotIn(self.upstream, pending)
        self.assertNotIn(b"unreviewed", pending)
        self.assertNotIn(b"Package index", pending)
        self.assertIn("cargo:warning=DEVELOPMENT LICENSE REVIEW PENDING", self.stdout)
        self.assertIn("Notice source hash or length mismatch", self.stderr)
        self.assertEqual(snapshot.read_bytes(), before)
        with self.assertRaises(SystemExit) as error:
            self.invoke("--check", "--development")
        self.assertEqual(error.exception.code, 2)

    def test_pending_diagnostic_cannot_overwrite_reviewed_inputs(self):
        snapshot = self.root / notices.INPUT
        self.put(notices.INPUT, b"{ invalid json")
        for name in (notices.INPUT, *notices.VENDOR_LICENSES):
            with self.subTest(input=name):
                path = self.root / name
                before = path.read_bytes()
                self.assertEqual(self.invoke("--development", "--output", str(path)), 1)
                self.assertEqual(path.read_bytes(), before)
                self.assertIn("must not overwrite", self.stderr)

    @unittest.skipUnless(shutil.which("rustc"), "native Rust compiler required for the real build wrapper")
    def test_real_build_wrapper_distinguishes_debug_from_release(self):
        source = self.root / "harness.rs"
        binary = self.root / ("harness.exe" if os.name == "nt" else "harness")
        module = SCRIPT.resolve().parents[1] / "crates" / "app" / "build_support" / "notices.rs"
        source.write_text(
            f'#[path = r#"{module}"#] mod notices;\n'
            'fn main() { if let Err(error) = notices::build_notices() '
            '{ eprintln!("{error}"); std::process::exit(1); } }\n',
            encoding="utf-8",
        )
        result = subprocess.run(["rustc", "--edition=2021", str(source), "-o", str(binary)],
                                capture_output=True, text=True, errors="replace")
        self.assertEqual(result.returncode, 0, result.stderr)
        output_dir = self.root / "build output"
        output_dir.mkdir()
        output = output_dir / notices.OUTPUT
        env = os.environ | {"CARGO_MANIFEST_DIR": str(self.root / "crates/app"), "OUT_DIR": str(output_dir),
                            "SLIMENRF_PYTHON": sys.executable}
        manifest = self.root / "Cargo.toml"
        manifest.write_bytes(manifest.read_bytes() + b"\n[profile.dev]\nopt-level = 1\n")
        snapshot = self.root / notices.INPUT
        before = snapshot.read_bytes()
        debug = subprocess.run([str(binary)], env=env | {"PROFILE": "debug"}, capture_output=True,
                               text=True, errors="replace")
        self.assertEqual(debug.returncode, 0, debug.stdout + debug.stderr)
        self.assertIn("cargo:warning=Refreshed", debug.stdout)
        self.assertIn(self.upstream, output.read_bytes())
        self.assertNotEqual(snapshot.read_bytes(), before)

        manifest.write_bytes(manifest.read_bytes() + b'\n[workspace.dependencies]\nnew = "1"\n')
        before = snapshot.read_bytes()
        output.unlink()
        for profile in ("release", "custom-release"):
            with self.subTest(profile=profile):
                release = subprocess.run([str(binary)], env=env | {"PROFILE": profile}, capture_output=True,
                                         text=True, errors="replace")
                self.assertNotEqual(release.returncode, 0)
                self.assertIn("Manifest semantics changed", release.stderr)
                self.assertFalse(output.exists())
        debug = subprocess.run([str(binary)], env=env | {"PROFILE": "debug"}, capture_output=True,
                               text=True, errors="replace")
        self.assertEqual(debug.returncode, 0, debug.stdout + debug.stderr)
        self.assertIn("cargo:warning=DEVELOPMENT LICENSE REVIEW PENDING", debug.stdout)
        self.assertIn(b"DEVELOPMENT LICENSE REVIEW PENDING", output.read_bytes())
        self.assertNotIn(self.upstream, output.read_bytes())
        self.assertEqual(snapshot.read_bytes(), before)

    def test_cargo_records_cover_hashed_and_repository_origin_input_changes(self):
        hashed_name = "reviewed inputs/additional input.txt"
        hashed_bytes = b"Additional reviewed input\n"
        self.put(hashed_name, hashed_bytes)
        self.document["input_sha256"][hashed_name] = notices.digest(hashed_bytes)
        origin_name = "native kernels/source with spaces.c"
        origin_bytes = b"Copyright Example\nPermission granted\nimplementation body\n"
        self.put(origin_name, origin_bytes)
        key = self.source(b"Copyright Example\nPermission granted\n", {
            "kind": "repository", "path": origin_name,
            "file_sha256": notices.digest(origin_bytes), "lines": [1, 2],
        })
        self.document["materials"][0]["notices"].append(key)
        self.save()
        output = self.root / "explicit notices.txt"
        self.assertEqual(self.invoke("--output", str(output), "--cargo-rerun"), 0)
        prefix = "cargo:rerun-if-changed="
        records = [Path(line.removeprefix(prefix)) for line in self.stdout.splitlines()
                   if line.startswith(prefix)]
        expected = {self.root / "scripts/generate_notices.py", self.root / notices.INPUT,
                    *(self.root / name for name in self.document["input_sha256"]), self.root / origin_name}
        self.assertEqual(set(records), expected)
        self.assertEqual(len(records), len(expected))
        before = output.read_bytes()
        for name, original in ((hashed_name, hashed_bytes), (origin_name, origin_bytes)):
            with self.subTest(input=name):
                self.assertIn(self.root / name, records)
                self.put(name, original + b"unreviewed change\n")
                self.assertEqual(self.invoke("--output", str(output), "--cargo-rerun"), 1)
                self.assertIn(f"Stale input hash: {name}", self.stderr)
                self.assertEqual(output.read_bytes(), before)
                self.put(name, original)

    def test_changed_manifest_requires_review(self):
        self.put("Cargo.toml", b"changed dependency\n")
        with self.assertRaisesRegex(notices.NoticeError, "Stale input hash: Cargo.toml"):
            self.load()

    def test_changed_lock_requires_review(self):
        self.put("Cargo.lock", b"changed lock\n")
        with self.assertRaisesRegex(notices.NoticeError, "Stale input hash: Cargo.lock"):
            self.load()

    def test_missing_graph_package_is_rejected(self):
        self.document["packages"].clear()
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Package coverage differs"):
            self.load()

    def test_missing_package_notice_is_rejected(self):
        self.document["packages"][0]["notices"] = []
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Unhandled package"):
            self.load()

    def test_changed_archive_checksum_is_rejected(self):
        self.document["packages"][0]["checksum"] = "wrong"
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Locked archive checksum differs"):
            self.load()

    def test_corrupted_source_bytes_are_rejected(self):
        self.document["sources"][self.upstream_key]["base64"] = base64.b64encode(b"altered").decode("ascii")
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Notice source hash or length mismatch"):
            self.load()

    def test_missing_source_is_rejected(self):
        del self.document["sources"][self.upstream_key]
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Missing package notice source"):
            self.load()

    def test_vendor_licenses_are_mandatory(self):
        del self.document["input_sha256"][notices.VENDOR_LICENSES[0]]
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Missing mandatory"):
            self.load()

    def test_vendor_notice_cannot_be_replaced_with_unrelated_text(self):
        key = self.document["materials"][0]["notices"][0]
        self.document["sources"][key]["origins"][0]["path"] = "LICENSE-MIT"
        self.document["sources"][key]["origins"][0]["file_sha256"] = self.inputs["LICENSE-MIT"]
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Mandatory native kernel license omitted"):
            self.load()

    def test_non_utf8_original_is_preserved_and_explicitly_transcoded(self):
        original = b"Patent terms call this \x93License\x94.\r\n"
        key = self.source(original, {"kind": "published_crate", "url": "https://example.invalid/PATENTS"},
                          "windows-1252")
        self.document["packages"][0]["notices"].append(key)
        self.save()
        manifest, sources = self.load()
        output = notices.render(self.root, manifest, sources)
        self.assertEqual(sources[key], original)
        self.assertIn("Patent terms call this \u201cLicense\u201d.\r\n".encode("utf-8"), output)
        self.assertIn(b"Original encoding: windows-1252. Transcoded to UTF-8", output)
        self.assertIn(key.encode("ascii"), output)

    def test_repository_grant_excerpt_is_checked_against_actual_source(self):
        name = "native/header.c"
        original = b"implementation header\nCopyright Example\nPermission granted\nimplementation body\n"
        self.put(name, original)
        self.document["input_sha256"][name] = notices.digest(original)
        key = self.source(b"Copyright Example\nPermission granted\n", {
            "kind": "repository", "path": name, "file_sha256": notices.digest(original), "lines": [2, 3],
        })
        self.document["materials"][0]["notices"].append(key)
        self.save()
        manifest, sources = self.load()
        output = notices.render(self.root, manifest, sources)
        self.assertIn(b"Copyright Example\nPermission granted\n", output)
        self.assertNotIn(b"implementation body", output)
        self.document["sources"][key]["origins"][0]["lines"] = [1, 2]
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Repository notice source differs"):
            self.load()

    def test_paths_cannot_depend_on_local_absolute_directories(self):
        self.document["input_sha256"]["C:/private/cache/license"] = "unknown"
        self.save()
        with self.assertRaisesRegex(notices.NoticeError, "Nonportable input path"):
            self.load()


class MaintainedInputCoverageTests(unittest.TestCase):
    def test_reviewed_manifest_fingerprints_match_current_inputs(self):
        root = SCRIPT.resolve().parents[1]
        _, _, original, candidate = notices.load_inputs(root, refresh_inputs=True)
        self.assertEqual(candidate, original)

    def test_all_reviewed_source_texts_survive_rendering(self):
        root = SCRIPT.resolve().parents[1]
        manifest, sources, _, _ = notices.load_inputs(root)
        output = notices.render(root, manifest, sources)
        output.decode("utf-8")
        for key, original in sources.items():
            with self.subTest(source=key):
                rendered = original.decode(manifest["sources"][key]["encoding"]).encode("utf-8")
                self.assertIn(rendered, output)
                if manifest["sources"][key]["encoding"] == "utf-8":
                    self.assertEqual(rendered, original)
        paths = {origin.get("path", "") for source in manifest["sources"].values()
                 for origin in source["origins"]}
        self.assertIn("LICENSE-BoringSSL", paths)
        self.assertIn("LICENSE-LUCIDE", paths)
        material_names = {item["name"] for item in manifest["materials"]}
        self.assertIn("Microsoft rendering code MIT notice", material_names)
        self.assertIn("shadcn theme palette MIT notice", material_names)
        mpl = {item["name"]: item for item in manifest["mpl_source"]}
        for name in ("serialport", "option-ext"):
            package = next(item for item in manifest["packages"] if item["name"] == name)
            self.assertEqual(mpl[name]["sha256"], package["checksum"])
            self.assertEqual(mpl[name]["url"], package["source_archive"])


if __name__ == "__main__":
    unittest.main()
