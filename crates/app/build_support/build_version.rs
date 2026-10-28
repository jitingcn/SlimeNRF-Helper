use std::{
    collections::BTreeSet,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct BuildVersion {
    display: String,
    watched: BTreeSet<PathBuf>,
}

pub(super) fn emit_build_version() {
    println!("cargo:rerun-if-env-changed=GITHUB_ACTIONS");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=PATH");

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let in_actions = env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
    let actions_sha = env::var("GITHUB_SHA").ok();
    let snapshot = env::var_os("CARGO_MANIFEST_DIR").map_or_else(
        || BuildVersion {
            display: display_version(
                &version,
                actions_sha.as_deref().filter(|_| in_actions),
                false,
            ),
            watched: BTreeSet::new(),
        },
        |manifest| {
            let root = PathBuf::from(manifest).join("../..");
            resolve_version(
                &root,
                &version,
                in_actions,
                actions_sha.as_deref(),
                OsStr::new("git"),
            )
        },
    );
    for path in snapshot.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rustc-env=SLIMENRF_BUILD_DISPLAY_VERSION={}",
        snapshot.display
    );
}

fn display_version(base: &str, sha: Option<&str>, dirty: bool) -> String {
    let Some(sha) = sha.and_then(short_sha) else {
        return base.to_owned();
    };
    let separator = if base.contains('+') { '.' } else { '+' };
    let dirty = if dirty { ".dirty" } else { "" };
    format!("{base}{separator}{sha}{dirty}")
}

fn valid_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn short_sha(sha: &str) -> Option<String> {
    valid_sha(sha).then(|| sha[..7].to_ascii_lowercase())
}

struct Git<'a> {
    root: &'a Path,
    executable: &'a OsStr,
}

impl Git<'_> {
    fn output(&self, args: &[&str]) -> Option<Vec<u8>> {
        let output = Command::new(self.executable)
            .current_dir(self.root)
            .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
            .args(args)
            // A source copy inside another checkout, or a caller's alternate index,
            // must not identify or inspect a different worktree.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_NAMESPACE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    }

    fn text(&self, args: &[&str]) -> Option<String> {
        let output = self.output(args)?;
        let output = String::from_utf8(output).ok()?;
        Some(output.trim_end_matches(['\r', '\n']).to_owned())
    }
}

