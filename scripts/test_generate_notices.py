"""Behavioral tests for frozen notice inputs and byte-preserving output."""
from __future__ import annotations

import base64
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("generate_notices.py")
SPEC = importlib.util.spec_from_file_location("generate_notices", SCRIPT)
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class NoticeGeneratorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.root.joinpath("scripts").mkdir()
        self.inputs = {}
        for name, content in {
            "Cargo.toml": b'[workspace.package]\nlicense = "MIT OR Apache-2.0"\n',
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
        return notices.load_inputs(self.root)

    def invoke(self, *arguments):
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return notices.main(["--root", str(self.root), *arguments])

    def test_regeneration_and_check_are_deterministic(self):
        self.assertEqual(self.invoke(), 0)
        before = (self.root / notices.OUTPUT).read_bytes()
        self.assertEqual(self.invoke(), 0)
        self.assertEqual((self.root / notices.OUTPUT).read_bytes(), before)
        self.assertEqual(self.invoke("--check"), 0)
        self.assertIn(self.upstream, before)
        self.assertEqual(before.count(self.upstream), 1)
        self.assertIn(b"Application MIT terms\n", before)
        self.assertIn(b"Application Apache terms\n", before)

    def test_check_rejects_stale_output_without_rewriting_it(self):
        self.put(notices.OUTPUT, b"old document\n")
        self.assertEqual(self.invoke("--check"), 1)
        self.assertEqual((self.root / notices.OUTPUT).read_bytes(), b"old document\n")

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
    def test_all_reviewed_source_texts_survive_rendering(self):
        root = SCRIPT.resolve().parents[1]
        manifest, sources = notices.load_inputs(root)
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
