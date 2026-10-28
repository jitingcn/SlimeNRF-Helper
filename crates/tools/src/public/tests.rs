use super::*;
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

#[derive(Default)]
struct AccessTrace {
    opened: Vec<PathBuf>,
    reparse: Option<PathBuf>,
}

thread_local! {
    static ACCESS_TRACE: RefCell<AccessTrace> = RefCell::new(AccessTrace::default());
}

pub(super) fn record_open(path: &Path) {
    ACCESS_TRACE.with(|trace| trace.borrow_mut().opened.push(path.to_owned()));
}

pub(super) fn injected_reparse(path: &Path) -> bool {
    ACCESS_TRACE.with(|trace| trace.borrow().reparse.as_deref() == Some(path))
}

fn reset_trace() {
    ACCESS_TRACE.with(|trace| *trace.borrow_mut() = AccessTrace::default());
}

fn inject_reparse(path: &Path) {
    ACCESS_TRACE.with(|trace| trace.borrow_mut().reparse = Some(path.to_owned()));
}

fn assert_not_opened(paths: &[PathBuf]) {
    ACCESS_TRACE.with(|trace| {
        let trace = trace.borrow();
        for path in paths {
            assert!(
                !trace.opened.contains(path),
                "content opened for {}",
                path.display()
            );
        }
    });
}

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        reset_trace();
        let temporary = tempfile::tempdir().expect("create isolated fixture");
        let root = temporary.path().join("application");
        fs::create_dir(&root).expect("create fixture application");
        let fixture = Self {
            _temporary: temporary,
            root,
        };
        fixture.put(
            "Cargo.toml",
            b"[workspace]\nmembers = [\"crates/app\", \"crates/recording\"]\n",
        );
        fixture.put(
            "crates/app/Cargo.toml",
            b"[package]\nname = \"slimenrf-helper\"\n",
        );
        fixture.put(
            "crates/recording/Cargo.toml",
            b"[package]\nname = \"slimenrf-recording\"\n",
        );
        fixture.put("crates/app/src/main.rs", b"fn main() {}\n");
        for english in MAINTAINED {
            let stem = english.strip_suffix(".md").expect("markdown suffix");
            let name = stem.rsplit('/').next().expect("document name");
            fixture.put(
                english,
                format!("# Review fixture\n\n[中文]({name}.zh-CN.md)\n").as_bytes(),
            );
            fixture.put(
                &format!("{stem}.zh-CN.md"),
                format!("# 审查样例\n\n[English]({name}.md)\n").as_bytes(),
            );
        }
        fixture.git_at(
            &fixture.root,
            &["-c", "init.templateDir=", "init", "--quiet"],
        );
        fixture
    }

    fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture path has parent"))
            .expect("create fixture parent");
        fs::write(&path, bytes).expect("write fixture");
        path
    }

    fn git_at(&self, root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.arg("-C").arg(root).args(args);
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_TERMINAL_PROMPT", "0");
        let output = command.output().expect("run fixture git");
        assert!(
            output.status.success(),
            "fixture git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git(&self, args: &[&str]) {
        self.git_at(&self.root, args);
    }

    fn audit(&self) -> Report {
        audit_at(&self.root, None).expect("fixture audit should pass boundary checks")
    }
}

fn finding(path: &str, line: usize, category: &'static str) -> Finding {
    Finding {
        path: path.to_owned(),
        line,
        category,
    }
}

