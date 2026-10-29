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

fn assert_ref_creation_watched(snapshot: &super::BuildVersion, reference: &Path) {
    let mut parent = reference.parent().expect("ref has a parent");
    while !parent.is_dir() {
        parent = parent.parent().expect("refs directory exists");
    }
    assert_watched(snapshot, parent);
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
fn exact_release_tags_use_the_base_version_only_when_clean() {
    if !isolated_git_test("exact_release_tags_use_the_base_version_only_when_clean") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let head = fixture.head(root);
    let dev = format!("1.2.3+{}", short(&head));
    let dirty = format!("{dev}.dirty");
    let other_event_sha = if short(&head) == "aaaaaaa" {
        "b".repeat(40)
    } else {
        "a".repeat(40)
    };

    assert_eq!(version(root, "1.2.3", false, None).display, dev);
    fixture.git(root, &["tag", "v1.2.30"]);
    fixture.git(root, &["tag", "V1.2.3"]);
    assert_eq!(version(root, "1.2.3", false, None).display, dev);
    fixture.git(root, &["tag", "-d", "V1.2.3"]);

    fixture.git(root, &["tag", "v1.2.3"]);
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, "1.2.3");
    assert_watched(&snapshot, &root.join(".git/refs/tags/v1.2.3"));
    assert_safe_watches(&snapshot, root, &[]);
    assert_eq!(
        version(root, "1.2.3", true, Some(&other_event_sha)).display,
        "1.2.3"
    );

    fixture.write(TRACKED, b"modified release checkout\n");
    assert_eq!(version(root, "1.2.3", false, None).display, dirty);
    assert_eq!(
        version(root, "1.2.3", true, Some(&other_event_sha)).display,
        "1.2.3"
    );
    fixture.git(root, &["checkout", "--", TRACKED]);

    fixture.git(root, &["tag", "-d", "v1.2.3"]);
    fixture.git(root, &["tag", "-a", "v1.2.3", "-m", "annotated release"]);
    assert_eq!(
        fixture.git(root, &["cat-file", "-t", "refs/tags/v1.2.3"]),
        "tag"
    );
    fixture.git(root, &["checkout", "--quiet", "--detach", "HEAD"]);
    assert_eq!(version(root, "1.2.3", false, None).display, "1.2.3");

    let prerelease = "1.2.3-rc.1+build.7";
    fixture.git(root, &["tag", "v1.2.3-rc.1+build.7"]);
    assert_eq!(version(root, prerelease, false, None).display, prerelease);
    fixture.write(TRACKED, b"modified prerelease checkout\n");
    let metadata = version(root, prerelease, false, None).display;
    assert_eq!(
        metadata,
        format!("1.2.3-rc.1+build.7.{}.dirty", short(&head))
    );
    semver::Version::parse(&metadata).expect("dirty prerelease remains valid SemVer");
}

