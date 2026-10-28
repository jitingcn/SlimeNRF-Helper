use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn executable_on_path(path: &OsStr, name: &str) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            let Ok(metadata) = fs::metadata(candidate) else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
}

fn native_powershell(path: &OsStr) -> Option<PathBuf> {
    #[cfg(windows)]
    let names = ["pwsh.exe", "pwsh.cmd", "pwsh.bat", "pwsh", "powershell.exe"];
    #[cfg(not(windows))]
    let names = ["pwsh"];
    let executable = names
        .iter()
        .find_map(|name| executable_on_path(path, name))?;
    #[cfg(target_os = "linux")]
    {
        let mut magic = [0u8; 4];
        if fs::File::open(&executable)
            .and_then(|mut file| file.read_exact(&mut magic))
            .is_err()
            || magic != *b"\x7fELF"
        {
            return None;
        }
    }
    Some(executable)
}

fn mark_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .expect("read fixture permissions")
            .permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions).expect("mark fixture executable");
    }
    #[cfg(not(unix))]
    let _ = path;
}

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
    hook: PathBuf,
    installer: PathBuf,
    shell: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl Fixture {
    fn new() -> Option<Self> {
        let mut environment: Vec<_> = env::vars_os()
            .filter(|(key, _)| {
                !key.to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("GIT_")
            })
            .collect();
        environment.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            (
                "GIT_CONFIG_GLOBAL".into(),
                if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
            ),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ]);
        let mut version = Command::new("git");
        version
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .arg("--version");
        let output = match version.output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("cannot start Git: {error}"),
        };
        assert!(
            output.status.success(),
            "git --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        #[cfg(windows)]
        let shell = {
            let output = Command::new("git")
                .env_clear()
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .arg("--exec-path")
                .output()
                .expect("query Git for Windows exec path");
            assert!(
                output.status.success(),
                "git --exec-path failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            let shell = path.parent()?.parent()?.parent()?.join("bin/sh.exe");
            if !shell.is_file() {
                return None;
            }
            shell
        };
        #[cfg(not(windows))]
        let shell = executable_on_path(&env::var_os("PATH")?, "sh")?;

        let temporary = tempfile::Builder::new()
            .prefix("slimenrf hook ")
            .tempdir()
            .expect("create Git fixture");
        let root = temporary.path().join("application");
        fs::create_dir(&root).expect("create Git fixture root");
        let hook = root.join(".githooks/pre-commit");
        let installer = root.join("scripts/install-hooks.ps1");
        let fixture = Self {
            _temporary: temporary,
            root,
            hook,
            installer,
            shell,
            environment,
        };
        fixture.checked_git(&["-c", "init.templateDir=", "init", "--quiet"]);
        fixture.checked_git(&["config", "--local", "user.name", "Hook Test"]);
        fixture.checked_git(&[
            "config",
            "--local",
            "user.email",
            "hook-test@example.invalid",
        ]);
        fixture.checked_git(&["config", "--local", "commit.gpgsign", "false"]);
        fixture.checked_git(&["config", "--local", "core.autocrlf", "false"]);
        fixture.put(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/app\", \"crates/recording\"]\n",
        );
        fixture.put(
            "crates/app/Cargo.toml",
            "[package]\nname = \"slimenrf-helper\"\n",
        );
        fixture.put("crates/app/src/main.rs", "fn main() {}\n");
        fixture.put("quality.ps1", "# fixture\n");
        fixture.put("quality.sh", "# fixture\n");
        fixture.put("README.md", "fixture\n");
        fs::create_dir_all(fixture.hook.parent().expect("hook parent"))
            .expect("create hook directory");
        fs::create_dir_all(fixture.installer.parent().expect("installer parent"))
            .expect("create script directory");
        fs::copy(
            crate::workspace_root().join(".githooks/pre-commit"),
            &fixture.hook,
        )
        .expect("copy production hook");
        fs::copy(
            crate::workspace_root().join("scripts/install-hooks.ps1"),
            &fixture.installer,
        )
        .expect("copy production installer");
        mark_executable(&fixture.hook);
        Some(fixture)
    }

    fn put(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(&path, text).expect("write fixture file");
        path
    }

    fn command(&self, executable: impl AsRef<OsStr>, cwd: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(cwd)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)));
        command
    }

    fn git(&self, arguments: &[&str]) -> Output {
        self.command("git", &self.root)
            .args(arguments)
            .output()
            .expect("run Git in fixture")
    }

    fn checked_git(&self, arguments: &[&str]) -> String {
        let output = self.git(arguments);
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("Git fixture output is UTF-8")
    }

    fn run_hook(&self, cwd: &Path, extra_path: Option<&Path>) -> Output {
        let mut command = self.command(&self.shell, cwd);
        command.arg(&self.hook);
        if let Some(path) = extra_path {
            command.env("PATH", self.with_path_prefix(path));
        }
        command.output().expect("run production hook")
    }

    fn with_path_prefix(&self, path: &Path) -> OsString {
        let old = self
            .environment
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value.as_os_str())
            .unwrap_or(OsStr::new(""));
        env::join_paths(std::iter::once(path.to_path_buf()).chain(env::split_paths(old)))
            .expect("fixture PATH")
    }

    fn commit_with_path(&self, arguments: &[&str], path: &Path) -> Output {
        self.command("git", &self.root)
            .env("PATH", self.with_path_prefix(path))
            .env("CDPATH", ".")
            .args(arguments)
            .output()
            .expect("run commit through hook")
    }

    fn run_installer(&self, script: &Path) -> Option<Output> {
        let path = self
            .environment
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))?
            .1
            .as_os_str();
        let powershell = native_powershell(path)?;
        Some(
            self.command(powershell, &self.root)
                .args(["-NoProfile", "-NonInteractive", "-File"])
                .arg(script)
                .output()
                .expect("run native PowerShell installer"),
        )
    }
}