fn findings(items: &[(&str, usize, &'static str)]) -> BTreeSet<Finding> {
    items
        .iter()
        .map(|(path, line, category)| finding(path, *line, category))
        .collect()
}

fn symlink(target: &Path, link: &Path, directory: bool) -> bool {
    #[cfg(unix)]
    let result = {
        let _ = directory;
        std::os::unix::fs::symlink(target, link)
    };
    #[cfg(windows)]
    let result = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = {
        let _ = (target, link, directory);
        Err(std::io::Error::other("symlinks unavailable"))
    };
    result.is_ok()
}

#[test]
fn initial_repository_scans_nonignored_candidates_before_first_commit() {
    let fixture = Fixture::new();
    let report = fixture.audit();
    assert_eq!(report.inventory.tracked.len(), 0);
    assert!(!report.inventory.untracked.is_empty());
    assert!(report.inventory.ignored_tracked.is_empty());
    assert_eq!(
        report.inventory.candidates().len(),
        report.inventory.untracked.len()
    );
    assert_eq!(report.scanned_text, report.inventory.candidates().len());
    assert!(report.findings.is_empty());
    let output = render_report(&report);
    assert_eq!(output.len(), 1);
    let summary: serde_json::Value =
        serde_json::from_str(&output[0]).expect("valid JSON inventory");
    assert_eq!(summary["inventory"]["tracked"], 0);
    assert_eq!(summary["inventory"]["ignored_tracked"], 0);
    assert_eq!(summary["inventory"]["findings"], 0);
    assert_eq!(
        summary["inventory"]["nonignored_untracked"],
        report.inventory.untracked.len()
    );
    assert_eq!(
        summary["inventory"]["scanned_text"],
        report.inventory.candidates().len()
    );
}

#[test]
fn inventory_keeps_tracked_untracked_and_ignored_tracked_separate() {
    let fixture = Fixture::new();
    fixture.put("tracked.txt", b"public\n");
    fixture.put("now-ignored.txt", b"still reviewed\n");
    fixture.git(&["add", "tracked.txt", "now-ignored.txt"]);
    fixture.put("untracked.txt", b"public\n");
    fixture.put(".gitignore", b"now-ignored.txt\nignored-only.txt\n");
    fixture.put("ignored-only.txt", b"not a publication candidate\n");
    let report = fixture.audit();
    assert_eq!(report.inventory.tracked, ["now-ignored.txt", "tracked.txt"]);
    assert_eq!(report.inventory.ignored_tracked, ["now-ignored.txt"]);
    assert!(
        report
            .inventory
            .untracked
            .iter()
            .any(|path| path == "untracked.txt")
    );
    assert!(!report.inventory.candidates().contains("ignored-only.txt"));
    assert!(report.findings.is_empty());
    assert_eq!(report.scanned_text, report.inventory.candidates().len());
}

#[test]
fn new_crate_sources_are_inventoried_and_scanned() {
    let fixture = Fixture::new();
    let tracked = "crates/terminal-core/src/lib.rs";
    let untracked = "crates/firmware-image/src/parser.rs";
    let secret = ["// ", "token", " = \"public-review-fixture\"\n"].concat();
    for path in [tracked, untracked] {
        fixture.put(path, secret.as_bytes());
    }
    fixture.git(&["add", "--", tracked]);
    let report = fixture.audit();
    assert!(report.inventory.tracked.iter().any(|path| path == tracked));
    assert!(
        report
            .inventory
            .untracked
            .iter()
            .any(|path| path == untracked)
    );
    assert_eq!(
        report.findings,
        findings(&[
            (tracked, 1, "credential-assignment"),
            (untracked, 1, "credential-assignment")
        ])
    );
}

#[test]
fn forbidden_tracked_paths_are_flagged_without_content_reads() {
    let fixture = Fixture::new();
    let paths = [
        ".local/private.txt",
        "dist/export.txt",
        "captures/session.csv",
        "recordings/session.csv",
        "firmwares/image.hex",
        ".env",
        "signing.pem",
        "preferences.json",
        "device-registry.json",
        "custom-shortcuts.json",
        ".cargo/credentials.toml",
        ".git-credentials",
        ".vscode/state.json",
    ];
    let full: Vec<_> = paths
        .iter()
        .map(|path| fixture.put(path, b"unopened private fixture\n"))
        .collect();
    let mut args = vec!["add", "--force", "--"];
    args.extend(paths);
    fixture.git(&args);
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&full);
    assert_eq!(
        report.findings,
        paths
            .iter()
            .map(|path| finding(path, 0, "forbidden-publication-path"))
            .collect()
    );
}

#[test]
fn secret_forms_and_private_roots_report_only_locations() {
    let fixture = Fixture::new();
    let drive = "C:";
    let home = "home";
    let users = "Users";
    let mount = "mnt";
    let unc = "\\\\";
    let token = "token";
    let bearer = "Bearer";
    let scheme = "https://";
    let signature = "sig";
    let github_prefix = "ghp_";
    let key_type = "OPENSSH";
    let basic = "Basic";
    let github_token = "GH_TOKEN";
    let lines = [
        format!("{drive}\\Users\\sample-person\\work\\project"),
        format!("/{home}/sample-person/project"),
        format!("/{users}/sample-person/project"),
        format!("/{mount}/c/workstation/project"),
        format!("{unc}private-host\\share\\project"),
        format!("{token} = \"sample-assignment-value\""),
        format!("{bearer} sample-authorization-value"),
        format!("{scheme}reader:sample-password@example.invalid/a"),
        format!("https://example.invalid/a?{signature}=sample-signature"),
        format!("{github_prefix}{}", "a".repeat(24)),
        format!("-----BEGIN {key_type} PRIVATE KEY-----"),
        format!("Authorization: {basic} c2FtcGxlOnNhbXBsZQ=="),
        format!("{github_token}=sample-environment-value"),
    ];
    fixture.put("public.txt", format!("{}\n", lines.join("\n")).as_bytes());
    let report = fixture.audit();
    let categories = [
        "private-absolute-path",
        "private-absolute-path",
        "private-absolute-path",
        "private-absolute-path",
        "private-absolute-path",
        "credential-assignment",
        "authorization-value",
        "authenticated-url",
        "signed-credential",
        "credential-token",
        "private-key",
        "authorization-value",
        "credential-assignment",
    ];
    assert_eq!(
        report.findings,
        categories
            .iter()
            .enumerate()
            .map(|(index, category)| finding("public.txt", index + 1, category))
            .collect()
    );
    let output = render_report(&report).join("\n");
    for value in [
        "sample-person",
        "sample-password",
        "sample-signature",
        "private-host",
    ] {
        assert!(!output.contains(value));
    }
}

#[test]
fn reviewed_synthetic_credentials_do_not_exempt_new_values_or_files() {
    let fixture = ["https://", "user:password@api.github.com/credentials"].concat();
    let bearer = ["bearer", " ", "fixture-secret"].concat();
    let path = "crates/app/src/firmware_source/http_tests.rs";
    assert!(scan_text(path, &format!("{fixture}\n{bearer}")).is_empty());
    assert_eq!(
        scan_text(path, &fixture.replace("password", "different-password")),
        findings(&[(path, 1, "authenticated-url")])
    );
    for other in ["src/firmware_source/http_tests.rs", "other.rs"] {
        assert_eq!(
            scan_text(other, &bearer),
            findings(&[(other, 1, "authorization-value")])
        );
    }
    assert!(
        scan_text(
            "public.rs",
            &format!(
                "https://github.com/upstream/repository\n{}\nconst USB_VENDOR: u16 = 0x1209;\n",
                "a".repeat(64)
            )
        )
        .is_empty()
    );
}

#[test]
fn app_update_credential_fixtures_are_exact_and_file_scoped() {
    let path = "crates/app/src/app_update/http_tests.rs";
    let scheme = "https://";
    let signature = "sig";
    let token = "token";
    let cases = [
        (
            "authenticated-url",
            format!("{scheme}user:fixture-secret@api.github.com/private-fixture-path"),
        ),
        ("signed-credential", format!("?{signature}=FiXture%2Fpart")),
        ("signed-credential", format!("?{signature}=fixture-secret")),
        (
            "signed-credential",
            format!("?{signature}=fixture-secret-signature"),
        ),
    ];
    for (category, value) in cases {
        assert!(scan_text(path, &value).is_empty());
        let changed = value
            .replace("fixture", "different")
            .replace("FiXture", "different");
        assert_eq!(scan_text(path, &changed), findings(&[(path, 1, category)]));
        assert_eq!(
            scan_text("other.rs", &value),
            findings(&[("other.rs", 1, category)])
        );
    }
    let extra = format!("?{signature}=fixture-secret&{token}=unreviewed-value");
    assert_eq!(
        scan_text(path, &extra),
        findings(&[(path, 1, "signed-credential")])
    );
}

#[test]
fn app_update_windows_fixture_paths_require_exact_complete_lines() {
    let path = "crates/app/src/app_update/windows/tests.rs";
    let drive = "C:";
    let target = format!(
        "target: wide(OsString::from(r\"{drive}\\fixture\\application.exe\").as_os_str()),",
    );
    let working =
        format!("working_directory: wide(OsString::from(r\"{drive}\\fixture\").as_os_str()),");
    for line in [target, working] {
        assert!(scan_text(path, &format!("    {line}")).is_empty());
        assert_eq!(
            scan_text("other.rs", &line),
            findings(&[("other.rs", 1, "private-absolute-path")])
        );
        assert_eq!(
            scan_text(path, &line.replace("fixture", "private")),
            findings(&[(path, 1, "private-absolute-path")])
        );
        let suffix = format!("{line} // {drive}\\private\\customer");
        assert_eq!(
            scan_text(path, &suffix),
            findings(&[(path, 1, "private-absolute-path")])
        );
    }
}

#[test]
fn reviewed_public_tool_path_requires_the_exact_complete_line() {
    let tool = ["C:", "Program Files", "LLVM", "bin", "libclang.dll"].join("\\");
    let line = format!("if (-not (Test-Path -LiteralPath '{tool}' -PathType Leaf)) {{");
    let path = ".github/workflows/ci.yml";
    assert!(scan_text(path, &line).is_empty());
    assert_eq!(
        scan_text(path, &line.replace("libclang.dll", "private-signing.key")),
        findings(&[(path, 1, "private-absolute-path")])
    );
}

#[test]
fn reviewed_private_path_does_not_exempt_a_whitespace_suffix() {
    let fixture = Fixture::new();
    let path = "crates/app/src/bundled_resources/tests.rs";
    let drive = "C:";
    fixture.put(
        path,
        format!("        \"{drive}/firmwares/a.bin\",\n").as_bytes(),
    );
    assert!(fixture.audit().findings.is_empty());
    fixture.put(
        path,
        format!("        \"{drive}/firmwares/a.bin private/customer/file\",\n").as_bytes(),
    );
    let report = fixture.audit();
    assert_eq!(
        report.findings,
        findings(&[(path, 1, "private-absolute-path")])
    );
    let output = render_report(&report).join("\n");
    assert!(!output.contains("firmwares/a.bin"));
    assert!(!output.contains("private/customer/file"));
}

#[test]
fn only_relocated_public_receiver_log_is_allowed() {
    assert!(!forbidden(
        "crates/app/src/rssi_scan/fixtures/receiver_scan.log"
    ));
    assert!(forbidden("src/rssi_scan/fixtures/receiver_scan.log"));
    assert!(forbidden("crates/app/src/rssi_scan/fixtures/private.log"));
}

#[test]
fn reviewed_translation_labels_do_not_exempt_credentials() {
    for (path, label) in [("locales/en.yml", "Token"), ("locales/zh-CN.yml", "令牌")] {
        let name = "token";
        let approved = format!("{name}: \"{label}\"");
        assert!(scan_text(path, &approved).is_empty());
        let changed = format!("{name}: \"unreviewed-private-value\"");
        assert_eq!(
            scan_text(path, &changed),
            findings(&[(path, 1, "credential-assignment")])
        );
        assert_eq!(
            scan_text("notes.yml", &approved),
            findings(&[("notes.yml", 1, "credential-assignment")])
        );
    }
}

#[test]
fn binary_undecodable_and_missing_candidates_fail() {
    let fixture = Fixture::new();
    fixture.put("image.png", b"\x89PNG\0fixture");
    fixture.put(
        "printable.png",
        b"printable bytes do not clear a binary asset",
    );
    fixture.put("unknown.txt", b"\xfffixture");
    let missing = fixture.put("missing.txt", b"previously indexed\n");
    fixture.git(&["add", "missing.txt"]);
    fs::remove_file(missing).expect("remove tracked fixture");
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[
        fixture.root.join("image.png"),
        fixture.root.join("printable.png"),
    ]);
    assert_eq!(
        report.findings,
        findings(&[
            ("image.png", 0, "unreviewed-binary"),
            ("missing.txt", 0, "missing-path"),
            ("printable.png", 0, "unreviewed-binary"),
            ("unknown.txt", 0, "unreviewed-undecodable")
        ])
    );
}