#[test]
fn annotated_release_tag_must_point_to_head_after_commits_and_moves() {
    if !isolated_git_test("annotated_release_tag_must_point_to_head_after_commits_and_moves") {
        return;
    }
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let root = &fixture.root;
    let original = fixture.head(root);
    fixture.git(root, &["tag", "-a", "v1.2.3", "-m", "first release"]);
    assert_eq!(version(root, "1.2.3", false, None).display, "1.2.3");

    fixture.write(TRACKED, b"commit after release\n");
    fixture.git(root, &["add", "--", TRACKED]);
    fixture.git(root, &["commit", "--quiet", "-m", "commit after release"]);
    let head = fixture.head(root);
    assert_ne!(head, original);
    let dev = format!("1.2.3+{}", short(&head));
    let snapshot = version(root, "1.2.3", false, None);
    assert_eq!(snapshot.display, dev);
    assert_watched(&snapshot, &root.join(".git/refs/tags/v1.2.3"));
    assert_eq!(version(root, "1.2.3", true, Some(&original)).display, dev);

    fixture.git(
        root,
        &["tag", "-f", "-a", "v1.2.3", "-m", "moved to head", "HEAD"],
    );
    assert_eq!(version(root, "1.2.3", false, None).display, "1.2.3");
    fixture.git(
        root,
        &["tag", "-f", "-a", "v1.2.3", "-m", "moved back", &original],
    );
    assert_eq!(version(root, "1.2.3", false, None).display, dev);
    fixture.git(root, &["tag", "-d", "v1.2.3"]);
    assert_eq!(version(root, "1.2.3", false, None).display, dev);
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
    let tag_ref = common_dir.join("refs/tags/v1.2.3");
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&original)));
    assert_watched(&snapshot, &worktree.join(".git"));
    assert_watched(&snapshot, &git_dir.join("HEAD"));
    assert_watched(&snapshot, &git_dir.join("index"));
    assert_watched(&snapshot, &branch_ref);
    assert_ref_creation_watched(&snapshot, &tag_ref);
    assert_watched(&snapshot, &worktree.join(TRACKED));
    assert_not_watched(&snapshot, &git_dir);
    assert_not_watched(&snapshot, &common_dir);
    assert_safe_watches(&snapshot, &worktree, &[]);

    fixture.git(&fixture.root, &["tag", "v1.2.3"]);
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, "1.2.3");
    assert_watched(&snapshot, &tag_ref);
    assert_not_watched(&snapshot, &common_dir);

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

    fixture.git(&fixture.root, &["tag", "-f", "v1.2.3", &linked_head]);
    assert_eq!(version(&worktree, "1.2.3", false, None).display, "1.2.3");
    fixture.git(&fixture.root, &["pack-refs", "--all", "--prune"]);
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, "1.2.3");
    assert_watched(&snapshot, &common_dir.join("packed-refs"));
    assert_ref_creation_watched(&snapshot, &tag_ref);
    assert_not_watched(&snapshot, &tag_ref);
    assert_not_watched(&snapshot, &common_dir);
    assert_safe_watches(&snapshot, &worktree, &[]);

    fixture.git(&fixture.root, &["tag", "-f", "v1.2.3", &original]);
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&linked_head)));
    assert_watched(&snapshot, &tag_ref);
    assert_watched(&snapshot, &common_dir.join("packed-refs"));
    fixture.git(&fixture.root, &["tag", "-d", "v1.2.3"]);
    let snapshot = version(&worktree, "1.2.3", false, None);
    assert_eq!(snapshot.display, format!("1.2.3+{}", short(&linked_head)));
    assert_ref_creation_watched(&snapshot, &tag_ref);
    assert_safe_watches(&snapshot, &worktree, &[]);
}

