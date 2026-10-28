//! Non-mutating review of Git publication candidates.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
const MAINTAINED: [&str; 6] = [
    "README.md",
    "CHANGELOG.md",
    "docs/USER_GUIDE.md",
    "docs/DEVELOPMENT.md",
    "docs/ARCHITECTURE.md",
    "docs/PROTOCOLS.md",
];
const FORBIDDEN_PARTS: &[&str] = &[
    ".git",
    ".local",
    "target",
    "build",
    "dist",
    "captures",
    "recordings",
    "firmwares",
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".ruff_cache",
    ".claude",
    ".codex",
    ".omp",
    ".idea",
    "credentials",
    "secrets",
];
const FORBIDDEN_SUFFIXES: &[&str] = &[
    ".hidlog",
    ".slimecap",
    ".log",
    ".tmp",
    ".temp",
    ".key",
    ".pem",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".pyc",
    ".pyo",
    ".swp",
    ".swo",
];
const BINARY_SUFFIXES: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".webp", ".bmp", ".avif", ".exe", ".dll", ".so",
    ".dylib", ".a", ".lib", ".o", ".obj", ".bin", ".uf2", ".zip", ".gz", ".xz", ".bz2", ".7z",
    ".rar", ".tar", ".pdf", ".ttf", ".otf", ".woff", ".woff2", ".wav", ".mp3", ".mp4", ".sqlite",
    ".db",
];
const PUBLIC_LOG: &str = "crates/app/src/rssi_scan/fixtures/receiver_scan.log";
type BoundaryResult<T> = Result<T, &'static str>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    path: String,
    line: usize,
    category: &'static str,
}

#[derive(Default)]
struct Inventory {
    tracked: Vec<String>,
    untracked: Vec<String>,
    ignored_tracked: Vec<String>,
}

impl Inventory {
    fn candidates(&self) -> BTreeSet<&str> {
        self.tracked
            .iter()
            .chain(&self.untracked)
            .map(String::as_str)
            .collect()
    }
}

struct Report {
    inventory: Inventory,
    findings: BTreeSet<Finding>,
    scanned_text: usize,
    unreviewed_or_forbidden: usize,
}

#[derive(Default)]
struct Document {
    links: Vec<(usize, String)>,
    anchors: BTreeSet<String>,
    explicit_anchors: BTreeSet<String>,
}

fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            if part.is_empty()
                || part == "."
                || part == ".."
                || part.contains(['\\', ':', '\0'])
                || part.ends_with([' ', '.'])
            {
                return false;
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            !matches!(
                stem.as_str(),
                "con"
                    | "prn"
                    | "aux"
                    | "nul"
                    | "com1"
                    | "com2"
                    | "com3"
                    | "com4"
                    | "com5"
                    | "com6"
                    | "com7"
                    | "com8"
                    | "com9"
                    | "lpt1"
                    | "lpt2"
                    | "lpt3"
                    | "lpt4"
                    | "lpt5"
                    | "lpt6"
                    | "lpt7"
                    | "lpt8"
                    | "lpt9"
            )
        })
}

fn suffix(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or_default();
    match name.rfind('.') {
        Some(index) if index > 0 && index < name.len() - 1 => &name[index..],
        _ => "",
    }
}

fn forbidden(path: &str) -> bool {
    let parts: Vec<String> = path.split('/').map(str::to_lowercase).collect();
    let Some(name) = parts.last() else {
        return false;
    };
    parts
        .iter()
        .any(|part| FORBIDDEN_PARTS.contains(&part.as_str()))
        || (parts.iter().any(|part| part == ".vscode")
            && !matches!(
                path,
                ".vscode" | ".vscode/settings.json" | ".vscode/extensions.json"
            ))
        || (name.starts_with(".env") && name != ".env.example")
        || matches!(
            name.as_str(),
            "slimenrf-helper.local.yaml"
                | "slimenrf-helper.local.yml"
                | "preferences.json"
                | "device-registry.json"
                | "remembered-devices.json"
                | "custom-shortcuts.json"
                | "credentials"
                | "credentials.toml"
                | "credentials.json"
                | "credentials.yml"
                | "credentials.yaml"
                | ".git-credentials"
                | "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | "thumbs.db"
                | ".ds_store"
        )
        || name.ends_with(".local.yaml")
        || name.ends_with(".local.yml")
        || (FORBIDDEN_SUFFIXES.contains(&suffix(name)) && path != PUBLIC_LOG)
}

fn is_reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn checked_stat(path: &Path) -> BoundaryResult<Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            "missing-path"
        } else {
            "unreadable-metadata"
        }
    })?;
    #[cfg(test)]
    if tests::injected_reparse(path) {
        return Err("symlink-or-reparse-point");
    }
    if is_reparse(&metadata) {
        return Err("symlink-or-reparse-point");
    }
    Ok(metadata)
}

fn check_absolute(path: &Path) -> BoundaryResult<()> {
    for component in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = checked_stat(component)?;
        if component != path && !metadata.is_dir() {
            return Err("non-directory-component");
        }
    }
    Ok(())
}

fn candidate_stat(root: &Path, relative: &str) -> BoundaryResult<Metadata> {
    if !valid_relative(relative) {
        return Err("invalid-candidate-path");
    }
    if forbidden(relative) {
        return Err("forbidden-publication-path");
    }
    let mut current = root.to_owned();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        let metadata = checked_stat(&current)?;
        if parts.peek().is_some() && !metadata.is_dir() {
            return Err("non-directory-component");
        }
        if parts.peek().is_none() {
            return Ok(metadata);
        }
    }
    Err("invalid-candidate-path")
}