#[test]
fn markdown_heading_explicit_reference_and_encoded_targets() {
    let fixture = Fixture::new();
    fixture.put("notes.md", b"# Notes\n\n[space](docs/space%20name.md#keep-this)\n[heading](#use-tool)\n[repeat](#use-tool-1)\n[ref][target]\n[target]: docs/space%20name.md#keep-this\n\n## Use `tool`\n## Use `tool`\n`[not a link](missing.md)`\n```md\n[not a link](missing.md)\n```\n");
    fixture.put(
        "docs/space name.md",
        b"# Target\n<a id=\"keep-this\"></a>\n",
    );
    assert!(fixture.audit().findings.is_empty());
    fixture.put(
        "notes.md",
        b"[missing](docs/space%20name.md#absent)\n[broken](missing.md)\n",
    );
    assert_eq!(
        fixture.audit().findings,
        findings(&[
            ("notes.md", 1, "markdown-missing-anchor"),
            ("notes.md", 2, "missing-path")
        ])
    );
}

#[test]
fn markdown_forbidden_and_escaping_targets_never_open_contents() {
    let fixture = Fixture::new();
    fixture.put(".gitignore", b".local/\nhidden.md\n");
    let private = fixture.put(".local/private.md", b"must not be opened\n");
    let hidden = fixture.put("hidden.md", b"not a publication input\n");
    fixture.put("notes.md", b"[forbidden](.local/private.md#secret)\n[escape](%2e%2e/private.md)\n[file](file:///private/file.md)\n[reference][missing]\n[ignored](hidden.md#private)\n");
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[private, hidden]);
    assert_eq!(
        report.findings,
        findings(&[
            ("notes.md", 1, "forbidden-publication-path"),
            ("notes.md", 2, "markdown-target-outside-root"),
            ("notes.md", 3, "markdown-unsafe-target"),
            ("notes.md", 4, "markdown-unsafe-target"),
            ("notes.md", 5, "markdown-target-not-reviewed-text")
        ])
    );
}

