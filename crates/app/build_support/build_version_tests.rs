use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const GIT_CHILD: &str = "SLIMENRF_BUILD_VERSION_GIT_TEST_CHILD";
const EMIT_CHILD: &str = "SLIMENRF_BUILD_VERSION_EMIT_TEST_CHILD";
const TRACKED: &str = "tracked file.txt";
const BRANCH: &str = "version-fixture";

fn isolate_git_environment(command: &mut Command, home: &Path) {
    for (key, _) in env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", home.join("no system git config"))
        .env("GIT_CONFIG_GLOBAL", home.join("no global git config"))
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
}

fn isolated_git_test(name: &str) -> bool {
    if let Some(child) = env::var_os(GIT_CHILD) {
        assert_eq!(child.as_os_str(), OsStr::new(name));
        println!("build-version-git-child:{name}");
        return true;
    }

    let home = tempfile::tempdir().expect("create isolated child home");
    let mut command = Command::new(env::current_exe().expect("find test executable"));
    command
        .arg("--exact")
        .arg(format!(
            "build_resource_tests::build_version::tests::{name}"
        ))
        .arg("--nocapture")
        .current_dir(home.path())
        .env(GIT_CHILD, name)
        .env_remove(EMIT_CHILD);
    isolate_git_environment(&mut command, home.path());
    let output = command.output().expect("run isolated Git test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated test {name} failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains(&format!("build-version-git-child:{name}")),
        "isolated test {name} did not execute: stdout={stdout} stderr={stderr}"
    );
    false
}