#[cfg(unix)]
fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.size() == b.size()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
}

#[cfg(windows)]
fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    a.file_size() == b.file_size()
        && a.creation_time() == b.creation_time()
        && a.last_write_time() == b.last_write_time()
}

#[cfg(not(any(unix, windows)))]
fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Opening the reparse point itself lets the metadata check reject a replacement.
        options.custom_flags(0x0020_0000);
    }
    #[cfg(test)]
    tests::record_open(path);
    options.open(path)
}

#[cfg(windows)]
fn same_opened_identity(a: &File, b: &File) -> io::Result<bool> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct HandleInformation {
        attributes: u32,
        creation: FileTime,
        access: FileTime,
        modified: FileTime,
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut c_void, info: *mut HandleInformation) -> i32;
    }
    fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
        let mut info = std::mem::MaybeUninit::<HandleInformation>::uninit();
        // SAFETY: the file owns a valid handle and the API initializes info on success.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the preceding API call reported that the structure was initialized.
        let info = unsafe { info.assume_init() };
        Ok((info.volume, info.index_high, info.index_low))
    }
    Ok(identity(a)? == identity(b)?)
}

fn read_public_bytes(root: &Path, relative: &str) -> BoundaryResult<Vec<u8>> {
    let before = candidate_stat(root, relative)?;
    if !before.is_file() {
        return Err("non-regular-candidate");
    }
    if BINARY_SUFFIXES.contains(&suffix(&relative.to_lowercase())) {
        return Err("unreviewed-binary");
    }
    if before.len() > MAX_TEXT_BYTES {
        return Err("unreviewed-oversized-file");
    }
    let path = root.join(relative);
    let mut file = open_no_follow(&path).map_err(|_| "unreadable-candidate")?;
    let opened = file.metadata().map_err(|_| "unreadable-candidate")?;
    let after = candidate_stat(root, relative)?;
    if !opened.is_file()
        || is_reparse(&opened)
        || !same_metadata(&before, &opened)
        || !same_metadata(&before, &after)
    {
        return Err("candidate-changed-during-read");
    }
    #[cfg(windows)]
    {
        let second = open_no_follow(&path).map_err(|_| "unreadable-candidate")?;
        if !same_opened_identity(&file, &second).map_err(|_| "unreadable-candidate")? {
            return Err("candidate-changed-during-read");
        }
    }
    let mut data = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    (&mut file)
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| "unreadable-candidate")?;
    let finished = file.metadata().map_err(|_| "unreadable-candidate")?;
    if !same_metadata(&opened, &finished) {
        return Err("candidate-changed-during-read");
    }
    if data.len() as u64 > MAX_TEXT_BYTES {
        return Err("unreviewed-oversized-file");
    }
    Ok(data)
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn git(root: &Path, arguments: &[&str]) -> BoundaryResult<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(arguments);
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
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device());
    let output = command.output().map_err(|_| "git-unavailable")?;
    if !output.status.success() {
        return Err("git-command-failed");
    }
    Ok(output.stdout)
}

fn lexical_absolute(path: &Path) -> BoundaryResult<PathBuf> {
    let absolute = std::path::absolute(path).map_err(|_| "invalid-arguments")?;
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(part.as_os_str()),
        }
    }
    Ok(result)
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn application_root(root: &Path, requested: Option<&Path>) -> BoundaryResult<PathBuf> {
    let root = lexical_absolute(root)?;
    if let Some(requested) = requested
        && !paths_equal(&lexical_absolute(requested)?, &root)
    {
        return Err("root-does-not-match-application");
    }
    check_absolute(&root)?;
    let metadata = checked_stat(&root.join(".git"))?;
    if !metadata.is_dir() && !metadata.is_file() {
        return Err("invalid-git-metadata");
    }
    let git_root = git(&root, &["rev-parse", "--show-toplevel"])?;
    let git_root = std::str::from_utf8(&git_root)
        .map_err(|_| "git-root-does-not-match-application")?
        .trim_end_matches(['\r', '\n']);
    let git_root = lexical_absolute(Path::new(git_root))?;
    // Compare resolved names only after the reparse checks; keep the lexical root for reads.
    let resolved_root =
        fs::canonicalize(&root).map_err(|_| "git-root-does-not-match-application")?;
    let resolved_git_root =
        fs::canonicalize(&git_root).map_err(|_| "git-root-does-not-match-application")?;
    if !paths_equal(&resolved_git_root, &resolved_root) {
        return Err("git-root-does-not-match-application");
    }
    let workspace = read_public_bytes(&root, "Cargo.toml")?;
    let workspace = std::str::from_utf8(&workspace)
        .map_err(|_| "invalid-application-sentinel")?
        .trim_start_matches('\u{feff}');
    if !REGEX.workspace.is_match(workspace) {
        return Err("invalid-application-sentinel");
    }
    for (path, package) in [
        ("crates/app/Cargo.toml", "slimenrf-helper"),
        ("crates/recording/Cargo.toml", "slimenrf-recording"),
    ] {
        let source = read_public_bytes(&root, path)?;
        let source = std::str::from_utf8(&source)
            .map_err(|_| "invalid-application-sentinel")?
            .trim_start_matches('\u{feff}');
        let Some(section) = REGEX.package_section.captures(source) else {
            return Err("invalid-application-sentinel");
        };
        if !REGEX
            .package_name
            .captures_iter(&section[1])
            .any(|name| &name[1] == package)
        {
            return Err("invalid-application-sentinel");
        }
    }
    if !candidate_stat(&root, "crates/app/src/main.rs")?.is_file() {
        return Err("invalid-application-sentinel");
    }
    Ok(root)
}