fn assert_incremental_version(fixture: &GitFixture, expected: &str, expected_runs: usize) {
    let mut command = Command::new(option_env!("CARGO").unwrap_or("cargo"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "--offline", "--quiet", "--manifest-path"])
        .arg(fixture.root.join("crates/app/Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            fixture.directory.path().join("incremental target"),
        )
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env("GITHUB_ACTIONS", "false")
        .env_remove("GITHUB_SHA");
    isolate_git_environment(&mut command, fixture.directory.path());
    // Keep rustup's real home while isolating Git's configuration and variables.
    for key in ["HOME", "USERPROFILE", "XDG_CONFIG_HOME"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let output = command.output().expect("run incremental Cargo fixture");
    let stdout = String::from_utf8(output.stdout).expect("Cargo fixture output is UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "incremental Cargo fixture failed: stdout={stdout} stderr={stderr}"
    );
    assert_eq!(
        stdout.trim(),
        expected,
        "incremental Cargo output: {stderr}"
    );
    let runs = fs::read_to_string(fixture.root.join("version-runs"))
        .expect("read build-script invocation log")
        .lines()
        .count();
    assert_eq!(runs, expected_runs, "incremental Cargo output: {stderr}");
}

#[test]
fn incremental_cargo_build_tracks_tag_creation_packing_moves_and_deletion() {
    let Some(fixture) = GitFixture::new() else {
        return;
    };
    let app = fixture.root.join("crates/app");
    fs::create_dir_all(app.join("src")).expect("create incremental fixture");
    fixture.write(
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/app\"]\nresolver = \"3\"\n",
    );
    fs::write(
        app.join("Cargo.toml"),
        b"[package]\nname = \"version-watch-fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
    )
    .expect("write incremental fixture manifest");
    fs::write(
        app.join("src/main.rs"),
        b"fn main() { println!(\"{}\", env!(\"SLIMENRF_BUILD_DISPLAY_VERSION\")); }\n",
    )
    .expect("write incremental fixture executable");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("build_support/build_version.rs");
    let build_script = format!(
        "#[path = {source:?}]\nmod build_version;\n\
         fn main() {{\n\
         use std::io::Write as _;\n\
         let root = std::path::PathBuf::from(std::env::var_os(\"CARGO_MANIFEST_DIR\").unwrap()).join(\"../..\");\n\
         let mut log = std::fs::OpenOptions::new().create(true).append(true).open(root.join(\"version-runs\")).unwrap();\n\
         writeln!(log, \"run\").unwrap();\n\
         build_version::emit_build_version();\n\
         }}\n"
    );
    fs::write(app.join("build.rs"), build_script).expect("write incremental fixture build script");
    fixture.git(&fixture.root, &["add", "--", "Cargo.toml", "crates/app"]);
    fixture.git(
        &fixture.root,
        &["commit", "--quiet", "-m", "incremental Cargo fixture"],
    );
    let root = &fixture.root;
    let head = fixture.head(root);
    let dev = format!("1.2.3+{}", short(&head));
    let tree = fixture.git(root, &["rev-parse", "HEAD^{tree}"]);
    let alternate = fixture.git(
        root,
        &["commit-tree", &tree, "-p", &head, "-m", "alternate commit"],
    );

    assert_incremental_version(&fixture, &dev, 1);
    assert_incremental_version(&fixture, &dev, 1);

    // Allow coarse filesystem timestamps to advance beyond Cargo's last build.
    let tick = || std::thread::sleep(std::time::Duration::from_millis(1100));
    tick();
    fixture.git(root, &["tag", "-a", "v1.2.3", "-m", "release"]);
    assert_incremental_version(&fixture, "1.2.3", 2);
    tick();
    fixture.git(root, &["pack-refs", "--all", "--prune"]);
    assert_incremental_version(&fixture, "1.2.3", 3);
    tick();
    fixture.git(
        root,
        &["tag", "-f", "-a", "v1.2.3", "-m", "move away", &alternate],
    );
    assert_incremental_version(&fixture, &dev, 4);
    tick();
    fixture.git(
        root,
        &["tag", "-f", "-a", "v1.2.3", "-m", "move back", &head],
    );
    assert_incremental_version(&fixture, "1.2.3", 5);
    tick();
    fixture.git(root, &["tag", "-d", "v1.2.3"]);
    assert_incremental_version(&fixture, &dev, 6);
    tick();
    fixture.git(root, &["tag", "v1.2.3"]);
    assert_incremental_version(&fixture, "1.2.3", 7);
    tick();
    fixture.write(TRACKED, b"incremental dirty change\n");
    assert_incremental_version(&fixture, &format!("{dev}.dirty"), 8);
    tick();
    fixture.git(root, &["checkout", "--", TRACKED]);
    assert_incremental_version(&fixture, "1.2.3", 9);
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

    fixture.git(
        &fixture.root,
        &["tag", "-a", "v1.2.3", "-m", "emitter release"],
    );
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("true"),
        Some(&fallback),
    );
    assert_emitted(&stdout, "1.2.3");
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("false"),
        Some(&fallback),
    );
    assert_emitted(&stdout, &format!("1.2.3+{}.dirty", short(&head)));
    fixture.git(&fixture.root, &["checkout", "--", TRACKED]);
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("false"),
        Some(&fallback),
    );
    assert_emitted(&stdout, "1.2.3");
    fixture.git(&fixture.root, &["tag", "-d", "v1.2.3"]);
    let stdout = emit_in_child(
        &manifest,
        &cwd,
        directory.path(),
        Some("false"),
        Some(&fallback),
    );
    assert_emitted(&stdout, &format!("1.2.3+{}", short(&head)));
}