#[test]
fn markdown_directories_require_publication_candidates_without_ignored_reads() {
    let fixture = Fixture::new();
    fixture.put(".gitignore", b"hidden-docs/\npublic-docs/ignored/\n");
    let hidden = fixture.put("hidden-docs/README.md", b"ignored directory content\n");
    let nested = fixture.put("public-docs/ignored/README.md", b"ignored nested content\n");
    fixture.put("public-docs/guide.md", b"# Public guide\n");
    fixture.put(
        "notes.md",
        b"[ignored](hidden-docs/)\n[public](public-docs/)\n[root](.)\n",
    );
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[hidden, nested]);
    assert_eq!(
        report.findings,
        findings(&[("notes.md", 1, "markdown-target-not-published")])
    );
    let candidates = report.inventory.candidates();
    assert!(candidates.contains("public-docs/guide.md"));
    assert!(!candidates.contains("hidden-docs/README.md"));
    assert!(!candidates.contains("public-docs/ignored/README.md"));
}

#[test]
fn language_pairing_navigation_and_explicit_anchor_parity() {
    let fixture = Fixture::new();
    fixture.put(
        "docs/USER_GUIDE.zh-CN.md",
        "# 指南\n\n[English](USER_GUIDE.md)\n[wrong](../README.md)\n<a id=\"only-chinese\"></a>\n"
            .as_bytes(),
    );
    fs::remove_file(fixture.root.join("README.zh-CN.md")).expect("remove Chinese document");
    assert_eq!(
        fixture.audit().findings,
        findings(&[
            ("README.md", 1, "missing-reciprocal-language-link"),
            ("README.md", 3, "missing-path"),
            ("README.zh-CN.md", 0, "missing-language-document"),
            (
                "docs/USER_GUIDE.zh-CN.md",
                4,
                "markdown-language-navigation"
            ),
            ("docs/USER_GUIDE.md", 0, "language-explicit-anchor-mismatch"),
            (
                "docs/USER_GUIDE.zh-CN.md",
                0,
                "language-explicit-anchor-mismatch"
            )
        ])
    );
}