fn resolve_version(
    root: &Path,
    base: &str,
    in_actions: bool,
    actions_sha: Option<&str>,
    git_executable: &OsStr,
) -> BuildVersion {
    let mut watched = BTreeSet::new();
    let git = Git {
        root,
        executable: git_executable,
    };
    let sha = repository(&git).and_then(|repository| {
        repository.watch_git_files(&git, &mut watched);
        let missing = if in_actions {
            BTreeSet::new()
        } else {
            watch_tracked_files(&git, &mut watched)
        };
        let head = git.text(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        if !valid_sha(&head) {
            return None;
        }
        let dirty = !in_actions
            && git
                .output(&[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--no-renames",
                    "--untracked-files=no",
                    "--ignore-submodules=untracked",
                ])
                .is_some_and(|status| {
                    watch_deleted_files(&git, &status, &missing, &mut watched);
                    !status.is_empty()
                });
        Some((head, dirty))
    });
    let (sha, dirty) = match sha.as_ref() {
        Some((head, dirty)) => (Some(head.as_str()), *dirty),
        None => (actions_sha.filter(|_| in_actions), false),
    };
    BuildVersion {
        display: display_version(base, sha, dirty),
        watched,
    }
}

struct Repository {
    git_dir: PathBuf,
    common_dir: PathBuf,
}

fn repository(git: &Git<'_>) -> Option<Repository> {
    let paths = git.text(&[
        "rev-parse",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ])?;
    let mut paths = paths.lines();
    let top_level = PathBuf::from(paths.next()?);
    let git_dir = git.root.join(paths.next()?);
    let common_dir = git.root.join(paths.next()?);
    if paths.next().is_some()
        || fs::canonicalize(top_level).ok()? != fs::canonicalize(git.root).ok()?
    {
        return None;
    }
    Some(Repository {
        git_dir,
        common_dir,
    })
}

impl Repository {
    fn watch_git_files(&self, git: &Git<'_>, watched: &mut BTreeSet<PathBuf>) {
        // Linked worktrees have a .git file whose target can change; normal .git
        // directories must not be watched recursively.
        watch_file(&git.root.join(".git"), watched);
        let head = self.git_dir.join("HEAD");
        watch_file(&head, watched);
        watch_file(&self.git_dir.join("index"), watched);
        watch_file(&self.common_dir.join("packed-refs"), watched);

        let mut next = symbolic_reference(&head);
        let mut seen = BTreeSet::new();
        while let Some(reference) = next {
            if !reference.starts_with("refs/")
                || reference.contains('\\')
                || reference
                    .split('/')
                    .any(|component| component.is_empty() || matches!(component, "." | ".."))
                || !seen.insert(reference.clone())
            {
                break;
            }
            let Some(path) = git.text(&["rev-parse", "--git-path", &reference]) else {
                break;
            };
            let path = git.root.join(path);
            if path.is_file() {
                watch_file(&path, watched);
            } else {
                // A packed ref can acquire a loose file without changing HEAD or
                // packed-refs. Watch only its nearest existing refs directory.
                self.watch_ref_parent(&path, watched);
            }
            next = symbolic_reference(&path);
        }
    }

    fn watch_ref_parent(&self, path: &Path, watched: &mut BTreeSet<PathBuf>) {
        let common_refs = fs::canonicalize(self.common_dir.join("refs")).ok();
        let worktree_refs = fs::canonicalize(self.git_dir.join("refs")).ok();
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory.is_dir() {
                if let Ok(resolved) = fs::canonicalize(directory)
                    && (common_refs
                        .as_ref()
                        .is_some_and(|refs| resolved.starts_with(refs))
                        || worktree_refs
                            .as_ref()
                            .is_some_and(|refs| resolved.starts_with(refs)))
                {
                    watch_path(directory, watched);
                }
                break;
            }
            parent = directory.parent();
        }
    }
}

fn symbolic_reference(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let reference = content
        .strip_prefix("ref: ")?
        .trim_end_matches(['\r', '\n']);
    (!reference.is_empty()).then(|| reference.to_owned())
}

fn watch_tracked_files(git: &Git<'_>, watched: &mut BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut missing = BTreeSet::new();
    let Some(files) = git.output(&["ls-files", "--stage", "-z"]) else {
        return missing;
    };
    for entry in files
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        // Submodules are directories, not source-file watches.
        if entry.starts_with(b"160000 ") {
            continue;
        }
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let Ok(file) = std::str::from_utf8(&entry[tab + 1..]) else {
            continue;
        };
        let path = git.root.join(file);
        if path.is_file() {
            watch_path(&path, watched);
        } else if fs::symlink_metadata(&path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            missing.insert(path);
        }
    }
    missing
}

fn watch_deleted_files(
    git: &Git<'_>,
    status: &[u8],
    missing: &BTreeSet<PathBuf>,
    watched: &mut BTreeSet<PathBuf>,
) {
    for entry in status.split(|byte| *byte == 0) {
        // Porcelain XY: only a worktree deletion can be undone without an index
        // update. Do not watch sparse-checkout omissions or absent Git refs.
        if entry.len() < 4 || entry[1] != b'D' || entry[2] != b' ' {
            continue;
        }
        let Ok(file) = std::str::from_utf8(&entry[3..]) else {
            continue;
        };
        let path = git.root.join(file);
        if missing.contains(&path) {
            // Cargo reruns while this tracked file is absent; this is necessary
            // to detect its restoration without recursively watching the root.
            watch_path(&path, watched);
        }
    }
}

fn watch_file(path: &Path, watched: &mut BTreeSet<PathBuf>) {
    if path.is_file() {
        watch_path(path, watched);
    }
}

fn watch_path(path: &Path, watched: &mut BTreeSet<PathBuf>) {
    if path
        .to_str()
        .is_some_and(|path| !path.contains(['\r', '\n']))
    {
        watched.insert(path.to_owned());
    }
}

#[cfg(test)]
#[path = "build_version_tests.rs"]
mod tests;