fn git_available() -> bool {
    match Command::new("git").arg("--version").output() {
        Ok(output) => {
            assert!(
                output.status.success(),
                "git --version failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("Git is unavailable; skipping repository-specific version test");
            false
        }
        Err(error) => panic!("cannot invoke git --version: {error}"),
    }
}

struct GitFixture {
    directory: tempfile::TempDir,
    root: PathBuf,
    hooks: PathBuf,
}

impl GitFixture {
    fn new() -> Option<Self> {
        if !git_available() {
            return None;
        }
        let directory = tempfile::tempdir().expect("create Git fixture directory");
        let root = directory.path().join("repository with spaces");
        let hooks = directory.path().join("empty hooks");
        fs::create_dir_all(&root).expect("create Git fixture root");
        fs::create_dir_all(&hooks).expect("create empty hooks directory");
        let fixture = Self {
            directory,
            root,
            hooks,
        };
        fixture.git(&fixture.root, &["init", "--quiet"]);
        fixture.git(
            &fixture.root,
            &["symbolic-ref", "HEAD", "refs/heads/version-fixture"],
        );
        fixture.write(TRACKED, b"initial tracked content\n");
        fixture.write(".gitignore", b"ignored file.txt\n");
        fixture.git(&fixture.root, &["add", "--", TRACKED, ".gitignore"]);
        fixture.git(
            &fixture.root,
            &["commit", "--quiet", "-m", "initial fixture"],
        );
        Some(fixture)
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new("git");
        command.current_dir(cwd);
        isolate_git_environment(&mut command, self.directory.path());
        command
            .args(["-c", "commit.gpgsign=false"])
            .args(["-c", "tag.gpgsign=false"])
            .args(["-c", "core.autocrlf=false"])
            .args(["-c", "core.fsmonitor=false"])
            .arg("-c")
            .arg(format!("core.hooksPath={}", self.hooks.display()))
            .env("GIT_AUTHOR_NAME", "Version Fixture")
            .env("GIT_AUTHOR_EMAIL", "version-fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Version Fixture")
            .env("GIT_COMMITTER_EMAIL", "version-fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", "2001-02-03T04:05:06+0000")
            .env("GIT_COMMITTER_DATE", "2001-02-03T04:05:06+0000");
        command
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let mut command = self.command(cwd);
        command.args(args);
        git_output(&mut command)
    }

    fn head(&self, cwd: &Path) -> String {
        let head = self.git(cwd, &["rev-parse", "--verify", "HEAD^{commit}"]);
        assert!(matches!(head.len(), 40 | 64), "unexpected Git HEAD: {head}");
        assert!(head.bytes().all(|byte| byte.is_ascii_hexdigit()));
        head
    }

    fn write(&self, path: &str, content: &[u8]) {
        fs::write(self.root.join(path), content).expect("write Git fixture file");
    }
}

fn git_output(command: &mut Command) -> String {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cannot run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output is UTF-8")
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn version(
    root: &Path,
    base: &str,
    in_actions: bool,
    actions_sha: Option<&str>,
) -> super::BuildVersion {
    super::resolve_version(root, base, in_actions, actions_sha, OsStr::new("git"))
}

fn short(head: &str) -> String {
    head[..7].to_ascii_lowercase()
}

fn normalized_path(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = fs::canonicalize(parent)
    {
        return parent.join(name);
    }
    path.to_path_buf()
}

fn is_watched(snapshot: &super::BuildVersion, path: &Path) -> bool {
    let path = normalized_path(path);
    snapshot
        .watched
        .iter()
        .any(|watched| normalized_path(watched) == path)
}

fn assert_watched(snapshot: &super::BuildVersion, path: &Path) {
    assert!(
        is_watched(snapshot, path),
        "expected {path:?} in watches {:?}",
        snapshot.watched
    );
}

fn assert_not_watched(snapshot: &super::BuildVersion, path: &Path) {
    assert!(
        !is_watched(snapshot, path),
        "unexpected {path:?} in watches {:?}",
        snapshot.watched
    );
}

fn assert_safe_watches(snapshot: &super::BuildVersion, root: &Path, allowed_missing: &[&Path]) {
    assert_not_watched(snapshot, root);
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        assert_not_watched(snapshot, &dot_git);
    }
    for watched in &snapshot.watched {
        assert!(
            watched.exists()
                || allowed_missing
                    .iter()
                    .any(|path| normalized_path(path) == normalized_path(watched)),
            "unexpected nonexistent watch {watched:?} in {:?}",
            snapshot.watched
        );
    }
}

#[test]
fn fallback_without_git_validates_actions_sha() {
    let directory = tempfile::tempdir().expect("create source fixture");
    let root = directory.path().join("source copy with spaces");
    fs::create_dir_all(&root).expect("create source copy");
    let missing_git = directory.path().join("nonexistent git executable");
    let resolve = |base, actions, sha| {
        super::resolve_version(&root, base, actions, sha, missing_git.as_os_str())
    };
    let sha40 = "ABCDEF01".repeat(5);
    let sha64 = "FEDCBA98".repeat(8);

    let local = resolve("1.2.3", false, Some(&sha40));
    assert_eq!(local.display, "1.2.3");
    assert!(local.watched.is_empty());
    let no_sha = resolve("1.2.3", true, None);
    assert_eq!(no_sha.display, "1.2.3");
    assert!(no_sha.watched.is_empty());
    let actions = resolve("1.2.3", true, Some(&sha40));
    assert_eq!(actions.display, "1.2.3+abcdef0");
    assert!(actions.watched.is_empty());
    let existing_metadata = resolve("1.2.3-rc.1+build.9", true, Some(&sha64));
    assert_eq!(existing_metadata.display, "1.2.3-rc.1+build.9.fedcba9");
    assert!(existing_metadata.watched.is_empty());
    semver::Version::parse(&existing_metadata.display).expect("result remains valid SemVer");

    let invalid = [
        String::new(),
        "abcdef0".to_owned(),
        "a".repeat(39),
        "a".repeat(41),
        "a".repeat(63),
        "a".repeat(65),
        format!("{}g", "a".repeat(39)),
        format!(" {sha40}"),
        format!("{sha40}\n"),
        format!("{sha40}\ncargo:rustc-env=INJECTED=1"),
        format!("{sha40}\rcargo:rustc-env=INJECTED=1"),
        "💥".repeat(10),
    ];
    for sha in invalid {
        let result =
            super::resolve_version(&root, "1.2.3", true, Some(&sha), missing_git.as_os_str());
        assert_eq!(result.display, "1.2.3", "invalid SHA {sha:?}");
        assert!(result.watched.is_empty(), "invalid SHA {sha:?}");
    }
}

#[test]
fn tracked_changes_reverts_and_exclusions_update_the_version() {
    if !isolated_git_test("tracked_changes_reverts_and_exclusions_update_the_version") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let tracked = root.join(TRACKED);
    let head = fixture.head(root);
    let clean = format!("1.2.3+{}", short(&head));
    let dirty = format!("{clean}.dirty");

    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, clean);
    assert_watched(&snapshot, &root.join(".git/HEAD"));
    assert_watched(&snapshot, &root.join(".git/index"));
    assert_watched(&snapshot, &root.join(".git/refs/heads").join(BRANCH));
    assert_watched(&snapshot, &tracked);
    assert_watched(&snapshot, &root.join(".gitignore"));
    assert_not_watched(&snapshot, &root.join(".git/packed-refs"));
    assert_not_watched(&snapshot, &root.join(".git/index.lock"));
    assert_not_watched(&snapshot, &root.join(".git/refs/heads/nonexistent"));
    assert_safe_watches(&snapshot, root, &[]);

    fixture.write(TRACKED, b"unstaged change\n");
    assert_eq!(fixture.git(root, &["diff", "--name-only"]), TRACKED);
    assert!(
        fixture
            .git(root, &["diff", "--cached", "--name-only"])
            .is_empty()
    );
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, dirty);
    assert_watched(&snapshot, &tracked);
    assert_safe_watches(&snapshot, root, &[]);
    let metadata = version(root, "1.2.3+existing", false, None);
    assert_eq!(
        metadata.display,
        format!("1.2.3+existing.{}.dirty", short(&head))
    );
    semver::Version::parse(&metadata.display).expect("dirty result remains valid SemVer");
    fixture.git(root, &["checkout", "--", TRACKED]);
    assert_eq!(version(root, "1.2.3", false, None).display, clean);

    fixture.write(TRACKED, b"staged change\n");
    fixture.git(root, &["add", "--", TRACKED]);
    assert!(fixture.git(root, &["diff", "--name-only"]).is_empty());
    assert_eq!(
        fixture.git(root, &["diff", "--cached", "--name-only"]),
        TRACKED
    );
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, dirty);
    assert_watched(&snapshot, &root.join(".git/index"));
    assert_safe_watches(&snapshot, root, &[]);
    fixture.git(root, &["checkout", "HEAD", "--", TRACKED]);
    assert_eq!(version(root, "1.2.3", false, None).display, clean);

    fixture.write("untracked file.txt", b"untracked\n");
    fixture.write("ignored file.txt", b"ignored\n");
    assert_eq!(
        fixture.git(root, &["check-ignore", "ignored file.txt"]),
        "ignored file.txt"
    );
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, clean);
    assert_not_watched(&snapshot, &root.join("untracked file.txt"));
    assert_not_watched(&snapshot, &root.join("ignored file.txt"));
    assert_safe_watches(&snapshot, root, &[]);

    fs::remove_file(&tracked).expect("delete tracked fixture file");
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, dirty);
    assert_watched(&snapshot, &tracked);
    assert_safe_watches(&snapshot, root, &[&tracked]);
    fixture.git(root, &["checkout", "--", TRACKED]);
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, clean);
    assert_watched(&snapshot, &tracked);
    assert_safe_watches(&snapshot, root, &[]);

    fixture.write(TRACKED, b"committed change\n");
    fixture.git(root, &["add", "--", TRACKED]);
    fixture.git(root, &["commit", "--quiet", "-m", "second fixture commit"]);
    let next_head = fixture.head(root);
    assert_ne!(next_head, head);
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&next_head)));
    assert_watched(&snapshot, &root.join(".git/refs/heads").join(BRANCH));
    assert_safe_watches(&snapshot, root, &[]);
}

