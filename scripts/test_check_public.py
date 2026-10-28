"""Behavioral public-checker tests using disposable, initially uncommitted Git repos."""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


CHECKER = Path(__file__).with_name("check-public.py")
DOCUMENTS = (
    "README", "CHANGELOG", "docs/USER_GUIDE", "docs/DEVELOPMENT",
    "docs/ARCHITECTURE", "docs/PROTOCOLS",
)


@unittest.skipUnless(shutil.which("git"), "Git is required for isolated repository tests")
class PublicCheckerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve() / "application"
        self.root.mkdir()
        self.put("Cargo.toml", '[workspace]\nmembers = ["crates/app", "crates/recording", '
                               '"crates/firmware-image", "crates/terminal-core"]\n')
        self.put("crates/app/Cargo.toml", '[package]\nname = "slimenrf-helper"\n')
        self.put("crates/recording/Cargo.toml", '[package]\nname = "slimenrf-recording"\n')
        self.put("crates/firmware-image/Cargo.toml", '[package]\nname = "slimenrf-firmware-image"\n')
        self.put("crates/terminal-core/Cargo.toml", '[package]\nname = "slimenrf-terminal-core"\n')
        self.put("crates/app/src/main.rs", "fn main() {}\n")
        (self.root / "scripts").mkdir()
        shutil.copyfile(CHECKER, self.root / "scripts/check-public.py")
        for base in DOCUMENTS:
            name = base.rsplit("/", 1)[-1]
            self.put(base + ".md", f"# Review fixture\n\n[中文]({name}.zh-CN.md)\n")
            self.put(base + ".zh-CN.md", f"# 审查样例\n\n[English]({name}.md)\n")
        for suffix, added in ((".md", "Added"), (".zh-CN.md", "新增")):
            path = self.root / ("CHANGELOG" + suffix)
            with path.open("a", encoding="utf-8") as stream:
                stream.write(
                    "\n## [Unreleased]\n\n## [0.1.1] - 2026-10-27\n\n"
                    f"### {added}\n\n- Release publication fixture.\n\n"
                    "[Unreleased]: https://github.com/example/project/compare/v0.1.1...HEAD\n"
                    "[0.1.1]: https://github.com/example/project/releases/tag/v0.1.1\n"
                )
        self.git("-c", "init.templateDir=", "init", "--quiet")
        name = "public_checker_fixture_" + str(id(self))
        spec = importlib.util.spec_from_file_location(name, self.root / "scripts/check-public.py")
        self.checker = importlib.util.module_from_spec(spec)
        sys.modules[name] = self.checker
        self.addCleanup(sys.modules.pop, name, None)
        previous_bytecode = sys.dont_write_bytecode
        try:
            sys.dont_write_bytecode = True
            spec.loader.exec_module(self.checker)
        finally:
            sys.dont_write_bytecode = previous_bytecode

    def put(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            path.write_bytes(content)
        else:
            path.write_text(content, encoding="utf-8")
        return path

    def git(self, *arguments, root=None):
        environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        environment.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                            "GIT_TERMINAL_PROMPT": "0"})
        result = subprocess.run(["git", "-C", str(root or self.root), *arguments],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
        if result.returncode:
            self.fail("Isolated Git fixture command failed")
        return result.stdout

    def run_checker(self, *arguments):
        result = subprocess.run([sys.executable, "-B", str(self.root / "scripts/check-public.py"), *arguments],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8")
        self.assertEqual(result.stderr, "")
        return result.returncode, [json.loads(line) for line in result.stdout.splitlines()]

    def findings(self, report):
        return {(item.path, item.line, item.category) for item in report.findings}

    def test_initial_repository_scans_nonignored_candidates_before_first_commit(self):
        status, output = self.run_checker("--root", str(self.root))
        self.assertEqual(status, 0)
        self.assertEqual(len(output), 1)
        inventory = output[0]["inventory"]
        self.assertEqual(inventory["tracked"], 0)
        self.assertGreater(inventory["nonignored_untracked"], 0)
        self.assertEqual(inventory["ignored_tracked"], 0)
        self.assertEqual(inventory["candidates"], inventory["nonignored_untracked"])
        self.assertEqual(inventory["scanned_text"], inventory["candidates"])
        self.assertEqual(inventory["findings"], 0)

    def test_inventory_keeps_tracked_untracked_and_ignored_tracked_separate(self):
        self.put("tracked.txt", "public\n")
        self.put("now-ignored.txt", "still reviewed\n")
        self.git("add", "tracked.txt", "now-ignored.txt")
        self.put("untracked.txt", "public\n")
        self.put(".gitignore", "now-ignored.txt\nignored-only.txt\n")
        self.put("ignored-only.txt", "not a publication candidate\n")
        report = self.checker.audit()
        self.assertEqual(report.inventory.tracked, ("now-ignored.txt", "tracked.txt"))
        self.assertEqual(report.inventory.ignored_tracked, ("now-ignored.txt",))
        self.assertIn("untracked.txt", report.inventory.untracked)
        self.assertNotIn("ignored-only.txt", report.inventory.candidates)
        self.assertEqual(self.findings(report), set())
        self.assertEqual(report.counts["scanned_text"], len(report.inventory.candidates))

    def test_new_crate_sources_are_inventoried_and_scanned(self):
        tracked = "crates/terminal-core/src/lib.rs"
        untracked = "crates/firmware-image/src/parser.rs"
        for path in (tracked, untracked):
            self.put(path, "// " + "token" + ' = "public-review-fixture"\n')
        self.git("add", "--", tracked)

        report = self.checker.audit()
        self.assertIn(tracked, report.inventory.tracked)
        self.assertIn(untracked, report.inventory.untracked)
        self.assertEqual(self.findings(report), {
            (tracked, 1, "credential-assignment"),
            (untracked, 1, "credential-assignment"),
        })

    def test_forbidden_tracked_paths_are_flagged_without_content_reads(self):
        paths = (".local/private.txt", "dist/export.txt", "captures/session.csv",
                 "recordings/session.csv", "firmwares/image.hex", ".env", "signing.pem",
                 "preferences.json", "device-registry.json", "custom-shortcuts.json",
                 ".cargo/credentials.toml", ".git-credentials", ".vscode/state.json")
        for path in paths:
            self.put(path, "unopened private fixture\n")
        self.git("add", "--force", "--", *paths)
        original = self.checker.os.open

        def guarded_open(path, *arguments, **keywords):
            relative = Path(path).relative_to(self.root).as_posix()
            if relative in paths:
                raise AssertionError("Forbidden content was opened")
            return original(path, *arguments, **keywords)

        with mock.patch.object(self.checker.os, "open", side_effect=guarded_open):
            report = self.checker.audit()
        self.assertEqual(self.findings(report), {
            (path, 0, "forbidden-publication-path") for path in paths
        })

    def test_secret_forms_and_private_roots_report_only_locations(self):
        lines = [
            "C:" + r"\Users\sample-person\work\project",
            "/home/" + "sample-person/project",
            "/Users/" + "sample-person/project",
            "/mnt/" + "c/workstation/project",
            "\\\\" + r"private-host\share\project",
            "token" + ' = "sample-assignment-value"',
            "Bearer " + "sample-authorization-value",
            "https://" + "reader:sample-password@example.invalid/a",
            "https://example.invalid/a?sig" + "=sample-signature",
            "ghp_" + "a" * 24,
            "-----BEGIN " + "OPENSSH PRIVATE KEY-----",
            "Authorization: " + "Basic c2FtcGxlOnNhbXBsZQ==",
            "GH_TOKEN" + "=sample-environment-value",
        ]
        self.put("public.txt", "\n".join(lines) + "\n")
        status, output = self.run_checker()
        self.assertEqual(status, 1)
        self.assertEqual(output[:-1], [
            {"path": "public.txt", "line": index, "category": category}
            for index, category in enumerate([
                *(["private-absolute-path"] * 5), "credential-assignment", "authorization-value",
                "authenticated-url", "signed-credential", "credential-token", "private-key",
                "authorization-value", "credential-assignment",
            ], 1)
        ])
        rendered = json.dumps(output)
        for value in ("sample-person", "sample-password", "sample-signature", "private-host"):
            self.assertNotIn(value, rendered)

    def test_reviewed_synthetic_credentials_do_not_exempt_new_values_or_files(self):
        fixture = "https://" + "user:password@api.github.com/credentials"
        bearer = "bearer " + "fixture-secret"
        path = "crates/app/src/firmware_source/http_tests.rs"
        self.assertEqual(self.checker.scan_text(path, fixture + "\n" + bearer), set())
        changed = fixture.replace("password", "different-password")
        self.assertEqual(self.findings_from_text(path, changed), {(path, 1, "authenticated-url")})
        self.assertEqual(self.findings_from_text("src/firmware_source/http_tests.rs", bearer),
                         {("src/firmware_source/http_tests.rs", 1, "authorization-value")})
        self.assertEqual(self.findings_from_text("other.rs", bearer), {("other.rs", 1, "authorization-value")})
        public = "https://github.com/upstream/repository\n" + "a" * 64 + "\nconst USB_VENDOR: u16 = 0x1209;\n"
        self.assertEqual(self.checker.scan_text("public.rs", public), set())

    def test_app_update_credential_fixtures_are_exact_and_file_scoped(self):
        path = "crates/app/src/app_update/http_tests.rs"
        fixtures = (
            ("authenticated-url", "https://" + "user:fixture-secret@api.github.com/private-fixture-path"),
            ("signed-credential", "?sig" + "=FiXture%2Fpart"),
            ("signed-credential", "?sig" + "=fixture-secret"),
            ("signed-credential", "?sig" + "=fixture-secret-signature"),
        )
        for category, fixture in fixtures:
            with self.subTest(category=category, fixture=fixture):
                self.assertEqual(self.findings_from_text(path, fixture), set())
                changed = fixture.replace("fixture", "different").replace("FiXture", "different")
                self.assertEqual(self.findings_from_text(path, changed), {(path, 1, category)})
                self.assertEqual(self.findings_from_text("other.rs", fixture),
                                 {("other.rs", 1, category)})
        extra = "?sig" + "=fixture-secret&" + "token" + "=unreviewed-value"
        self.assertEqual(self.findings_from_text(path, extra), {(path, 1, "signed-credential")})

    def test_app_update_windows_fixture_paths_require_exact_complete_lines(self):
        path = "crates/app/src/app_update/windows/tests.rs"
        target = ('target: wide(OsString::from(r"' + "C:" + r'\fixture\application.exe'
                  + '").as_os_str()),')
        working_directory = ('working_directory: wide(OsString::from(r"' + "C:"
                             + r'\fixture' + '").as_os_str()),')
        for line in (target, working_directory):
            with self.subTest(line=line):
                self.assertEqual(self.findings_from_text(path, "    " + line), set())
                self.assertEqual(self.findings_from_text("other.rs", line),
                                 {("other.rs", 1, "private-absolute-path")})
                changed = line.replace("fixture", "private")
                self.assertEqual(self.findings_from_text(path, changed),
                                 {(path, 1, "private-absolute-path")})
                suffix = line + " // " + "C:" + r'\private\customer'
                self.assertEqual(self.findings_from_text(path, suffix),
                                 {(path, 1, "private-absolute-path")})

    def test_reviewed_public_tool_path_requires_the_exact_complete_line(self):
        tool = "C:" + chr(92) + chr(92).join(("Program Files", "LLVM", "bin", "libclang.dll"))
        line = "if (-not (Test-Path -LiteralPath '" + tool + "' -PathType Leaf)) {"
        path = ".github/workflows/ci.yml"
        self.assertEqual(self.checker.scan_text(path, line), set())
        changed = line.replace("libclang.dll", "private-signing.key")
        self.assertEqual(self.findings_from_text(path, changed), {(path, 1, "private-absolute-path")})

    def test_reviewed_private_path_does_not_exempt_a_whitespace_suffix(self):
        path = "crates/app/src/bundled_resources/tests.rs"
        self.put(path, '        "C:' + '/firmwares/a.bin",\n')
        status, output = self.run_checker()
        self.assertEqual(status, 0)
        self.assertEqual(output[:-1], [])
        self.put(path, '        "C:' + '/firmwares/a.bin private/customer/file",\n')
        status, output = self.run_checker()
        self.assertEqual(status, 1)
        self.assertEqual(output[:-1], [
            {"path": path, "line": 1, "category": "private-absolute-path"},
        ])
        self.assertNotIn("firmwares/a.bin", json.dumps(output))
        self.assertNotIn("private/customer/file", json.dumps(output))

    def test_only_relocated_public_receiver_log_is_allowed(self):
        self.assertFalse(self.checker.forbidden("crates/app/src/rssi_scan/fixtures/receiver_scan.log"))
        self.assertTrue(self.checker.forbidden("src/rssi_scan/fixtures/receiver_scan.log"))
        self.assertTrue(self.checker.forbidden("crates/app/src/rssi_scan/fixtures/private.log"))

    def test_reviewed_translation_labels_do_not_exempt_credentials(self):
        prefix = "token" + ": "
        for path, label in (("locales/en.yml", "Token"), ("locales/zh-CN.yml", "令牌")):
            approved = prefix + json.dumps(label, ensure_ascii=False)
            self.assertEqual(self.findings_from_text(path, approved), set())
            changed = prefix + json.dumps("unreviewed-private-value")
            self.assertEqual(self.findings_from_text(path, changed), {
                (path, 1, "credential-assignment"),
            })
            self.assertEqual(self.findings_from_text("notes.yml", approved), {
                ("notes.yml", 1, "credential-assignment"),
            })

    def findings_from_text(self, path, text):
        return {(item.path, item.line, item.category) for item in self.checker.scan_text(path, text)}

    def test_binary_undecodable_and_missing_candidates_fail(self):
        self.put("image.png", b"\x89PNG\x00fixture")
        self.put("printable.png", b"printable bytes do not clear a binary asset")
        self.put("unknown.txt", b"\xfffixture")
        missing = self.put("missing.txt", "previously indexed\n")
        self.git("add", "missing.txt")
        missing.unlink()
        status, output = self.run_checker()
        self.assertEqual(status, 1)
        self.assertEqual(output[:-1], [
            {"path": "image.png", "line": 0, "category": "unreviewed-binary"},
            {"path": "missing.txt", "line": 0, "category": "missing-path"},
            {"path": "printable.png", "line": 0, "category": "unreviewed-binary"},
            {"path": "unknown.txt", "line": 0, "category": "unreviewed-undecodable"},
        ])

    def test_markdown_heading_explicit_reference_and_encoded_targets(self):
        self.put("notes.md", '# Notes\n\n[space](docs/space%20name.md#keep-this)\n'
                 '[heading](#use-tool)\n[repeat](#use-tool-1)\n[ref][target]\n'
                 '[target]: docs/space%20name.md#keep-this\n\n'
                 '## Use `tool`\n## Use `tool`\n'
                 '`[not a link](missing.md)`\n```md\n[not a link](missing.md)\n```\n')
        self.put("docs/space name.md", '# Target\n<a id="keep-this"></a>\n')
        self.assertEqual(self.findings(self.checker.audit()), set())
        self.put("notes.md", '[missing](docs/space%20name.md#absent)\n[broken](missing.md)\n')
        self.assertEqual(self.findings(self.checker.audit()), {
            ("notes.md", 1, "markdown-missing-anchor"), ("notes.md", 2, "missing-path"),
        })

    def test_markdown_forbidden_and_escaping_targets_never_open_contents(self):
        self.put(".gitignore", ".local/\nhidden.md\n")
        self.put(".local/private.md", "must not be opened\n")
        self.put("hidden.md", "not a publication input\n")
        self.put("notes.md", '[forbidden](.local/private.md#secret)\n'
                 '[escape](%2e%2e/private.md)\n[file](file:///private/file.md)\n'
                 '[reference][missing]\n[ignored](hidden.md#private)\n')
        original = self.checker.os.open

        def guarded_open(path, *arguments, **keywords):
            if ".local" in Path(path).parts or Path(path).name == "hidden.md":
                raise AssertionError("Forbidden Markdown target was opened")
            return original(path, *arguments, **keywords)

        with mock.patch.object(self.checker.os, "open", side_effect=guarded_open):
            report = self.checker.audit()
        self.assertEqual(self.findings(report), {
            ("notes.md", 1, "forbidden-publication-path"),
            ("notes.md", 2, "markdown-target-outside-root"),
            ("notes.md", 3, "markdown-unsafe-target"),
            ("notes.md", 4, "markdown-unsafe-target"),
            ("notes.md", 5, "markdown-target-not-reviewed-text"),
        })

    def test_markdown_directories_require_publication_candidates_without_ignored_reads(self):
        self.put(".gitignore", "hidden-docs/\npublic-docs/ignored/\n")
        self.put("hidden-docs/README.md", "ignored directory content must not be opened\n")
        self.put("public-docs/ignored/README.md", "ignored nested content must not be opened\n")
        self.put("public-docs/guide.md", "# Public guide\n")
        self.put("notes.md", "[ignored](hidden-docs/)\n[public](public-docs/)\n[root](.)\n")
        original = self.checker.os.open

        def guarded_open(path, *arguments, **keywords):
            relative = Path(path).relative_to(self.root).as_posix()
            if relative.startswith(("hidden-docs/", "public-docs/ignored/")):
                raise AssertionError("Ignored directory content was opened")
            return original(path, *arguments, **keywords)

        with mock.patch.object(self.checker.os, "open", side_effect=guarded_open):
            report = self.checker.audit()
        self.assertEqual(self.findings(report), {
            ("notes.md", 1, "markdown-target-not-published"),
        })
        self.assertIn("public-docs/guide.md", report.inventory.candidates)
        self.assertNotIn("hidden-docs/README.md", report.inventory.candidates)
        self.assertNotIn("public-docs/ignored/README.md", report.inventory.candidates)

    def test_language_pairing_navigation_and_explicit_anchor_parity(self):
        self.put("docs/USER_GUIDE.zh-CN.md", '# 指南\n\n[English](USER_GUIDE.md)\n'
                 '[wrong](../README.md)\n<a id="only-chinese"></a>\n')
        (self.root / "README.zh-CN.md").unlink()
        findings = self.findings(self.checker.audit())
        self.assertEqual(findings, {
            ("README.md", 1, "missing-reciprocal-language-link"),
            ("README.md", 3, "missing-path"),
            ("README.zh-CN.md", 0, "missing-language-document"),
            ("docs/USER_GUIDE.zh-CN.md", 4, "markdown-language-navigation"),
            ("docs/USER_GUIDE.md", 0, "language-explicit-anchor-mismatch"),
            ("docs/USER_GUIDE.zh-CN.md", 0, "language-explicit-anchor-mismatch"),
        })

    def test_changelog_requires_its_translated_document_and_reciprocal_link(self):
        (self.root / "CHANGELOG.zh-CN.md").unlink()
        status, output = self.run_checker()
        self.assertEqual(status, 1)
        self.assertEqual(output[:-1], [
            {"path": "CHANGELOG.md", "line": 1, "category": "missing-reciprocal-language-link"},
            {"path": "CHANGELOG.md", "line": 3, "category": "missing-path"},
            {"path": "CHANGELOG.zh-CN.md", "line": 0, "category": "missing-language-document"},
        ])

    def test_changelog_navigation_preserves_the_selected_language(self):
        self.put("CHANGELOG.md", "# Changelog\n\n[中文](CHANGELOG.zh-CN.md)\n"
                 "[guide](docs/USER_GUIDE.md)\n")
        self.put("CHANGELOG.zh-CN.md", "# 更新日志\n\n[English](CHANGELOG.md)\n"
                 "[指南](docs/USER_GUIDE.zh-CN.md)\n")
        self.assertEqual(self.findings(self.checker.audit()), set())
        self.put("CHANGELOG.zh-CN.md", "# 更新日志\n\n[English](CHANGELOG.md)\n"
                 "[wrong](docs/USER_GUIDE.md)\n")
        self.assertEqual(self.findings(self.checker.audit()), {
            ("CHANGELOG.zh-CN.md", 4, "markdown-language-navigation"),
        })

    def test_changelog_contents_still_reject_private_paths(self):
        path = self.root / "CHANGELOG.md"
        with path.open("a", encoding="utf-8") as stream:
            stream.write("\n- Evidence: /home/" + "sample-person/private/release.txt\n")
        report = self.checker.audit()
        self.assertEqual({(item.path, item.category) for item in report.findings}, {
            ("CHANGELOG.md", "private-absolute-path"),
        })

    def test_ignored_planning_pair_is_not_required_or_opened(self):
        self.put(".gitignore", "PLAN.md\nPLAN.zh-CN.md\n")
        self.assertEqual(self.findings(self.checker.audit()), set())
        for path in ("PLAN.md", "PLAN.zh-CN.md"):
            self.put(path, "ignored local planning material\n")
        original = self.checker.os.open

        def guarded_open(path, *arguments, **keywords):
            if Path(path).name in {"PLAN.md", "PLAN.zh-CN.md"}:
                raise AssertionError("Ignored planning content was opened")
            return original(path, *arguments, **keywords)

        with mock.patch.object(self.checker.os, "open", side_effect=guarded_open):
            report = self.checker.audit()
        self.assertEqual(self.findings(report), set())
        self.assertNotIn("PLAN.md", report.inventory.candidates)
        self.assertNotIn("PLAN.zh-CN.md", report.inventory.candidates)

    def test_root_override_cannot_select_parent_or_another_repository(self):
        status, output = self.run_checker("--root", str(self.root.parent))
        self.assertEqual((status, output), (2, [{
            "path": ".", "line": 0, "category": "root-does-not-match-application",
        }]))
        self.put("Cargo.toml", '[package]\nname = "different-application"\n')
        status, output = self.run_checker()
        self.assertEqual((status, output), (2, [{
            "path": ".", "line": 0, "category": "invalid-application-sentinel",
        }]))

    def test_application_sentinel_requires_the_relocated_package(self):
        self.put("crates/app/Cargo.toml", '[package]\nname = "different-application"\n')
        status, output = self.run_checker()
        self.assertEqual((status, output), (2, [{
            "path": ".", "line": 0, "category": "invalid-application-sentinel",
        }]))

    def test_enclosing_parent_repository_is_not_an_application_repository(self):
        shutil.rmtree(self.root / ".git")
        self.git("-c", "init.templateDir=", "init", "--quiet", root=self.root.parent)
        status, output = self.run_checker()
        self.assertEqual((status, output), (2, [{"path": ".", "line": 0, "category": "missing-path"}]))

    def make_symlink(self, target, link, directory=False):
        try:
            os.symlink(target, link, target_is_directory=directory)
        except (OSError, NotImplementedError):
            self.skipTest("Creating symlinks is not permitted on this host")

    def test_candidate_and_markdown_symlinks_are_never_followed(self):
        outside = self.root.parent / "private"
        outside.mkdir()
        (outside / "private.md").write_text("not a publication input\n", encoding="utf-8")
        self.make_symlink(outside, self.root / "linked", directory=True)
        self.make_symlink(outside / "private.md", self.root / "linked.md")
        self.put("notes.md", '[direct](linked.md#private)\n[ancestor](linked/private.md)\n'
                 '[cancelled](linked/../README.md)\n')
        report = self.checker.audit()
        self.assertEqual(self.findings(report), {
            ("linked", 0, "symlink-or-reparse-point"),
            ("linked.md", 0, "symlink-or-reparse-point"),
            ("notes.md", 1, "symlink-or-reparse-point"),
            ("notes.md", 2, "symlink-or-reparse-point"),
            ("notes.md", 3, "symlink-or-reparse-point"),
        })

    def test_root_symlink_or_reparse_attribute_is_rejected(self):
        original = self.checker.Path.lstat
        root = self.root

        def reparse_lstat(path, *arguments, **keywords):
            info = original(path, *arguments, **keywords)
            if path == root:
                class ReparseInfo:
                    st_mode = stat.S_IFDIR | 0o755
                    st_file_attributes = 0x400
                return ReparseInfo()
            return info

        with mock.patch.object(self.checker.Path, "lstat", reparse_lstat):
            with self.assertRaises(self.checker.BoundaryError) as raised:
                self.checker.audit()
        self.assertEqual(raised.exception.category, "symlink-or-reparse-point")

    def test_linked_worktree_indirection_uses_the_same_application_root_guard(self):
        self.git("add", "--all")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Fixture")
        linked = self.root.parent / "linked-worktree"
        self.git("worktree", "add", "--quiet", "--detach", str(linked))
        result = subprocess.run([sys.executable, "-B", str(linked / "scripts/check-public.py"),
                                 "--root", str(linked)], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, encoding="utf-8")
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.returncode, 0)
        output = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(output), 1)
        inventory = output[0]["inventory"]
        self.assertGreater(inventory["tracked"], 0)
        self.assertEqual(inventory["nonignored_untracked"], 0)
        self.assertEqual(inventory["ignored_tracked"], 0)
        self.assertEqual(inventory["candidates"], inventory["tracked"])
        self.assertEqual(inventory["scanned_text"], inventory["candidates"])
        self.assertEqual(inventory["findings"], 0)

    def test_symlinked_application_ancestor_is_rejected(self):
        alias = self.root.parent / "application-alias"
        self.make_symlink(self.root, alias, directory=True)
        result = subprocess.run([sys.executable, "-B", str(alias / "scripts/check-public.py")],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8")
        self.assertEqual(result.stderr, "")
        self.assertEqual((result.returncode, json.loads(result.stdout)), (2, {
            "path": ".", "line": 0, "category": "symlink-or-reparse-point",
        }))

    def test_candidate_windows_reparse_attribute_prevents_content_reads(self):
        target = self.put("public.txt", "must not be opened\n")
        original_lstat = self.checker.Path.lstat
        original_open = self.checker.os.open

        def reparse_lstat(path, *arguments, **keywords):
            info = original_lstat(path, *arguments, **keywords)
            if path == target:
                class ReparseInfo:
                    st_mode = stat.S_IFREG | 0o644
                    st_file_attributes = 0x400
                return ReparseInfo()
            return info

        def guarded_open(path, *arguments, **keywords):
            if Path(path) == target:
                raise AssertionError("Reparse-point content was opened")
            return original_open(path, *arguments, **keywords)

        with mock.patch.object(self.checker.Path, "lstat", reparse_lstat):
            with mock.patch.object(self.checker.os, "open", side_effect=guarded_open):
                report = self.checker.audit()
        self.assertEqual(self.findings(report), {("public.txt", 0, "symlink-or-reparse-point")})




if __name__ == "__main__":
    unittest.main()