#[test]
fn changelog_requires_its_translated_document_and_reciprocal_link() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("CHANGELOG.zh-CN.md")).expect("remove Chinese changelog");
    assert_eq!(
        fixture.audit().findings,
        findings(&[
            ("CHANGELOG.md", 1, "missing-reciprocal-language-link"),
            ("CHANGELOG.md", 3, "missing-path"),
            ("CHANGELOG.zh-CN.md", 0, "missing-language-document")
        ])
    );
}

#[test]
fn changelog_navigation_preserves_the_selected_language() {
    let fixture = Fixture::new();
    fixture.put(
        "CHANGELOG.md",
        "# Changelog\n\n[中文](CHANGELOG.zh-CN.md)\n[guide](docs/USER_GUIDE.md)\n".as_bytes(),
    );
    fixture.put(
        "CHANGELOG.zh-CN.md",
        "# 更新日志\n\n[English](CHANGELOG.md)\n[指南](docs/USER_GUIDE.zh-CN.md)\n".as_bytes(),
    );
    assert!(fixture.audit().findings.is_empty());
    fixture.put(
        "CHANGELOG.zh-CN.md",
        "# 更新日志\n\n[English](CHANGELOG.md)\n[wrong](docs/USER_GUIDE.md)\n".as_bytes(),
    );
    assert_eq!(
        fixture.audit().findings,
        findings(&[("CHANGELOG.zh-CN.md", 4, "markdown-language-navigation")])
    );
}