#[test]
fn detached_head_and_commits_use_the_checked_out_commit() {
    if !isolated_git_test("detached_head_and_commits_use_the_checked_out_commit") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let branch_head = fixture.head(root);
    fixture.git(root, &["checkout", "--quiet", "--detach", "HEAD"]);
    assert_eq!(
        fs::read_to_string(root.join(".git/HEAD"))
            .expect("read detached HEAD")
            .trim(),
        branch_head
    );
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&branch_head)));
    assert_watched(&snapshot, &root.join(".git/HEAD"));
    assert_watched(&snapshot, &root.join(".git/index"));
    assert_not_watched(&snapshot, &root.join(".git/refs/heads").join(BRANCH));
    assert_safe_watches(&snapshot, root, &[]);

    fixture.write(TRACKED, b"detached change\n");
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(
        snapshot.display,
        format!("1.2.3+{}.dirty", short(&branch_head))
    );
    assert_safe_watches(&snapshot, root, &[]);
    fixture.git(root, &["add", "--", TRACKED]);
    fixture.git(
        root,
        &["commit", "--quiet", "-m", "detached fixture commit"],
    );
    let detached_head = fixture.head(root);
    assert_ne!(detached_head, branch_head);
    assert_eq!(
        fixture.git(root, &["rev-parse", "refs/heads/version-fixture"]),
        branch_head
    );
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&detached_head)));
    assert_watched(&snapshot, &root.join(".git/HEAD"));
    assert_not_watched(&snapshot, &root.join(".git/refs/heads").join(BRANCH));
    assert_safe_watches(&snapshot, root, &[]);
}