fn inventory(root: &Path) -> BoundaryResult<Inventory> {
    let mut groups = Vec::new();
    for args in [
        &["ls-files", "--cached", "-z", "--"][..],
        &["ls-files", "--others", "--exclude-standard", "-z", "--"][..],
        &[
            "ls-files",
            "--cached",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
        ][..],
    ] {
        let raw = git(root, args)?;
        if !raw.is_empty() && raw.last() != Some(&0) {
            return Err("invalid-git-inventory");
        }
        let mut paths = BTreeSet::new();
        for value in raw
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
        {
            let path = std::str::from_utf8(value).map_err(|_| "invalid-git-inventory")?;
            if !valid_relative(path) {
                return Err("invalid-git-inventory");
            }
            paths.insert(path.to_owned());
        }
        groups.push(paths.into_iter().collect());
    }
    let mut groups = groups.into_iter();
    Ok(Inventory {
        tracked: groups.next().unwrap_or_default(),
        untracked: groups.next().unwrap_or_default(),
        ignored_tracked: groups.next().unwrap_or_default(),
    })
}

struct Patterns {
    workspace: Regex,
    package_section: Regex,
    package_name: Regex,
    private_path: Regex,
    secrets: Vec<(&'static str, Regex)>,
    url: Regex,
    signed: Regex,
    fence: Regex,
    explicit: Regex,
    heading: Regex,
    underline: Regex,
    heading_link: Regex,
    html_tag: Regex,
    definition: Regex,
    definition_prefix: Regex,
    inline_link: Regex,
    html_link: Regex,
    reference: Regex,
    slug_character: Regex,
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static public-check pattern is valid")
}

static REGEX: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    workspace: regex(r"(?m)^\[workspace\][ \t\r]*$"),
    package_section: regex(r"(?m)^\[package\][ \t]*\r?\n((?:[^\[\n][^\n]*(?:\n|\z)|\n)*)"),
    package_name: regex(r#"(?m)^name[ \t]*=[ \t]*"([^"]+)"[ \t\r]*$"#),
    private_path: regex(
        r#"(?i)(?:[A-Za-z]:[\\/]+[^\s"'<>),;]*|/(?:home|users|root|workspaces?|projects)/[^\s"'<>),;]+|/mnt/[a-z]/[^\s"'<>),;]+|//[\w.-]+/[^\s"'<>),;]+|\\\\[\w.-]+\\[^\s"'<>),;]+)"#,
    ),
    secrets: vec![
        (
            "private-key",
            regex(r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----"),
        ),
        (
            "credential-token",
            regex(
                r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_-]{20,}|AKIA[A-Z0-9]{16}|xox[baprs]-[A-Za-z0-9-]{15,}|sk-(?:proj-)?[A-Za-z0-9_-]{20,})\b",
            ),
        ),
        (
            "authorization-value",
            regex(
                r"(?i)\bBearer\s+[A-Za-z0-9][A-Za-z0-9._~+/-]*={0,2}|\bAuthorization:\s*Basic\s+[A-Za-z0-9+/]+={0,2}",
            ),
        ),
        (
            "credential-assignment",
            regex(
                r#"(?i)\b(?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret[_-]?key|token|secret|GH_TOKEN|GITHUB_TOKEN)\b["']?\s*[:=]\s*["'][^"'\r\n]+["']"#,
            ),
        ),
        (
            "credential-assignment",
            regex(
                r"(?i)^\s*(?:export\s+)?(?:GH_TOKEN|GITHUB_TOKEN|API_KEY|ACCESS_TOKEN|CLIENT_SECRET|PASSWORD)\s*=\s*[A-Za-z0-9_+/-]{8,}",
            ),
        ),
    ],
    url: regex(r#"(?i)\b(?:https?|ftp|ssh)://[^\s"'<>\\)]+"#),
    signed: regex(
        r"(?i)(?:[?&]|\b)(?:sig|signature|access_token|token|api_key|password|client_secret|X-Amz-Signature|X-Goog-Signature)=[A-Za-z0-9%_+./~-]+",
    ),
    fence: regex(r"^ {0,3}(`{3,}|~{3,})"),
    explicit: regex(r#"(?i)<[^>]+\b(?:id|name)\s*=\s*["']([^"']+)["']"#),
    heading: regex(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$"),
    underline: regex(r"^ {0,3}(?:=+|-+)\s*$"),
    heading_link: regex(r"!?\[([^]]*)\]\([^)]*\)"),
    html_tag: regex(r"<[^>]*>"),
    definition: regex(r"^ {0,3}\[([^]]+)\]:\s*(<[^>]*>|\S+)"),
    definition_prefix: regex(r"^ {0,3}\[[^]]+\]:"),
    inline_link: regex(r"\]\(\s*(<[^>]*>|(?:[^\s()\\]|\\.|\([^()]*\))*)"),
    html_link: regex(r#"(?i)<[^>]+\b(?:href|src)\s*=\s*["']([^"']+)["']"#),
    reference: regex(r"\[([^]]+)\](?:\[([^]]*)\])?"),
    slug_character: regex(r"^[\p{L}\p{N}]$"),
});

// Values are exact synthetic fixtures, never whole-file exemptions.
const REVIEWED_VALUES: &[(&str, &str, &str)] = &[
    (
        "crates/app/src/firmware_source/http_tests.rs",
        "authorization-value",
        concat!("bearer ", "fixture-secret"),
    ),
    (
        "crates/app/src/app_update/http_tests.rs",
        "authenticated-url",
        concat!(
            "https://",
            "user:fixture-secret@api.github.com/private-fixture-path"
        ),
    ),
    (
        "crates/app/src/app_update/http_tests.rs",
        "signed-credential",
        concat!("?sig", "=FiXture%2Fpart"),
    ),
    (
        "crates/app/src/app_update/http_tests.rs",
        "signed-credential",
        concat!("?sig", "=fixture-secret"),
    ),
    (
        "crates/app/src/app_update/http_tests.rs",
        "signed-credential",
        concat!("?sig", "=fixture-secret-signature"),
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "authorization-value",
        concat!("Bearer ", "ghp_private_token"),
    ),
    (
        "crates/app/src/firmware_source/http_tests.rs",
        "authenticated-url",
        concat!("https://", "user:password@api.github.com/credentials"),
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "authenticated-url",
        concat!(
            "https://",
            "user:password@nightly.link/private",
            "?sig",
            "=private-signature"
        ),
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "authenticated-url",
        concat!("https://", "user:secret@api.github.com/a"),
    ),
    (
        "crates/app/src/firmware_source/http_tests.rs",
        "signed-credential",
        concat!("?sig", "=fixture-signature"),
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "signed-credential",
        concat!("?sig", "=private-signature"),
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "signed-credential",
        concat!("sig", "=detached-signature"),
    ),
    (
        "crates/tools/src/public/tests.rs",
        "credential-assignment",
        concat!("token", " = ", "\"", "token", "\""),
    ),
    (
        "crates/tools/src/public/tests.rs",
        "credential-assignment",
        concat!("github_token", " = ", "\"", "GH_TOKEN", "\""),
    ),
    (
        "crates/tools/src/public/tests.rs",
        "credential-assignment",
        concat!("secret", " = ", "\"", "fixture-sensitive-value", "\""),
    ),
];

// Full trimmed-line SHA-256 values prevent an approved path from hiding a suffix.
const REVIEWED_LINES: &[(&str, &str, &str)] = &[
    (
        "crates/app/src/app_update/windows/tests.rs",
        "private-absolute-path",
        "36250d10780c563db98c1482baa76b505ac72e45dbbdd71c9d49c3584cde910c",
    ),
    (
        "crates/app/src/app_update/windows/tests.rs",
        "private-absolute-path",
        "2b45a5b35111e442b1db2fedcf72718a3948c3827cf339b6b7a3aa3b68dd817e",
    ),
    (
        "crates/app/src/bundled_resources/tests.rs",
        "private-absolute-path",
        "ddf69c2c4e85fcd02698314d3fc0fe7f142d9cccb30793083674970efa693e39",
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "private-absolute-path",
        "2c8c73153f7a4a4f442695e8d039ba3db64a8ca0ee68b4946714ca359366acef",
    ),
    (
        "crates/app/src/firmware_source/tests.rs",
        "private-absolute-path",
        "5a59ce6bfe62671241ec414aa91729988dc2c38e3deaa90c8bbd9dbbac91fcab",
    ),
    (
        "crates/app/src/uf2/linux.rs",
        "private-absolute-path",
        "d1320baad7a8a5d72539d347aa0c66a2f82e18916fe0935e85347217d3872b07",
    ),
    (
        "crates/app/src/uf2/linux.rs",
        "private-absolute-path",
        "66e66d60df53e41798e5f223537f737d7e3ccc4378ea45a19ba18a108bbacb3d",
    ),
    (
        "crates/app/src/uf2/linux.rs",
        "private-absolute-path",
        "68b8c9de1d9714feda4e7191d4963d5c366f92b79723e6a6b621f8f9427b8cce",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "5a316d00bdb4939ead3667c550edfe23ea47b6a3f42380a1842719ca7dea8227",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "6f072659367d7cedf4c746c972e1dc0cf05acbaa6e612c1ba32cfff1ab0fc6d6",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "290386f05d64744dc7c21888da104676f03ed56cc38b0e0111f4775b66bb9651",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "e6cfcb1a9a1e65f6c26b52447714f9d93e24c4c372f6741d8ab2575b396518b1",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "2c7f400ef1511ab9ee560371c101c2c584b6833a3d6f58bf55d0b330a205ed26",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "3f4dc059b78e46a964a260700d367ee42051ee52daea8b471596436b8c49ef23",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "a21097572eae3a8d2e93d3d2a1fe81de0ea8ae2aad6d8eea50e697924b1f5dec",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "85ffe7693debced9382c7f3512212b34fc3b35465db843d8d5c5b45f6e97bee9",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "c705c6885bc0b74bb96080749820534dabcae575d760f56623728559645295cd",
    ),
    (
        "crates/app/src/ui/flash/tests/images.rs",
        "private-absolute-path",
        "c16b4d348f5ce69d1e0c567ac1449062547c13b02e4c4e5aaef98963be627656",
    ),
    (
        "crates/tools/src/public/tests.rs",
        "private-absolute-path",
        "e051b12ebb57e184f288f4f831b3756e1369fab0344ea2242534419d070046aa",
    ),
    (
        "crates/tools/src/public/tests.rs",
        "private-absolute-path",
        "7fee43ceedbc98dce08a72640cf9165838acf5e46669ae66cf88c78cbb0bd3e6",
    ),
    (
        "crates/tools/src/public/tests.rs",
        "private-absolute-path",
        "c012100c1951c6dec358765aa19aa9c593cae3e101583f434af52e710cc8a342",
    ),
    (
        ".github/workflows/ci.yml",
        "private-absolute-path",
        "f0b25a20b3b15d855b6175d6cf904aac0e87838e9b83db6935cef53af088680d",
    ),
    (
        "locales/en.yml",
        "credential-assignment",
        "7df7e1c2fb26008af0010be924ca39e9f106e781fbb69283ac51bfb5975314be",
    ),
    (
        "locales/zh-CN.yml",
        "credential-assignment",
        "290aa464dc17edd4afb1ece58c910c83899a4294afea89297de35ac709354218",
    ),
];

fn reviewed(
    path: &str,
    category: &str,
    value: &str,
    line: &str,
    line_hash: &mut Option<String>,
) -> bool {
    if REVIEWED_VALUES.contains(&(path, category, value)) {
        return true;
    }
    if !REVIEWED_LINES
        .iter()
        .any(|item| item.0 == path && item.1 == category)
    {
        return false;
    }
    let hash = line_hash
        .get_or_insert_with(|| crate::lower_hex(Sha256::digest(line.trim().as_bytes()).as_ref()));
    REVIEWED_LINES
        .iter()
        .any(|item| item.0 == path && item.1 == category && item.2 == hash.as_str())
}

fn url_authority(url: &str) -> BoundaryResult<&str> {
    let authority = url
        .split_once("://")
        .map_or("", |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.contains('[') != host.contains(']') {
        return Err("malformed-url");
    }
    if let Some(start) = host.find('[') {
        let end = host.find(']').ok_or("malformed-url")?;
        let literal = host.get(start + 1..end).ok_or("malformed-url")?;
        if literal.parse::<std::net::Ipv6Addr>().is_err()
            && !(literal.starts_with('v') && literal.contains('.'))
        {
            return Err("malformed-url");
        }
    }
    Ok(authority)
}

fn scan_text(path: &str, text: &str) -> BTreeSet<Finding> {
    let mut findings = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let mut hash = None;
        let mut add = |category: &'static str, value: &str| {
            if !reviewed(path, category, value, line, &mut hash) {
                findings.insert(Finding {
                    path: path.to_owned(),
                    line: index + 1,
                    category,
                });
            }
        };
        let mut position = 0;
        while let Some(found) = REGEX.private_path.find_at(line, position) {
            let previous = line[..found.start()].chars().next_back();
            let rejected = previous.is_some_and(|c| c.is_alphanumeric() || c == '_')
                || (found.as_str().starts_with("//")
                    && previous.is_some_and(|c| c == ':' || c == '/'))
                || found.as_str().starts_with("\\\\.\\");
            if rejected {
                // All alternatives begin with ASCII; retry after the rejected start
                // so an embedded path can still satisfy its own boundary.
                position = found.start() + 1;
                continue;
            }
            add("private-absolute-path", found.as_str());
            position = found.end();
        }
        for (category, pattern) in &REGEX.secrets {
            for found in pattern.find_iter(line) {
                add(category, found.as_str());
            }
        }
        for found in REGEX.signed.find_iter(line) {
            add("signed-credential", found.as_str());
        }
        for found in REGEX.url.find_iter(line) {
            match url_authority(found.as_str()) {
                Ok(authority) if authority.contains('@') => {
                    add("authenticated-url", found.as_str())
                }
                Err(_) => add("malformed-url", found.as_str()),
                _ => {}
            }
        }
    }
    findings
}

fn html_unescape(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        if let Some(numeric) = rest.strip_prefix('#') {
            let (radix, digits) = if let Some(hex) = numeric.strip_prefix(['x', 'X']) {
                (16, hex)
            } else {
                (10, numeric)
            };
            let count = digits
                .bytes()
                .take_while(|byte| {
                    if radix == 16 {
                        byte.is_ascii_hexdigit()
                    } else {
                        byte.is_ascii_digit()
                    }
                })
                .count();
            if count != 0 {
                let value = u32::from_str_radix(&digits[..count], radix).unwrap_or(u32::MAX);
                if let Some(decoded) = html_numeric(value) {
                    output.push(decoded);
                }
                let consumed = rest.len() - digits.len() + count;
                rest = &rest[consumed..];
                if let Some(after) = rest.strip_prefix(';') {
                    rest = after;
                }
                continue;
            }
        } else {
            let count = rest
                .bytes()
                .take(32)
                .take_while(u8::is_ascii_alphanumeric)
                .count();
            let total = if rest.as_bytes().get(count) == Some(&b';') {
                count + 1
            } else {
                count
            };
            let mut matched = false;
            for end in (1..=total).rev() {
                if let Some(&(first, second)) = markup5ever::data::NAMED_ENTITIES.get(&rest[..end])
                {
                    if first == 0 {
                        continue;
                    }
                    if let Some(c) = char::from_u32(first) {
                        output.push(c);
                    }
                    if let Some(c) = char::from_u32(second).filter(|c| *c != '\0') {
                        output.push(c);
                    }
                    rest = &rest[end..];
                    matched = true;
                    break;
                }
            }
            if matched {
                continue;
            }
        }
        output.push('&');
    }
    output.push_str(rest);
    output
}

fn html_numeric(value: u32) -> Option<char> {
    const WINDOWS_CONTROLS: [u32; 32] = [
        0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
        0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
        0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
    ];
    if (0x80..=0x9f).contains(&value) {
        return char::from_u32(WINDOWS_CONTROLS[(value - 0x80) as usize]);
    }
    let value = match value {
        0 | 0xd800..=0xdfff | 0x110000.. => 0xfffd,
        _ => value,
    };
    if matches!(value, 0x01..=0x08 | 0x0b | 0x0e..=0x1f | 0x7f..=0x9f | 0xfdd0..=0xfdef)
        || value & 0xffff >= 0xfffe
    {
        return None;
    }
    char::from_u32(value)
}

fn visible_markdown(text: &str) -> Vec<&str> {
    let mut fence: Option<&str> = None;
    text.lines()
        .map(|line| {
            if let Some(marker) = REGEX.fence.captures(line) {
                let token = marker.get(1).expect("fence capture").as_str();
                match fence {
                    None => fence = Some(token),
                    Some(open)
                        if token.as_bytes()[0] == open.as_bytes()[0]
                            && token.len() >= open.len() =>
                    {
                        fence = None
                    }
                    _ => {}
                }
                ""
            } else if fence.is_some() || line.starts_with("    ") || line.starts_with('\t') {
                ""
            } else {
                line
            }
        })
        .collect()
}

fn without_inline_code(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        output.push_str(&rest[..start]);
        let after = &rest[start..];
        let count = after.bytes().take_while(|byte| *byte == b'`').count();
        let token = &after[..count];
        if let Some(end) = after[count..].find(token) {
            rest = &after[count + end + count..];
        } else {
            output.push_str(token);
            rest = &after[count..];
        }
    }
    output.push_str(rest);
    output
}

