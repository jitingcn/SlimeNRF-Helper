"""Behavioral tests for release identity, changelog extraction and output safety."""
from __future__ import annotations

import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("prepare-prerelease.py")
SPEC = importlib.util.spec_from_file_location("prepare_prerelease", SCRIPT)
preparation = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = preparation
SPEC.loader.exec_module(preparation)


class PrereleasePreparationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.temporary = Path(temporary.name).resolve()
        self.root = self.temporary / "source"
        self.root.mkdir()
        self.output = self.temporary / "notes.md"
        self.commit = "1234567890abcdef1234567890abcdef12345678"
        self.body = "### Added\n\n- A user-facing feature.\n\n### Fixed\n\n- A corrected behavior."
        self.manifest = (
            '[workspace]\nmembers = ["crates/member"]\n'
            '[workspace.package]\nversion = "1.2.3"\n'
            '[package]\nname = "slimenrf-helper"\nversion.workspace = true\n'
        )
        self.put("Cargo.toml", self.manifest)
        self.put("crates/member/Cargo.toml", '[package]\nname = "member"\nversion = "9.9.9"\n')
        self.changelog = (
            "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- Not yet released.\n\n"
            f"## [1.2.3] - 2026-10-26\n\n{self.body}\n\n"
            "## [1.2.2] - 2026-10-20\n\n### Fixed\n\n- An older change.\n\n"
            "[1.2.3]: https://example.invalid/releases/1.2.3\n"
        )
        self.put("CHANGELOG.md", self.changelog)
        self.put("CHANGELOG.zh-CN.md", "# 更新日志\n\n## [1.2.3] - 2026-10-26\n\n- 用户可见的变更。\n")

    def put(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def invoke(self, **changes):
        arguments = {
            "root": str(self.root),
            "tag": "v1.2.3",
            "repository": "example/project",
            "commit": self.commit,
            "notes-output": str(self.output),
        }
        arguments.update(changes)
        argv = [item for key, value in arguments.items() if value is not None
                for item in (f"--{key}", value)]
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = preparation.main(argv)
        return result, stdout.getvalue(), stderr.getvalue()

    def assert_refused(self, **changes):
        result, stdout, stderr = self.invoke(**changes)
        self.assertEqual(result, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertIn("Prerelease preparation failed:", stderr)
        self.assertFalse(self.output.exists())

    def test_workspace_version_and_exact_section_produce_commit_pinned_notes(self):
        originals = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        result, stdout, stderr = self.invoke()
        self.assertEqual((result, stdout, stderr), (0, "1.2.3\n", ""))
        expected = (f"{self.body}\n\n[简体中文](https://github.com/example/project/blob/"
                    f"{self.commit}/CHANGELOG.zh-CN.md)\n")
        self.assertEqual(self.output.read_bytes(), expected.encode("utf-8"))
        self.assertEqual({path: path.read_bytes() for path in originals}, originals)

    def test_root_literal_version_takes_precedence_over_workspace_and_members(self):
        self.put("Cargo.toml", self.manifest.replace('version.workspace = true', 'version = "2.0.0-rc.1+build.7"'))
        self.put("CHANGELOG.md", self.changelog.replace("1.2.3", "2.0.0-rc.1+build.7"))
        result, stdout, stderr = self.invoke(tag="v2.0.0-rc.1+build.7")
        self.assertEqual((result, stdout, stderr), (0, "2.0.0-rc.1+build.7\n", ""))
        self.assertIn(self.body, self.output.read_text(encoding="utf-8"))

    def test_missing_invalid_or_nonapplication_versions_fail_without_output(self):
        manifests = [
            self.manifest.replace('[workspace.package]\nversion = "1.2.3"\n', ""),
            self.manifest.replace('version.workspace = true', 'version.workspace = false'),
            self.manifest.replace('version.workspace = true', 'version.workspace = 1'),
            self.manifest.replace('version.workspace = true', 'version = 123'),
            self.manifest.replace('version.workspace = true', ''),
            self.manifest.replace('name = "slimenrf-helper"', 'name = "some-other-package"'),
            '[workspace]\nmembers = ["crates/member"]\n',
            '[workspace.package]\nversion = "1.2.3"\npackage = 7\n',
            'this is not TOML',
        ]
        for manifest in manifests:
            with self.subTest(manifest=manifest):
                self.put("Cargo.toml", manifest)
                self.assert_refused()

    def test_unsafe_or_nonsemantic_versions_cannot_enter_workflow_output(self):
        for version in ("01.2.3", "1.2", "1.2.3-01", "1.2.3/other", "1.2.3\nversion=evil", "1.2.3+", "1.2.3 "):
            with self.subTest(version=version):
                escaped = version.replace("\n", r"\n")
                self.put("Cargo.toml", self.manifest.replace('version.workspace = true', f'version = "{escaped}"'))
                self.assert_refused(tag=f"v{version}")

    def test_tag_must_be_exactly_the_application_version(self):
        for tag in ("1.2.3", "v1.2", "v1.2.30", "v1.2.3-rc.1", "v1.2.3\n", "refs/tags/v1.2.3", "v1.2.3/other"):
            with self.subTest(tag=tag):
                self.assert_refused(tag=tag)

    def test_final_section_stops_before_reference_definitions(self):
        self.put("CHANGELOG.md", f"# Changelog\n\n## [1.2.3] - 2024-02-29\n\n{self.body}\n\n"
                 "[Unreleased]: https://example.invalid/compare\n[1.2.3]: https://example.invalid/release\n")
        result, _, stderr = self.invoke()
        self.assertEqual(result, 0, stderr)
        notes = self.output.read_text(encoding="utf-8")
        self.assertTrue(notes.startswith(self.body + "\n\n"))
        self.assertNotIn("example.invalid", notes)
        self.assertNotIn("## [1.2.3]", notes)

    def test_missing_duplicate_undated_invalid_or_empty_sections_are_rejected(self):
        sections = [
            self.changelog.replace("[1.2.3]", "[1.2.4]"),
            self.changelog + f"\n## [1.2.3] - 2026-10-26\n\n{self.body}\n",
            self.changelog + f"\n## [1.2.3]\n\n{self.body}\n",
            self.changelog.replace("## [1.2.3] - 2026-10-26", "## [1.2.3]"),
            self.changelog.replace("2026-10-26", "2026-02-29"),
            self.changelog.replace("2026-10-26", "2026-2-9"),
            self.changelog.replace("2026-10-26", "2026-10-26 extra"),
            self.changelog.replace(self.body, ""),
            self.changelog.replace(self.body, "### Added\n\n### Fixed"),
        ]
        for changelog in sections:
            with self.subTest(changelog=changelog):
                self.put("CHANGELOG.md", changelog)
                self.assert_refused()

    def test_unsafe_repository_and_nonpinned_commit_are_rejected(self):
        for repository in ("https://github.com/example/project", "example/project/extra", "example/..",
                           "example/project?ref=main", "example/project\n", "example/project#fragment"):
            with self.subTest(repository=repository):
                self.assert_refused(repository=repository)
        for commit in ("main", self.commit[:12], "g" * 40, self.commit.upper(), self.commit + "/other", self.commit + "\n"):
            with self.subTest(commit=commit):
                self.assert_refused(commit=commit)

    def test_missing_chinese_changelog_is_not_published_as_a_broken_link(self):
        (self.root / "CHANGELOG.zh-CN.md").unlink()
        self.assert_refused()

    def test_explicit_absolute_external_output_is_required(self):
        with self.assertRaises(SystemExit) as raised:
            self.invoke(**{"notes-output": None})
        self.assertEqual(raised.exception.code, 2)
        for destination in ("notes.md", self.root / "generated.md", self.root / ".." / "source" / "generated.md",
                            self.root / "Cargo.toml", self.root / "CHANGELOG.md"):
            with self.subTest(destination=str(destination)):
                self.assert_refused(**{"notes-output": str(destination)})
        self.assertEqual((self.root / "Cargo.toml").read_text(encoding="utf-8"), self.manifest)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), self.changelog)
        self.assertFalse((self.root / "generated.md").exists())

    def test_existing_output_is_never_overwritten(self):
        self.output.write_bytes(b"previous notes\r\n")
        result, stdout, stderr = self.invoke()
        self.assertEqual(result, 1, stderr)
        self.assertEqual(stdout, "")
        self.assertEqual(self.output.read_bytes(), b"previous notes\r\n")


if __name__ == "__main__":
    unittest.main()
