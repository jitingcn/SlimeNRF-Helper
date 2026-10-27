"""Exercise the opt-in formatting hook and installer in disposable Git repositories."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which("git"), "Git is required for isolated hook tests")
class GitHookTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="slimenrf hook ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve() / "application"
        self.root.mkdir()
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                         "GIT_TERMINAL_PROMPT": "0"})
        if os.name == "nt":
            executable_path = subprocess.run(["git", "--exec-path"], env=self.env, check=True,
                                             capture_output=True, text=True).stdout.strip()
            self.shell = Path(executable_path).parents[2] / "bin/sh.exe"
            if not self.shell.is_file():
                self.skipTest("Git for Windows shell is unavailable")
        else:
            self.shell = shutil.which("sh")
            if self.shell is None:
                self.skipTest("POSIX shell is unavailable")
        self.git("-c", "init.templateDir=", "init", "--quiet")
        self.put("Cargo.toml", '[workspace]\nmembers = ["crates/app", "crates/recording"]\n')
        self.put("crates/app/Cargo.toml", '[package]\nname = "slimenrf-helper"\n')
        self.put("crates/app/src/main.rs", "fn main() {}\n")
        self.put("quality.ps1", "# fixture\n")
        self.put("quality.sh", "# fixture\n")
        self.put("README.md", "fixture\n")
        self.hook = self.root / ".githooks/pre-commit"
        self.hook.parent.mkdir()
        shutil.copyfile(ROOT / ".githooks/pre-commit", self.hook)
        self.installer = self.root / "scripts/install-hooks.ps1"
        self.installer.parent.mkdir()
        shutil.copyfile(ROOT / "scripts/install-hooks.ps1", self.installer)

    def put(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
        return path

    def git(self, *arguments):
        return subprocess.run(["git", "-C", str(self.root), *arguments], env=self.env,
                              capture_output=True, text=True, check=True).stdout

    def run_hook(self, *, cwd=None, env=None):
        return subprocess.run([str(self.shell), str(self.hook)], cwd=cwd or self.root,
                              env=env or self.env, capture_output=True, text=True)

    def run_installer(self, script=None):
        powershell = shutil.which("pwsh")
        if os.name == "nt":
            powershell = powershell or shutil.which("powershell.exe")
        elif sys.platform.startswith("linux") and powershell is not None:
            # WSL can expose Windows executables on PATH; only run a native ELF
            # against Linux fixture paths, not a PE binary or an opaque wrapper.
            try:
                with open(powershell, "rb") as executable:
                    if executable.read(4) != b"\x7fELF":
                        powershell = None
            except OSError:
                powershell = None
        if powershell is None:
            self.skipTest("Native PowerShell is required for installer behavior")
        return subprocess.run([powershell, "-NoProfile", "-NonInteractive", "-File", str(script or self.installer)],
                              cwd=self.root, env=self.env, capture_output=True, text=True)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux/WSL executable selection")
    def test_installer_never_uses_windows_interop_powershell(self):
        directory = self.root / "interop-bin"
        directory.mkdir()
        for name in ("pwsh", "powershell.exe"):
            with self.subTest(name=name):
                executable = directory / name
                executable.write_bytes(b"MZ fake Windows executable\n")
                executable.chmod(0o755)
                with mock.patch.dict(os.environ, {"PATH": str(directory)}):
                    with self.assertRaises(unittest.SkipTest):
                        self.run_installer()
                executable.unlink()

    def test_virtual_workspace_and_app_sentinels_are_required(self):
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stderr)
        for relative, value in (("Cargo.toml", '[package]\nname = "slimenrf-helper"\n'),
                                ("crates/app/Cargo.toml", '[package]\nname = "different-app"\n')):
            with self.subTest(path=relative):
                path = self.root / relative
                original = path.read_bytes()
                path.write_text(value, encoding="utf-8")
                result = self.run_hook()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("not the standalone SlimeNRF Helper application", result.stderr)
                path.write_bytes(original)
        (self.root / "crates/app/Cargo.toml").unlink()
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not the standalone SlimeNRF Helper application", result.stderr)

    def test_hook_rejects_an_enclosing_nonapplication_repository(self):
        nested = self.root / "nested"
        nested.mkdir()
        (self.root / "Cargo.toml").unlink()
        for relative, content in (
            ("Cargo.toml", '[workspace]\nmembers = ["crates/app"]\n'),
            ("quality.ps1", "# fixture\n"),
            ("quality.sh", "# fixture\n"),
            ("crates/app/Cargo.toml", '[package]\nname = "slimenrf-helper"\n'),
        ):
            path = nested / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        result = self.run_hook(cwd=nested)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not the standalone SlimeNRF Helper application", result.stderr)

    def test_hook_only_checks_format_for_relevant_changes_without_mutating_index(self):
        cargo = self.put("fixture-bin/cargo", "#!/bin/sh\nexit 19\n")
        cargo.chmod(0o755)
        env = self.env | {"PATH": str(cargo.parent) + os.pathsep + self.env.get("PATH", "")}
        self.git("add", "README.md")
        before = self.git("ls-files", "--stage")
        result = self.run_hook(env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("ls-files", "--stage"), before)
        self.git("add", "crates/app/src/main.rs")
        before = self.git("ls-files", "--stage")
        result = self.run_hook(env=env)
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertEqual(self.git("ls-files", "--stage"), before)

    def test_installer_honors_relative_hooks_path_and_refuses_replacement(self):
        self.git("config", "--local", "core.hooksPath", ".custom/hooks")
        destination = self.root / ".custom/hooks/pre-commit"
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(destination.read_bytes(), self.hook.read_bytes())
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        destination.write_bytes(b"existing user hook\n")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(destination.read_bytes(), b"existing user hook\n")

    def test_installer_rejects_parent_repository(self):
        nested = self.root / "nested"
        script = nested / "scripts/install-hooks.ps1"
        script.parent.mkdir(parents=True)
        shutil.copyfile(self.installer, script)
        result = self.run_installer(script)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("application directory must be the Git working tree root", result.stderr)
        self.assertFalse((self.root / ".git/hooks/pre-commit").exists())


if __name__ == "__main__":
    unittest.main()