fn markdown_document(text: &str) -> Document {
    let lines = visible_markdown(text);
    let mut document = Document::default();
    let mut definitions: BTreeMap<String, String> = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let visible = without_inline_code(line);
        for capture in REGEX.explicit.captures_iter(&visible) {
            document.explicit_anchors.insert(html_unescape(&capture[1]));
        }
        let heading = REGEX
            .heading
            .captures(line)
            .map(|capture| capture[1].to_owned())
            .or_else(|| {
                lines
                    .get(index + 1)
                    .filter(|next| REGEX.underline.is_match(next))
                    .filter(|_| !line.trim().is_empty())
                    .map(|_| line.trim().to_owned())
            });
        if let Some(title) = heading {
            let title = REGEX.heading_link.replace_all(&title, "$1");
            let title = REGEX.html_tag.replace_all(&title, "");
            let title = html_unescape(&title).to_lowercase();
            let mut slug = String::new();
            for character in title.chars() {
                if character == ' ' {
                    slug.push('-');
                } else if character == '-'
                    || character == '_'
                    || REGEX
                        .slug_character
                        .is_match(character.encode_utf8(&mut [0; 4]))
                {
                    slug.push(character);
                }
            }
            let mut anchor = slug.clone();
            let mut suffix = 0;
            while document.anchors.contains(&anchor) {
                suffix += 1;
                anchor = format!("{slug}-{suffix}");
            }
            document.anchors.insert(anchor);
        }
        if let Some(capture) = REGEX.definition.captures(&visible) {
            let destination = capture[2].trim_matches(['<', '>']).to_owned();
            definitions.insert(capture[1].to_lowercase(), destination.clone());
            document.links.push((number, destination));
            continue;
        }
        for capture in REGEX.inline_link.captures_iter(&visible) {
            let destination = capture[1].trim_matches(['<', '>']);
            let mut unescaped = String::with_capacity(destination.len());
            let mut chars = destination.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\'
                    && chars
                        .peek()
                        .is_some_and(|next| matches!(*next, '\\' | '(' | ')' | ' '))
                {
                    unescaped.push(chars.next().expect("peeked escaped character"));
                } else {
                    unescaped.push(c);
                }
            }
            document.links.push((number, unescaped));
        }
        for capture in REGEX.html_link.captures_iter(&visible) {
            document.links.push((number, html_unescape(&capture[1])));
        }
    }
    for (index, line) in lines.iter().enumerate() {
        let visible = without_inline_code(line);
        if REGEX.definition_prefix.is_match(&visible) {
            continue;
        }
        for capture in REGEX.reference.captures_iter(&visible) {
            let whole = capture.get(0).expect("reference capture");
            if visible
                .as_bytes()
                .get(whole.end())
                .is_some_and(|next| matches!(*next, b'[' | b'('))
            {
                continue;
            }
            let key = capture
                .get(2)
                .filter(|part| !part.as_str().is_empty())
                .unwrap_or_else(|| capture.get(1).expect("reference label"))
                .as_str()
                .to_lowercase();
            if let Some(destination) = definitions.get(&key) {
                document.links.push((index + 1, destination.clone()));
            } else if capture.get(2).is_some() {
                document
                    .links
                    .push((index + 1, "unresolved-reference:".to_owned()));
            }
        }
    }
    document
        .anchors
        .extend(document.explicit_anchors.iter().cloned());
    document
}