#[test]
fn changelog_contents_still_reject_private_paths() {
    let fixture = Fixture::new();
    let root = "home";
    let content = format!(
        "# Changelog\n\n[中文](CHANGELOG.zh-CN.md)\n\n- Evidence: /{root}/sample-person/private/release.txt\n"
    );
    fixture.put("CHANGELOG.md", content.as_bytes());
    assert_eq!(
        fixture.audit().findings,
        findings(&[("CHANGELOG.md", 5, "private-absolute-path")])
    );
}

#[test]
fn ignored_planning_pair_is_not_required_or_opened() {
    let fixture = Fixture::new();
    fixture.put(".gitignore", b"PLAN.md\nPLAN.zh-CN.md\n");
    assert!(fixture.audit().findings.is_empty());
    let english = fixture.put("PLAN.md", b"ignored local planning material\n");
    let chinese = fixture.put("PLAN.zh-CN.md", b"ignored local planning material\n");
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[english, chinese]);
    assert!(report.findings.is_empty());
    assert!(!report.inventory.candidates().contains("PLAN.md"));
    assert!(!report.inventory.candidates().contains("PLAN.zh-CN.md"));
}

#[test]
fn root_override_cannot_select_parent_or_another_repository() {
    let fixture = Fixture::new();
    assert_eq!(
        audit_at(&fixture.root, fixture.root.parent()).err(),
        Some("root-does-not-match-application")
    );
    fixture.put(
        "Cargo.toml",
        b"[package]\nname = \"different-application\"\n",
    );
    assert_eq!(
        audit_at(&fixture.root, None).err(),
        Some("invalid-application-sentinel")
    );
}

#[test]
fn application_sentinel_requires_the_relocated_package() {
    let fixture = Fixture::new();
    fixture.put(
        "crates/app/Cargo.toml",
        b"[package]\nname = \"different-application\"\n",
    );
    assert_eq!(
        audit_at(&fixture.root, None).err(),
        Some("invalid-application-sentinel")
    );
}

#[test]
fn enclosing_parent_repository_is_not_an_application_repository() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.root.join(".git")).expect("remove fixture git");
    fixture.git_at(
        fixture.root.parent().expect("fixture parent"),
        &["-c", "init.templateDir=", "init", "--quiet"],
    );
    assert_eq!(audit_at(&fixture.root, None).err(), Some("missing-path"));
}

#[test]
fn candidate_and_markdown_symlinks_are_never_followed() {
    let fixture = Fixture::new();
    let outside = fixture
        .root
        .parent()
        .expect("fixture parent")
        .join("private");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(outside.join("private.md"), b"not a publication input\n")
        .expect("write outside fixture");
    if !symlink(&outside, &fixture.root.join("linked"), true)
        || !symlink(
            &outside.join("private.md"),
            &fixture.root.join("linked.md"),
            false,
        )
    {
        return;
    }
    fixture.put("notes.md", b"[direct](linked.md#private)\n[ancestor](linked/private.md)\n[cancelled](linked/../README.md)\n");
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[
        fixture.root.join("linked.md"),
        fixture.root.join("linked/private.md"),
    ]);
    assert_eq!(
        report.findings,
        findings(&[
            ("linked", 0, "symlink-or-reparse-point"),
            ("linked.md", 0, "symlink-or-reparse-point"),
            ("notes.md", 1, "symlink-or-reparse-point"),
            ("notes.md", 2, "symlink-or-reparse-point"),
            ("notes.md", 3, "symlink-or-reparse-point")
        ])
    );
}

#[test]
fn root_symlink_or_reparse_attribute_is_rejected() {
    let fixture = Fixture::new();
    inject_reparse(&fixture.root);
    assert_eq!(
        audit_at(&fixture.root, None).err(),
        Some("symlink-or-reparse-point")
    );
}

#[test]
fn linked_worktree_indirection_uses_the_same_application_root_guard() {
    let fixture = Fixture::new();
    fixture.git(&["add", "--all"]);
    fixture.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "Fixture",
    ]);
    let linked = fixture
        .root
        .parent()
        .expect("fixture parent")
        .join("linked-worktree");
    fixture.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().expect("fixture path is utf8"),
    ]);
    let report = audit_at(&linked, Some(&linked)).expect("linked worktree audit");
    assert!(!report.inventory.tracked.is_empty());
    assert!(report.inventory.untracked.is_empty());
    assert!(report.inventory.ignored_tracked.is_empty());
    assert_eq!(
        report.inventory.candidates().len(),
        report.inventory.tracked.len()
    );
    assert_eq!(report.scanned_text, report.inventory.candidates().len());
    assert!(report.findings.is_empty());
}