#[test]
fn packed_branch_ref_tracks_later_loose_ref_updates() {
    if !isolated_git_test("packed_branch_ref_tracks_later_loose_ref_updates") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let original = fixture.head(root);
    fixture.git(root, &["pack-refs", "--all", "--prune"]);
    let packed_refs = root.join(".git/packed-refs");
    let loose_ref = root.join(".git/refs/heads").join(BRANCH);
    assert!(packed_refs.is_file());
    assert!(!loose_ref.exists());
    let mut ref_parent = loose_ref.parent().expect("branch ref has a parent");
    while !ref_parent.is_dir() {
        ref_parent = ref_parent.parent().expect("refs directory exists");
    }
    assert!(ref_parent.starts_with(root.join(".git/refs")));
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&original)));
    assert_watched(&snapshot, &root.join(".git/HEAD"));
    assert_watched(&snapshot, &root.join(".git/index"));
    assert_watched(&snapshot, &packed_refs);
    assert_watched(&snapshot, ref_parent);
    assert_not_watched(&snapshot, &loose_ref);
    assert_safe_watches(&snapshot, root, &[]);

    fixture.write(TRACKED, b"new commit after packing\n");
    fixture.git(root, &["add", "--", TRACKED]);
    fixture.git(root, &["commit", "--quiet", "-m", "commit after pack"]);
    let next = fixture.head(root);
    assert_ne!(next, original);
    assert!(loose_ref.is_file());
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&next)));
    assert_watched(&snapshot, &loose_ref);
    assert_watched(&snapshot, &packed_refs);
    assert_safe_watches(&snapshot, root, &[]);
}

#[test]
fn linked_worktree_uses_its_git_dir_and_shared_refs() {
    if !isolated_git_test("linked_worktree_uses_its_git_dir_and_shared_refs") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let original = fixture.head(&fixture.root);
    let worktree = fixture.directory.path().join("linked worktree with spaces");
    let mut command = fixture.command(&fixture.root);
    command
        .args(["worktree", "add", "--quiet", "-b", "linked-fixture"])
        .arg(&worktree);
    git_output(&mut command);

    let git_dir =
        fs::canonicalize(worktree.join(fixture.git(&worktree, &["rev-parse", "--git-dir"])))
            .expect("resolve linked worktree Git directory");
    let common_dir =
        fs::canonicalize(worktree.join(fixture.git(&worktree, &["rev-parse", "--git-common-dir"])))
            .expect("resolve shared Git directory");
    assert_ne!(git_dir, common_dir);
    assert_eq!(
        common_dir,
        fs::canonicalize(fixture.root.join(".git")).expect("resolve main Git directory")
    );
    assert!(worktree.join(".git").is_file());
    let branch_ref = common_dir.join("refs/heads/linked-fixture");
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&original)));
    assert_watched(&snapshot, &worktree.join(".git"));
    assert_watched(&snapshot, &git_dir.join("HEAD"));
    assert_watched(&snapshot, &git_dir.join("index"));
    assert_watched(&snapshot, &branch_ref);
    assert_watched(&snapshot, &worktree.join(TRACKED));
    assert_not_watched(&snapshot, &git_dir);
    assert_not_watched(&snapshot, &common_dir);
    assert_safe_watches(&snapshot, &worktree, &[]);

    fs::write(worktree.join(TRACKED), b"linked worktree change\n").expect("modify linked worktree");
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(
        snapshot.display,
        format!("1.2.3+{}.dirty", short(&original))
    );
    assert_safe_watches(&snapshot, &worktree, &[]);
    fixture.git(&worktree, &["add", "--", TRACKED]);
    fixture.git(
        &worktree,
        &["commit", "--quiet", "-m", "linked fixture commit"],
    );
    let linked_head = fixture.head(&worktree);
    assert_ne!(linked_head, original);
    assert_eq!(fixture.head(&fixture.root), original);
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&linked_head)));
    assert_watched(&snapshot, &git_dir.join("HEAD"));
    assert_watched(&snapshot, &git_dir.join("index"));
    assert_watched(&snapshot, &branch_ref);
    assert_safe_watches(&snapshot, &worktree, &[]);
}