#[cfg(target_os = "linux")]
#[test]
fn installer_never_uses_windows_interop_powershell() {
    let temporary = tempfile::tempdir().expect("create interop fixture");
    for name in ["pwsh", "powershell.exe"] {
        let executable = temporary.path().join(name);
        fs::write(&executable, b"MZ fake Windows executable\n").expect("write interop executable");
        mark_executable(&executable);
        assert!(
            native_powershell(temporary.path().as_os_str()).is_none(),
            "selected Windows interop {name}"
        );
        fs::remove_file(executable).expect("remove interop executable");
    }
}

#[test]
fn virtual_workspace_and_app_sentinels_are_required() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("Git and native shell are required");
        return;
    };
    let output = fixture.run_hook(&fixture.root, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (relative, value) in [
        ("Cargo.toml", "[package]\nname = \"slimenrf-helper\"\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"different-app\"\n",
        ),
    ] {
        let path = fixture.root.join(relative);
        let original = fs::read(&path).expect("read sentinel");
        fs::write(&path, value).expect("change sentinel");
        let output = fixture.run_hook(&fixture.root, None);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("not the standalone SlimeNRF Helper application")
        );
        fs::write(path, original).expect("restore sentinel");
    }
    fs::remove_file(fixture.root.join("crates/app/Cargo.toml")).expect("remove sentinel");
    let output = fixture.run_hook(&fixture.root, None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("not the standalone SlimeNRF Helper application")
    );
}

#[test]
fn hook_rejects_an_enclosing_nonapplication_repository() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("Git and native shell are required");
        return;
    };
    fs::remove_file(fixture.root.join("Cargo.toml")).expect("remove parent sentinel");
    let nested = fixture.root.join("nested");
    for (relative, content) in [
        (
            "nested/Cargo.toml",
            "[workspace]\nmembers = [\"crates/app\"]\n",
        ),
        ("nested/quality.ps1", "# fixture\n"),
        ("nested/quality.sh", "# fixture\n"),
        (
            "nested/crates/app/Cargo.toml",
            "[package]\nname = \"slimenrf-helper\"\n",
        ),
    ] {
        fixture.put(relative, content);
    }
    let output = fixture.run_hook(&nested, None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("not the standalone SlimeNRF Helper application")
    );
}

