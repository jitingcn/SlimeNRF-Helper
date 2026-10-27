"""Exercise native build and packaging Python discovery with isolated processes."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""

# This is an executable fixture, not an interpreter implementation: accepted Python
# invocations run the real interpreter so the production probe and generator execute.
FIXTURE = r'''
use std::{env, fs::OpenOptions, io::Write, process::{self, Command}};

fn main() {
    let executable = env::current_exe().expect("fixture executable");
    let name = executable.file_stem().expect("fixture name").to_string_lossy();
    let host_py = env::var_os("SLIMENRF_TEST_HOST_PY")
        .is_some_and(|path| executable == std::path::Path::new(&path));
    let arguments: Vec<String> = env::args().skip(1).collect();
    let mut log = OpenOptions::new().create(true).append(true)
        .open(env::var_os("SLIMENRF_TEST_LOG").expect("fixture log"))
        .expect("open fixture log");
    writeln!(log, "{}", if host_py { "host-py" } else { name.as_ref() }).expect("write fixture log");

    if name == "uv" {
        let expected = ["python", "find", "--no-project", "--system", "--no-python-downloads", "--offline", "--no-config", ">=3.11"];
        if arguments.iter().map(String::as_str).ne(expected) {
            std::fs::write(env::var_os("SLIMENRF_TEST_DOWNLOAD").unwrap(), b"unrestricted lookup").unwrap();
            eprintln!("fixture refused a lookup without all offline and isolation options");
            process::exit(2);
        }
        let value = env::var("SLIMENRF_TEST_UV_PATH").expect("fixture uv path");
        let mode = env::var("SLIMENRF_TEST_UV_MODE").unwrap_or_default();
        match mode.as_str() {
            "error" => { eprintln!("fixture uv lookup failed"); process::exit(23); }
            "empty" => {}
            "multiple" => print!("{value}\n{value}\n"),
            "relative" => print!("python\n"),
            "nul" => { print!("{value}"); std::io::stdout().write_all(b"\0\n").unwrap(); }
            "non_utf8" => { std::io::stdout().write_all(b"/\xff\n").unwrap(); }
            _ => println!("{value}"),
        }
        return;
    }
    if name == "cargo" {
        if arguments.first().map(String::as_str) == Some("metadata") {
            println!("{{\"fixture\":\"metadata\"}}");
        } else if arguments.first().map(String::as_str) == Some("build") {
            println!("{{\"fixture\":\"artifact\"}}");
        } else {
            eprintln!("unexpected fixture cargo invocation: {arguments:?}");
            process::exit(2);
        }
        return;
    }
    if !host_py && env::var("SLIMENRF_TEST_PASS").as_deref() != Ok(name.as_ref()) {
        eprintln!("fixture {name} unavailable");
        process::exit(19);
    }
    let arguments = if name == "py" && arguments.first().map(String::as_str) == Some("-3") {
        &arguments[1..]
    } else {
        &arguments[..]
    };
    let mut python = Command::new(env::var_os("SLIMENRF_TEST_REAL_PYTHON").expect("real Python"));
    let mode = if host_py { String::new() } else { env::var("SLIMENRF_TEST_PYTHON_MODE").unwrap_or_default() };
    if arguments.first().map(String::as_str) == Some("-c") && mode == "old" {
        python.args(["-c", "import sys; sys.version_info = (3, 10, 0); exec(sys.argv[1])", &arguments[1]]);
    } else if arguments.first().map(String::as_str) == Some("-c") && mode == "no_tomllib" {
        python.args(["-c", "import builtins, sys\noriginal = builtins.__import__\ndef guarded(name, *args, **kwargs):\n    if name == 'tomllib': raise ImportError('fixture tomllib unavailable')\n    return original(name, *args, **kwargs)\nbuiltins.__import__ = guarded\nexec(sys.argv[1])", &arguments[1]]);
    } else {
        python.args(arguments);
    }
    let status = python.status().expect("run real fixture interpreter");
    process::exit(status.code().unwrap_or(1));
}
'''

GENERATOR = '''\
import argparse
import json
from pathlib import Path
import sys
parser = argparse.ArgumentParser()
parser.add_argument("--root", required=True)
parser.add_argument("--output", required=True)
args, _ = parser.parse_known_args()
Path(args.output).write_text(json.dumps({"python": sys.executable, "root": args.root}), encoding="utf-8")
'''

PACKAGE_ARTIFACT = '''\
import json
import os
from pathlib import Path
import sys
root, metadata, artifacts, no_firmware, force, format_name = sys.argv[1:]
assert json.loads(Path(metadata).read_text(encoding="utf-8")) == {"fixture": "metadata"}
assert json.loads(Path(artifacts).read_text(encoding="utf-8")) == {"fixture": "artifact"}
assert os.environ["SLIMENRF_PYTHON"] == sys.executable
assert (no_firmware, force, format_name) == ("1", "0", "elf")
Path(root, "package-result.json").write_text(json.dumps({"python": sys.executable}), encoding="utf-8")
'''


class PythonDiscoveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compilation = tempfile.TemporaryDirectory(prefix="slimenrf discovery compiler ")
        cls.addClassCleanup(cls.compilation.cleanup)
        directory = Path(cls.compilation.name)
        source = directory / "fixture.rs"
        source.write_text(FIXTURE, encoding="utf-8")
        cls.fixture = directory / ("fixture" + EXE)
        cls.compile(source, cls.fixture)
        harness = directory / "notices_harness.rs"
        module = ROOT / "crates/app/build_support/notices.rs"
        harness.write_text(
            f'#[path = r#"{module}"#] mod notices;\n'
            'fn main() { if let Err(error) = notices::build_notices() '
            '{ eprintln!("{error}"); std::process::exit(1); } }\n',
            encoding="utf-8",
        )
        cls.harness = directory / ("notices_harness" + EXE)
        cls.compile(harness, cls.harness)

    @classmethod
    def compile(cls, source, destination):
        result = subprocess.run(
            ["rustc", "--edition=2021", str(source), "-o", str(destination)],
            cwd=ROOT, capture_output=True, text=True, errors="replace",
        )
        if result.returncode:
            raise RuntimeError(f"Could not compile isolated discovery fixture:\n{result.stderr}")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="slimenrf discovery case ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / "crates/app").mkdir(parents=True)
        if os.name == "nt":
            self.harness = self.root / self.harness.name
            shutil.copy2(type(self).harness, self.harness)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.output = self.root / "output"
        self.output.mkdir()
        scripts = self.root / "scripts"
        scripts.mkdir()
        (scripts / "generate_notices.py").write_text(GENERATOR, encoding="utf-8")
        (scripts / "package-artifact.py").write_text(PACKAGE_ARTIFACT, encoding="utf-8")
        shutil.copyfile(ROOT / "scripts/environment.sh", scripts / "environment.sh")
        shutil.copyfile(ROOT / "package.sh", self.root / "package.sh")
        for name in ("py", "python3", "python", "uv", "cargo"):
            shutil.copy2(self.fixture, self.bin / (name + EXE))
            if os.name == "nt":
                # Rust searches the harness executable's directory before Windows
                # system directories and the inherited PATH, not the working directory.
                shutil.copy2(self.fixture, self.root / (name + EXE))
        self.resolved = self.root / "installed Python" / ("resolved-python" + EXE)
        self.resolved.parent.mkdir()
        shutil.copy2(self.fixture, self.resolved)
        if sys.platform.startswith("linux"):
            for tool in ("uname", "dirname", "mktemp", "rm"):
                path = shutil.which(tool)
                if path is None:
                    self.fail(f"Linux packaging fixture requires {tool}")
                (self.bin / tool).symlink_to(path)
        self.log = self.root / "invocations.txt"
        self.download = self.root / "download-attempt"
        self.env = {key: value for key, value in os.environ.items()
                    if key not in {"SLIMENRF_PYTHON", "PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"}
                    and not key.startswith("UV_") and not key.startswith("SLIMENRF_TEST_")}
        self.env.update({
            "PATH": str(self.bin),
            "CARGO_MANIFEST_DIR": str(self.root / "crates/app"),
            "OUT_DIR": str(self.output),
            "TMPDIR": str(self.root),
            "SLIMENRF_TEST_LOG": str(self.log),
            "SLIMENRF_TEST_DOWNLOAD": str(self.download),
            "SLIMENRF_TEST_UV_PATH": str(self.resolved),
            "SLIMENRF_TEST_REAL_PYTHON": str(Path(sys.executable).resolve()),
            "SLIMENRF_TEST_PASS": "resolved-python",
        })
        if os.name == "nt":
            self.host_py = self.root / "host Python" / "py.exe"
            self.host_py.parent.mkdir()
            shutil.copy2(self.fixture, self.host_py)
            self.env["SLIMENRF_TEST_HOST_PY"] = str(self.host_py)
            self.env["PATH"] = os.pathsep.join((str(self.host_py.parent), str(self.bin)))

    def entries(self):
        return ("notices", "package") if sys.platform.startswith("linux") else ("notices",)

    def run_entry(self, entry, *, cwd=None, **variables):
        self.log.unlink(missing_ok=True)
        result_file = (self.output / "THIRD_PARTY_NOTICES.txt" if entry == "notices"
                       else self.root / "package-result.json")
        result_file.unlink(missing_ok=True)
        env = self.env | variables
        command = ([str(self.harness)] if entry == "notices"
                   else ["/bin/bash", str(self.root / "package.sh"), "--no-firmware"])
        result = subprocess.run(command, cwd=cwd or self.root, env=env, capture_output=True,
                                text=True, errors="replace")
        events = self.log.read_text(encoding="utf-8").splitlines() if self.log.exists() else []
        self.assertNotIn("host-py", events, "discovery escaped to the host Python launcher")
        self.assertFalse(self.download.exists(), "discovery attempted an unrestricted uv lookup")
        return result, events, result_file

    def assert_success(self, result, result_file):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        output = json.loads(result_file.read_text(encoding="utf-8"))
        self.assertEqual(Path(output["python"]).resolve(), Path(sys.executable).resolve())

    def test_explicit_interpreter_takes_precedence(self):
        for entry in self.entries():
            with self.subTest(entry=entry):
                result, events, output = self.run_entry(entry, SLIMENRF_PYTHON=str(self.resolved))
                self.assert_success(result, output)
                self.assertIn("resolved-python", events)
                self.assertNotIn("uv", events)
                self.assertNotIn("python3", events)
                self.assertNotIn("python", events)
                self.assertNotIn("py", events)

    def test_explicit_relative_interpreter_uses_repository_root_from_app_directory(self):
        relative = self.resolved.relative_to(self.root)
        choices = [relative]
        if os.name == "nt":
            choices.append(relative.with_suffix(""))
        for choice in choices:
            with self.subTest(interpreter=str(choice)):
                result, events, output = self.run_entry(
                    "notices", cwd=self.root / "crates/app", SLIMENRF_PYTHON=str(choice),
                )
                self.assert_success(result, output)
                self.assertEqual(events, ["resolved-python", "resolved-python"])
                self.assertEqual(Path(json.loads(output.read_text(encoding="utf-8"))["root"]).resolve(), self.root)

    def test_explicit_executable_name_still_uses_path_from_app_directory(self):
        result, events, output = self.run_entry(
            "notices", cwd=self.root / "crates/app", SLIMENRF_PYTHON="python3",
            SLIMENRF_TEST_PASS="python3",
        )
        self.assert_success(result, output)
        self.assertEqual(events, ["python3", "python3"])

    def test_explicit_interpreter_probe_failure_never_falls_back(self):
        for entry in self.entries():
            for mode, message in (("old", "Python 3.11"), ("no_tomllib", "tomllib unavailable")):
                with self.subTest(entry=entry, mode=mode):
                    result, events, output = self.run_entry(
                        entry, SLIMENRF_PYTHON=str(self.resolved), SLIMENRF_TEST_PYTHON_MODE=mode,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertIn("resolved-python", events)
                    self.assertNotIn("uv", events)
                    self.assertFalse(output.exists())

    def test_invalid_or_empty_override_never_falls_back(self):
        for entry in self.entries():
            for value in ("", str(self.root / "missing-python"), f"{sys.executable} -V"):
                with self.subTest(entry=entry, override=value):
                    result, events, output = self.run_entry(entry, SLIMENRF_PYTHON=value)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Python", result.stderr)
                    self.assertNotIn("uv", events)
                    self.assertNotIn("python3", events)
                    self.assertNotIn("python", events)
                    self.assertNotIn("py", events)
                    self.assertFalse(output.exists())

    def test_regular_candidates_precede_uv(self):
        candidates = ["python3", "python"]
        if os.name == "nt":
            candidates.insert(0, "py")
        for entry in self.entries():
            for candidate in candidates:
                with self.subTest(entry=entry, candidate=candidate):
                    result, events, output = self.run_entry(entry, SLIMENRF_TEST_PASS=candidate)
                    self.assert_success(result, output)
                    self.assertIn(candidate, events)
                    self.assertNotIn("uv", events)

    @unittest.skipUnless(os.name == "nt", "Windows executable search")
    def test_windows_host_launcher_cannot_override_isolated_candidates(self):
        host_harness = self.host_py.parent / self.harness.name
        shutil.copy2(self.harness, host_harness)
        result = subprocess.run([str(host_harness)], cwd=self.root, env=self.env,
                                capture_output=True, text=True, errors="replace")
        output = self.output / "THIRD_PARTY_NOTICES.txt"
        self.assert_success(result, output)
        self.assertEqual(self.log.read_text(encoding="utf-8").splitlines(),
                         ["host-py", "host-py"])

        result, events, output = self.run_entry("notices")
        self.assert_success(result, output)
        self.assertEqual(events, ["py", "python3", "python", "uv",
                                  "resolved-python", "resolved-python"])

    def test_uv_uses_an_installed_interpreter_with_spaces(self):
        for entry in self.entries():
            with self.subTest(entry=entry):
                result, events, output = self.run_entry(entry)
                self.assert_success(result, output)
                self.assertIn("uv", events)
                self.assertIn("resolved-python", events)
                self.assertLess(events.index("python3"), events.index("uv"))
                if os.name == "nt":
                    self.assertLess(events.index("py"), events.index("python3"))
                self.assertLess(events.index("python"), events.index("uv"))
                self.assertLess(events.index("uv"), events.index("resolved-python"))

    @unittest.skipUnless(sys.platform.startswith("linux"), "isolated PATH lookup on Linux")
    def test_uv_works_when_regular_interpreters_are_absent(self):
        (self.bin / "python3").unlink()
        (self.bin / "python").unlink()
        for entry in self.entries():
            with self.subTest(entry=entry):
                result, events, output = self.run_entry(entry)
                self.assert_success(result, output)
                self.assertIn("uv", events)
                self.assertIn("resolved-python", events)

    @unittest.skipUnless(sys.platform.startswith("linux"), "isolated PATH lookup on Linux")
    def test_missing_uv_preserves_candidate_diagnostics(self):
        (self.bin / "uv").unlink()
        for entry in self.entries():
            with self.subTest(entry=entry):
                result, events, output = self.run_entry(entry)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("fixture python3 unavailable", result.stderr)
                self.assertIn("fixture python unavailable", result.stderr)
                self.assertIn("uv", result.stderr)
                self.assertIn("downloads Python", result.stderr)
                self.assertNotIn("resolved-python", events)
                self.assertFalse(output.exists())

    def test_uv_missing_interpreter_fails_its_probe(self):
        for entry in self.entries():
            with self.subTest(entry=entry):
                result, events, output = self.run_entry(
                    entry, SLIMENRF_TEST_UV_PATH=str(self.root / ("missing-python" + EXE)),
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("uv", result.stderr)
                self.assertIn("missing-python", result.stderr)
                self.assertIn("uv", events)
                self.assertFalse(output.exists())

    def test_uv_failure_and_invalid_results_never_run_generator(self):
        for entry in self.entries():
            for mode in ("error", "empty", "multiple", "relative", "nul", "non_utf8"):
                with self.subTest(entry=entry, mode=mode):
                    result, events, output = self.run_entry(entry, SLIMENRF_TEST_UV_MODE=mode)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("uv", result.stderr)
                    self.assertIn("uv", events)
                    self.assertNotIn("resolved-python", events)
                    self.assertFalse(output.exists())
                    if mode == "error":
                        self.assertIn("fixture uv lookup failed", result.stderr)

    def test_uv_result_must_pass_real_version_and_tomllib_probes(self):
        for entry in self.entries():
            for mode, message in (("old", "Python 3.11"), ("no_tomllib", "tomllib unavailable")):
                with self.subTest(entry=entry, mode=mode):
                    result, events, output = self.run_entry(entry, SLIMENRF_TEST_PYTHON_MODE=mode)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("uv", events)
                    self.assertIn("resolved-python", events)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