fn percent_decode(value: &str) -> BoundaryResult<String> {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' && index + 2 < input.len() {
            let hex = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            if let (Some(high), Some(low)) = (hex(input[index + 1]), hex(input[index + 2])) {
                output.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        output.push(input[index]);
        index += 1;
    }
    String::from_utf8(output).map_err(|_| "markdown-malformed-target")
}

fn local_target(
    root: &Path,
    source: &str,
    destination: &str,
) -> BoundaryResult<Option<(String, String)>> {
    let destination = html_unescape(destination);
    let destination = destination
        .trim_start_matches(|c: char| c.is_ascii_control() || c == ' ')
        .replace(['\r', '\n', '\t'], "");
    let (before_fragment, fragment) = destination.split_once('#').unwrap_or((&destination, ""));
    let mut path = before_fragment
        .split_once('?')
        .map_or(before_fragment, |(path, _)| path);
    if let Some((scheme, _)) = path.split_once(':')
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        if !matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "ftp" | "ssh"
        ) {
            return Err("markdown-unsafe-target");
        }
        if path.contains("://") {
            url_authority(path).map_err(|_| "markdown-malformed-target")?;
        }
        return Ok(None);
    }
    if let Some(network) = path.strip_prefix("//") {
        let authority = network.split('/').next().unwrap_or_default();
        if !authority.is_empty() {
            url_authority(&format!("http://{authority}"))
                .map_err(|_| "markdown-malformed-target")?;
            return Ok(None);
        }
        path = network;
    }
    let path = percent_decode(path)?;
    let anchor = percent_decode(fragment)?;
    if path.starts_with('/') || path.contains(['\\', '\0']) {
        return Err("markdown-unsafe-target");
    }
    if path.is_empty() {
        return Ok(Some((source.to_owned(), anchor)));
    }
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("markdown-target-outside-root");
                }
            }
            _ => {
                parts.push(part);
                candidate_stat(root, &parts.join("/"))?;
            }
        }
    }
    let target = parts.join("/");
    if target.is_empty() {
        return Ok(Some((".".to_owned(), anchor)));
    }
    candidate_stat(root, &target)?;
    Ok(Some((target, anchor)))
}