#[test]
fn hook_only_checks_format_for_relevant_changes_without_mutating_index() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("Git and native shell are required");
        return;
    };
    let cargo = fixture.put("fixture-bin/cargo", "#!/bin/sh\nexit 19\n");
    mark_executable(&cargo);
    let path = cargo.parent().expect("fixture bin");
    fixture.checked_git(&["config", "--local", "core.hooksPath", ".githooks"]);
    fixture.checked_git(&["add", "README.md"]);
    let before = fixture.checked_git(&["ls-files", "--stage"]);
    let output = fixture.run_hook(&fixture.root, Some(path));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.checked_git(&["ls-files", "--stage"]), before);
    let output = fixture.commit_with_path(&["commit", "-m", "safe baseline"], path);
    assert!(
        output.status.success(),
        "safe commit: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fixture.checked_git(&["add", "crates/app/src/main.rs"]);
    let before = fixture.checked_git(&["ls-files", "--stage"]);
    let tree = fixture.checked_git(&["write-tree"]);
    let head = fixture.checked_git(&["rev-parse", "HEAD"]);
    let output = fixture.run_hook(&fixture.root, Some(path));
    assert_eq!(
        output.status.code(),
        Some(19),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.checked_git(&["ls-files", "--stage"]), before);
    let output = fixture.commit_with_path(&["commit", "-m", "relevant change"], path);
    assert!(
        !output.status.success(),
        "format failure must reject the commit"
    );
    assert_eq!(fixture.checked_git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.checked_git(&["write-tree"]), tree);
    assert_eq!(fixture.checked_git(&["ls-files", "--stage"]), before);

    fixture.put("README.md", "partial commit\n");
    let output = fixture.commit_with_path(
        &[
            "commit",
            "--only",
            "-m",
            "safe partial commit",
            "--",
            "README.md",
        ],
        path,
    );
    assert!(
        output.status.success(),
        "partial commit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fixture
            .checked_git(&["diff", "--cached", "--name-only"])
            .lines()
            .collect::<Vec<_>>(),
        ["crates/app/src/main.rs"]
    );
    assert_eq!(
        fixture
            .checked_git(&["show", "--format=", "--name-only", "HEAD"])
            .lines()
            .collect::<Vec<_>>(),
        ["README.md"]
    );
    let head = fixture.checked_git(&["rev-parse", "HEAD"]);
    let tree = fixture.checked_git(&["write-tree"]);
    let output = fixture.commit_with_path(&["commit", "-m", "still relevant"], path);
    assert!(
        !output.status.success(),
        "remaining format failure must reject commit"
    );
    assert_eq!(fixture.checked_git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.checked_git(&["write-tree"]), tree);
}

#[test]
fn installer_honors_relative_hooks_path_and_refuses_replacement() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("Git and native shell are required");
        return;
    };
    fixture.checked_git(&["config", "--local", "core.hooksPath", ".custom/hooks"]);
    let other = fixture.put(".custom/hooks/post-commit", "existing post hook\n");
    let destination = fixture.root.join(".custom/hooks/pre-commit");
    let Some(output) = fixture.run_installer(&fixture.installer) else {
        eprintln!("native PowerShell is required");
        return;
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&destination).expect("installed hook"),
        fs::read(&fixture.hook).expect("source hook")
    );
    let output = fixture
        .run_installer(&fixture.installer)
        .expect("native PowerShell remains available");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(&destination, b"existing user hook\n").expect("install another hook");
    let output = fixture
        .run_installer(&fixture.installer)
        .expect("native PowerShell remains available");
    assert!(!output.status.success());
    assert_eq!(
        fs::read(&destination).expect("existing hook"),
        b"existing user hook\n"
    );
    assert_eq!(
        fs::read(&other).expect("other hook"),
        b"existing post hook\n"
    );
    assert_eq!(
        fixture
            .checked_git(&["config", "--local", "core.hooksPath"])
            .trim(),
        ".custom/hooks"
    );
}

#[test]
fn installer_rejects_parent_repository() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("Git and native shell are required");
        return;
    };
    let script = fixture.root.join("nested/scripts/install-hooks.ps1");
    fs::create_dir_all(script.parent().expect("nested script parent"))
        .expect("create nested scripts");
    fs::copy(&fixture.installer, &script).expect("copy nested installer");
    let Some(output) = fixture.run_installer(&script) else {
        eprintln!("native PowerShell is required");
        return;
    };
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("application directory must be the Git working tree root")
    );
    assert!(!fixture.root.join(".git/hooks/pre-commit").exists());
}