#[test]
fn symlinked_application_ancestor_is_rejected() {
    let fixture = Fixture::new();
    let alias = fixture
        .root
        .parent()
        .expect("fixture parent")
        .join("application-alias");
    if !symlink(&fixture.root, &alias, true) {
        return;
    }
    assert_eq!(
        audit_at(&alias, None).err(),
        Some("symlink-or-reparse-point")
    );
}

#[test]
fn candidate_windows_reparse_attribute_prevents_content_reads() {
    let fixture = Fixture::new();
    let target = fixture.put("public.txt", b"must not be opened\n");
    reset_trace();
    inject_reparse(&target);
    let report = fixture.audit();
    assert_not_opened(&[target]);
    assert_eq!(
        report.findings,
        findings(&[("public.txt", 0, "symlink-or-reparse-point")])
    );
}

#[test]
fn oversized_files_are_rejected_before_opening() {
    let fixture = Fixture::new();
    let path = fixture.root.join("large.txt");
    let file = fs::File::create(&path).expect("create oversized fixture");
    file.set_len(MAX_TEXT_BYTES + 1)
        .expect("size oversized fixture");
    drop(file);
    reset_trace();
    let report = fixture.audit();
    assert_not_opened(&[path]);
    assert_eq!(
        report.findings,
        findings(&[("large.txt", 0, "unreviewed-oversized-file")])
    );
}

#[test]
fn markdown_html_entities_and_percent_encoded_anchors_are_reviewed() {
    let fixture = Fixture::new();
    fixture.put("notes.md", b"[entity](docs/a&amp;b.md#one%26two)\n");
    fixture.put("docs/a&b.md", b"# Target\n<a id=\"one&amp;two\"></a>\n");
    assert!(fixture.audit().findings.is_empty());
    fixture.put("notes.md", b"[invalid](docs/a%ffb.md)\n");
    assert_eq!(
        fixture.audit().findings,
        findings(&[("notes.md", 1, "markdown-malformed-target")])
    );
    assert_eq!(
        html_unescape("&amp;&copy;&#x4e2d;&#25991; &not-an-entity;"),
        "&©中文 ¬-an-entity;"
    );
}

#[test]
fn unsafe_and_reserved_candidate_paths_are_refused() {
    for path in [
        "",
        "../private",
        "/absolute",
        "dir/../file",
        "dir//file",
        "dir/file.",
        "dir/file ",
        "CON",
        "dir/nul.txt",
        "COM9.log",
        "dir/a:b",
        "dir\\file",
    ] {
        assert!(!valid_relative(path), "unexpectedly accepted {path:?}");
    }
    assert!(valid_relative("docs/中文 file.md"));
}

#[test]
fn cli_arguments_and_ascii_json_preserve_the_output_contract() {
    assert_eq!(parse_arguments(&[]).expect("empty arguments"), None);
    assert_eq!(
        parse_arguments(&[OsString::from("--root"), OsString::from("relative")])
            .expect("root flag"),
        Some(PathBuf::from("relative"))
    );
    assert_eq!(
        parse_arguments(&[OsString::from("--root=relative")]).expect("root equals flag"),
        Some(PathBuf::from("relative"))
    );
    assert_eq!(
        parse_arguments(&[OsString::from("--root")]).err(),
        Some("invalid-arguments")
    );
    assert_eq!(
        parse_arguments(&[OsString::from("--unexpected")]).err(),
        Some("invalid-arguments")
    );
    let path = format!(
        "中文{}.md",
        char::from_u32(0x20000).expect("valid supplementary character")
    );
    let encoded = ascii_json(&json!({ "path": path, "line": 0, "category": "missing-path" }));
    assert!(encoded.is_ascii());
    let escaped = ["\\u4e2d", "\\u6587", "\\ud840", "\\udc00"].concat();
    assert!(encoded.contains(&escaped));
    let decoded: serde_json::Value = serde_json::from_str(&encoded).expect("valid ASCII JSON");
    assert_eq!(decoded["path"], path);
}