fn chinese_path(english: &str) -> String {
    format!(
        "{}.zh-CN.md",
        english
            .strip_suffix(".md")
            .expect("maintained markdown path")
    )
}

fn check_documents(root: &Path, texts: &BTreeMap<String, String>, report: &mut Report) {
    let documents: BTreeMap<&str, Document> = texts
        .iter()
        .filter(|(path, _)| path.to_lowercase().ends_with(".md"))
        .map(|(path, text)| (path.as_str(), markdown_document(text)))
        .collect();
    let mut published_directories = BTreeSet::new();
    for candidate in report.inventory.candidates() {
        published_directories.insert(".".to_owned());
        for (index, _) in candidate.match_indices('/') {
            published_directories.insert(candidate[..index].to_owned());
        }
    }
    let paired: BTreeSet<String> = MAINTAINED
        .iter()
        .flat_map(|english| [(*english).to_owned(), chinese_path(english)])
        .collect();
    let mut targets: BTreeMap<&str, Vec<(usize, String, String)>> = BTreeMap::new();
    for (&path, document) in &documents {
        let path_targets = targets.entry(path).or_default();
        for (number, destination) in &document.links {
            let result = (|| -> BoundaryResult<()> {
                let Some((relative, anchor)) = local_target(root, path, destination)? else {
                    return Ok(());
                };
                path_targets.push((*number, relative.clone(), anchor.clone()));
                if relative != "." {
                    let metadata = candidate_stat(root, &relative)?;
                    if metadata.is_dir() && !published_directories.contains(&relative) {
                        return Err("markdown-target-not-published");
                    }
                    if metadata.is_file() && !texts.contains_key(&relative) {
                        return Err("markdown-target-not-reviewed-text");
                    }
                }
                if !anchor.is_empty()
                    && !documents
                        .get(relative.as_str())
                        .is_some_and(|target| target.anchors.contains(&anchor))
                {
                    return Err("markdown-missing-anchor");
                }
                if paired.contains(path) && paired.contains(&relative) {
                    let is_chinese = path.ends_with(".zh-CN.md");
                    let counterpart = if is_chinese {
                        format!(
                            "{}.md",
                            path.strip_suffix(".zh-CN.md").expect("Chinese suffix")
                        )
                    } else {
                        chinese_path(path)
                    };
                    if relative != counterpart && is_chinese != relative.ends_with(".zh-CN.md") {
                        return Err("markdown-language-navigation");
                    }
                }
                Ok(())
            })();
            if let Err(category) = result {
                report.findings.insert(Finding {
                    path: path.to_owned(),
                    line: *number,
                    category,
                });
            }
        }
    }
    for english in MAINTAINED {
        let chinese = chinese_path(english);
        for (path, counterpart) in [(english, chinese.as_str()), (chinese.as_str(), english)] {
            if !documents.contains_key(path) {
                report.findings.insert(Finding {
                    path: path.to_owned(),
                    line: 0,
                    category: "missing-language-document",
                });
            } else if !targets.get(path).is_some_and(|links| {
                links.iter().any(|(number, target, anchor)| {
                    *number <= 5 && target == counterpart && anchor.is_empty()
                })
            }) {
                report.findings.insert(Finding {
                    path: path.to_owned(),
                    line: 1,
                    category: "missing-reciprocal-language-link",
                });
            }
        }
        if let (Some(english_doc), Some(chinese_doc)) =
            (documents.get(english), documents.get(chinese.as_str()))
            && english_doc.explicit_anchors != chinese_doc.explicit_anchors
        {
            for path in [english, chinese.as_str()] {
                report.findings.insert(Finding {
                    path: path.to_owned(),
                    line: 0,
                    category: "language-explicit-anchor-mismatch",
                });
            }
        }
    }
}