#[test]
fn source_copy_inside_an_unrelated_repository_does_not_use_its_head() {
    if !isolated_git_test("source_copy_inside_an_unrelated_repository_does_not_use_its_head") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let source_copy = fixture.root.join("nested source copy with spaces");
    fs::create_dir_all(&source_copy).expect("create nested source copy");
    assert_eq!(fixture.head(&source_copy), fixture.head(&fixture.root));
    let fallback = "ABCDEF01".repeat(5);
    let snapshot = version(&source_copy, "1.2.3", false, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3");
    assert!(snapshot.watched.is_empty());
    let snapshot = version(&source_copy, "1.2.3", true, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3+abcdef0");
    assert!(snapshot.watched.is_empty());

    let unborn = fixture
        .directory
        .path()
        .join("unborn repository with spaces");
    fs::create_dir_all(&unborn).expect("create unborn repository");
    fixture.git(&unborn, &["init", "--quiet"]);
    fixture.git(&unborn, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    let index = unborn.join(".git/index");
    let branch_ref = unborn.join(".git/refs/heads/unborn");
    let packed_refs = unborn.join(".git/packed-refs");
    assert!(!index.exists());
    assert!(!branch_ref.exists());
    assert!(!packed_refs.exists());
    let snapshot = version(&unborn, "1.2.3", false, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3");
    assert_watched(&snapshot, &unborn.join(".git/HEAD"));
    assert_not_watched(&snapshot, &index);
    assert_not_watched(&snapshot, &branch_ref);
    assert_not_watched(&snapshot, &packed_refs);
    assert_safe_watches(&snapshot, &unborn, &[]);
    let snapshot = version(&unborn, "1.2.3", true, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3+abcdef0");
    assert_not_watched(&snapshot, &index);
    assert_not_watched(&snapshot, &branch_ref);
    assert_not_watched(&snapshot, &packed_refs);
    assert_safe_watches(&snapshot, &unborn, &[]);

    let no_repository = fixture.directory.path().join("standalone source copy");
    fs::create_dir_all(&no_repository).expect("create standalone source copy");
    let snapshot = version(&no_repository, "1.2.3", false, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3");
    assert!(snapshot.watched.is_empty());
    let snapshot = version(&no_repository, "1.2.3", true, Some(&fallback));
    assert_eq!(snapshot.display, "1.2.3+abcdef0");
    assert!(snapshot.watched.is_empty());

    let absent_git = fixture.directory.path().join("missing git executable");
    let snapshot =
        super::resolve_version(&fixture.root, "1.2.3", false, None, absent_git.as_os_str());
    assert_eq!(snapshot.display, "1.2.3");
    assert!(snapshot.watched.is_empty());
    let snapshot = super::resolve_version(
        &fixture.root,
        "1.2.3",
        true,
        Some(&fallback),
        absent_git.as_os_str(),
    );
    assert_eq!(snapshot.display, "1.2.3+abcdef0");
    assert!(snapshot.watched.is_empty());
}

#[test]
fn actions_prefer_actual_head_and_ignore_staged_and_unstaged_changes() {
    if !isolated_git_test("actions_prefer_actual_head_and_ignore_staged_and_unstaged_changes") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let head = fixture.head(root);
    let fallback = if short(&head) == "aaaaaaa" {
        "b".repeat(40)
    } else {
        "a".repeat(40)
    };
    fixture.write(TRACKED, b"staged change\n");
    fixture.git(root, &["add", "--", TRACKED]);
    fixture.write(TRACKED, b"unstaged change on top\n");
    assert_eq!(fixture.git(root, &["diff", "--name-only"]), TRACKED);
    assert_eq!(
        fixture.git(root, &["diff", "--cached", "--name-only"]),
        TRACKED
    );
    let local = version(root, "1.2.3", false, Some(&fallback));
    assert_eq!(local.display, format!("1.2.3+{}.dirty", short(&head)));
    assert_safe_watches(&local, root, &[]);
    let actions = version(root, "1.2.3+existing", true, Some(&fallback));
    assert_eq!(actions.display, format!("1.2.3+existing.{}", short(&head)));
    assert_watched(&actions, &root.join(".git/HEAD"));
    assert_watched(&actions, &root.join(".git/index"));
    assert_watched(&actions, &root.join(".git/refs/heads").join(BRANCH));
    assert_safe_watches(&actions, root, &[]);
    let invalid_fallback = version(root, "1.2.3", true, Some("bad\ninjection"));
    assert_eq!(invalid_fallback.display, format!("1.2.3+{}", short(&head)));
    assert_safe_watches(&invalid_fallback, root, &[]);
}

fn emit_in_child(
    manifest: &Path,
    cwd: &Path,
    home: &Path,
    actions: Option<&str>,
    sha: Option<&str>,
) -> String {
    let mut command = Command::new(env::current_exe().expect("find test executable"));
    command
        .arg("--exact")
        .arg(
            "build_resource_tests::build_version::tests::emits_cargo_directives_in_a_child_process",
        )
        .arg("--nocapture")
        .current_dir(cwd)
        .env(EMIT_CHILD, "1")
        .env_remove(GIT_CHILD)
        .env("CARGO_MANIFEST_DIR", manifest)
        .env("CARGO_PKG_VERSION", "1.2.3")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("GITHUB_SHA");
    isolate_git_environment(&mut command, home);
    if let Some(actions) = actions {
        command.env("GITHUB_ACTIONS", actions);
    }
    if let Some(sha) = sha {
        command.env("GITHUB_SHA", sha);
    }
    let output = command.output().expect("run version emitter child");
    let stdout = String::from_utf8(output.stdout).expect("emitter output is UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "version emitter child failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("build-version-emitter-child"),
        "version emitter child did not execute: stdout={stdout} stderr={stderr}"
    );
    stdout
}

fn directives<'a>(stdout: &'a str, prefix: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .filter_map(|line| line.split_once(prefix).map(|(_, value)| value))
        .collect()
}

fn assert_emitted(stdout: &str, expected: &str) {
    assert_eq!(
        directives(stdout, "cargo:rustc-env=SLIMENRF_BUILD_DISPLAY_VERSION="),
        [expected],
        "emitter output: {stdout}"
    );
    assert_eq!(
        directives(stdout, "cargo:rerun-if-env-changed="),
        ["GITHUB_ACTIONS", "GITHUB_SHA", "PATH"],
        "emitter output: {stdout}"
    );
}

#[test]
fn emits_cargo_directives_in_a_child_process() {
    if env::var_os(EMIT_CHILD).is_some() {
        println!("build-version-emitter-child");
        super::emit_build_version();
        return;
    }

    let directory = tempfile::tempdir().expect("create emitter fixture");
    let source = directory.path().join("source copy with spaces");
    let manifest = source.join("crates/app");
    let cwd = directory.path().join("unrelated current directory");
    fs::create_dir_all(&manifest).expect("create fixture app manifest directory");
    fs::create_dir_all(&cwd).expect("create unrelated current directory");
    let fallback = "ABCDEF01".repeat(5);
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("true"),
        Some(&fallback),
    );
    assert_emitted(&stdout, "1.2.3+abcdef0");
    assert!(directives(&stdout, "cargo:rerun-if-changed=").is_empty());
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("false"),
        Some(&fallback),
    );
    assert_emitted(&stdout, "1.2.3");
    let injection = format!("{fallback}\ncargo:rustc-env=INJECTED=1");
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("true"),
        Some(&injection),
    );
    assert_emitted(&stdout, "1.2.3");
    assert!(!stdout.contains("cargo:rustc-env=INJECTED=1"));

    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let head = fixture.head(&fixture.root);
    fixture.write(TRACKED, b"dirty while emitting\n");
    let manifest = fixture.root.join("crates/app");
    fs::create_dir_all(&manifest).expect("create fixture app manifest directory");
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("true"),
        Some(&fallback),
    );
    assert_emitted(&stdout, &format!("1.2.3+{}", short(&head)));
    let watches: Vec<_> = directives(&stdout, "cargo:rerun-if-changed=")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    assert!(
        watches
            .iter()
            .any(|path| normalized_path(path) == normalized_path(&fixture.root.join(".git/HEAD"))),
        "emitter did not watch HEAD: {stdout}"
    );
    assert!(
        watches.iter().all(|path| path.exists()),
        "emitter output: {stdout}"
    );
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("false"),
        Some(&fallback),
    );
    assert_emitted(&stdout, &format!("1.2.3+{}.dirty", short(&head)));
}