#[test]
fn tool_test_directory_does_not_exempt_arbitrary_private_paths() {
    let drive = "C:";
    let slash = "/";
    let value = format!("{drive}{slash}{slash}unreviewed/private");
    for path in [
        "crates/tools/src/public/tests.rs",
        "crates/tools/src/notices/tests.rs",
    ] {
        assert_eq!(
            scan_text(path, &value),
            findings(&[(path, 1, "private-absolute-path")])
        );
    }
}

#[test]
fn reviewed_tool_test_credentials_do_not_exempt_other_values_or_files() {
    let path = "crates/tools/src/public/tests.rs";
    for (key, value) in [
        ("token", "token"),
        ("github_token", "GH_TOKEN"),
        ("secret", "fixture-sensitive-value"),
    ] {
        let approved = format!("{key} = \"{value}\"");
        let unreviewed = format!("{key} = \"unreviewed-value\"");
        assert!(scan_text(path, &approved).is_empty());
        assert_eq!(
            scan_text("other.rs", &approved),
            findings(&[("other.rs", 1, "credential-assignment")])
        );
        assert_eq!(
            scan_text(path, &unreviewed),
            findings(&[(path, 1, "credential-assignment")])
        );
        assert_eq!(
            scan_text(path, &format!("{approved}; {unreviewed}")),
            findings(&[(path, 1, "credential-assignment")])
        );
    }
}

#[cfg(unix)]
#[test]
fn fifo_candidates_are_refused_without_a_blocking_open() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new();
    let path = fixture.root.join("pipe.txt");
    let name = CString::new(path.as_os_str().as_bytes()).expect("fixture path contains no NUL");
    // SAFETY: name is a valid C string and mkfifo does not retain its pointer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    reset_trace();
    assert_eq!(
        read_public_bytes(&fixture.root, "pipe.txt").err(),
        Some("non-regular-candidate")
    );
    assert_not_opened(&[path]);
}

#[test]
fn empty_network_authority_does_not_hide_an_absolute_markdown_target() {
    let fixture = Fixture::new();
    assert_eq!(
        local_target(&fixture.root, "notes.md", "///private.md").err(),
        Some("markdown-unsafe-target")
    );
    assert_eq!(
        local_target(&fixture.root, "notes.md", "//").expect("empty authority"),
        Some(("notes.md".to_owned(), String::new()))
    );
    let authority_prefix = "//";
    let network = format!("{authority_prefix}example.invalid/path");
    assert_eq!(
        local_target(&fixture.root, "notes.md", &network).expect("external authority"),
        None
    );
}

#[test]
fn non_markdown_file_uris_report_embedded_unix_and_windows_private_paths() {
    let fixture = Fixture::new();
    let file_scheme = "file:";
    let home = "home";
    let drive = "C:";
    let users = "Users";
    let unix = format!("{file_scheme}///{home}/fixture-person/private/data");
    let windows = format!("{file_scheme}///{drive}/{users}/fixture-person/private/data");
    fixture.put("public.txt", format!("{unix}\n{windows}\n").as_bytes());

    let report = fixture.audit();
    assert_eq!(
        report.findings,
        findings(&[
            ("public.txt", 1, "private-absolute-path"),
            ("public.txt", 2, "private-absolute-path"),
        ])
    );
    let output = render_report(&report).join("\n");
    assert!(!output.contains("fixture-person"));
    assert!(!output.contains("private/data"));
}

#[test]
fn malformed_authority_in_plain_text_and_markdown_reports_redacted_json_and_continues() {
    let fixture = Fixture::new();
    let scheme = "https://";
    let token = "token";
    let bad = "]::1[";
    let secret = "fixture-sensitive-value";
    let plain = format!("{scheme}{bad}\n{token} = \"{secret}\"\n");
    let markdown = format!("[bad]({scheme}{bad})\n\n## Later\n[valid](#later)\n");
    fixture.put("public.txt", plain.as_bytes());
    fixture.put("notes.md", markdown.as_bytes());

    let report = fixture.audit();
    assert_eq!(
        report.findings,
        findings(&[
            ("notes.md", 1, "malformed-url"),
            ("notes.md", 1, "markdown-malformed-target"),
            ("public.txt", 1, "malformed-url"),
            ("public.txt", 2, "credential-assignment"),
        ])
    );
    let output = render_report(&report);
    let rendered = output.join("\n");
    assert!(!rendered.contains(bad));
    assert!(!rendered.contains(secret));
    let inventory: serde_json::Value =
        serde_json::from_str(output.last().expect("inventory JSON")).expect("valid JSON inventory");
    assert_eq!(inventory["inventory"]["findings"], 4);
}