fn audit_at(root: &Path, requested: Option<&Path>) -> BoundaryResult<Report> {
    let root = application_root(root, requested)?;
    let inventory = inventory(&root)?;
    let candidates: Vec<String> = inventory
        .candidates()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut report = Report {
        inventory,
        findings: BTreeSet::new(),
        scanned_text: 0,
        unreviewed_or_forbidden: 0,
    };
    let mut texts = BTreeMap::new();
    for path in candidates {
        let result = (|| -> BoundaryResult<String> {
            let data = read_public_bytes(&root, &path)?;
            if data
                .iter()
                .any(|byte| *byte < 32 && !matches!(*byte, 9 | 10 | 12 | 13))
            {
                return Err("unreviewed-binary");
            }
            let text = String::from_utf8(data).map_err(|_| "unreviewed-undecodable")?;
            Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
        })();
        match result {
            Ok(text) => {
                report.findings.extend(scan_text(&path, &text));
                texts.insert(path, text);
                report.scanned_text += 1;
            }
            Err(category) => {
                report.findings.insert(Finding {
                    path,
                    line: 0,
                    category,
                });
                report.unreviewed_or_forbidden += 1;
            }
        }
    }
    check_documents(&root, &texts, &mut report);
    Ok(report)
}

fn ascii_json(value: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let json = value.to_string();
    let mut result = String::with_capacity(json.len());
    for c in json.chars() {
        if c <= '\u{7e}' {
            result.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]).iter() {
                write!(result, "\\u{unit:04x}").expect("writing to String");
            }
        }
    }
    result
}

fn render_report(report: &Report) -> Vec<String> {
    let mut lines: Vec<String> = report
        .findings
        .iter()
        .map(|finding| {
            ascii_json(&json!({
                "path": finding.path, "line": finding.line, "category": finding.category,
            }))
        })
        .collect();
    let mut counts = serde_json::Map::new();
    counts.insert("tracked".to_owned(), json!(report.inventory.tracked.len()));
    counts.insert(
        "nonignored_untracked".to_owned(),
        json!(report.inventory.untracked.len()),
    );
    counts.insert(
        "ignored_tracked".to_owned(),
        json!(report.inventory.ignored_tracked.len()),
    );
    counts.insert(
        "candidates".to_owned(),
        json!(report.inventory.candidates().len()),
    );
    if report.scanned_text != 0 {
        counts.insert("scanned_text".to_owned(), json!(report.scanned_text));
    }
    if report.unreviewed_or_forbidden != 0 {
        counts.insert(
            "unreviewed_or_forbidden".to_owned(),
            json!(report.unreviewed_or_forbidden),
        );
    }
    counts.insert("findings".to_owned(), json!(report.findings.len()));
    lines.push(ascii_json(&json!({ "inventory": counts })));
    lines
}

fn parse_arguments(args: &[OsString]) -> BoundaryResult<Option<PathBuf>> {
    let mut requested = None;
    let mut arguments = args.iter();
    while let Some(arg) = arguments.next() {
        if arg == "--root" {
            let value = arguments.next().ok_or("invalid-arguments")?;
            requested = Some(PathBuf::from(value));
        } else if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--root=")) {
            requested = Some(PathBuf::from(value));
        } else {
            return Err("invalid-arguments");
        }
    }
    Ok(requested)
}

/// Run the public-candidate audit, returning 0 for clean, 1 for findings, 2 for a boundary error.
pub fn run(args: &[OsString]) -> Result<u8, String> {
    use std::io::Write as _;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        writeln!(out, "Review Git publication candidates without walking the workspace.\n\nUsage: slimenrf-tools check-public [--root PATH]\n\n  --root PATH  Require the application root to match this directory")
            .map_err(|error| format!("writing public-check output: {error}"))?;
        return Ok(0);
    }
    let result = parse_arguments(args)
        .and_then(|requested| audit_at(&crate::workspace_root(), requested.as_deref()));
    match result {
        Ok(report) => {
            for line in render_report(&report) {
                writeln!(out, "{line}")
                    .map_err(|error| format!("writing public-check output: {error}"))?;
            }
            Ok(u8::from(!report.findings.is_empty()))
        }
        Err(category) => {
            writeln!(
                out,
                "{}",
                ascii_json(&json!({ "path": ".", "line": 0, "category": category }))
            )
            .map_err(|error| format!("writing public-check output: {error}"))?;
            Ok(2)
        }
    }
}
