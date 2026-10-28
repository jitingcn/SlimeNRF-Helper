//! Prepare release notes from the application manifest and changelog.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

struct Arguments {
    root: PathBuf,
    tag: String,
    repository: String,
    commit: String,
    output: PathBuf,
}

struct ReleaseMetadata {
    version: String,
    notes: String,
}

fn text_argument(value: &OsStr, option: &str) -> Result<String, String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{option} must be UTF-8"))
}

fn parse_arguments(args: &[OsString]) -> Result<Arguments, String> {
    let mut root = crate::workspace_root();
    let mut tag = None;
    let mut repository = None;
    let mut commit = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].to_str().ok_or("Option names must be UTF-8")?;
        let (option, inline) = match argument.split_once('=') {
            Some((option, value)) => (option, Some(OsStr::new(value))),
            None => (argument, None),
        };
        if !matches!(
            option,
            "--root" | "--tag" | "--repository" | "--commit" | "--notes-output"
        ) {
            return Err(format!("Unrecognized argument: {option}"));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                index += 1;
                args.get(index)
                    .ok_or_else(|| format!("Missing value for {option}"))?
                    .as_os_str()
            }
        };
        match option {
            "--root" => root = PathBuf::from(value),
            "--tag" => tag = Some(text_argument(value, option)?),
            "--repository" => repository = Some(text_argument(value, option)?),
            "--commit" => commit = Some(text_argument(value, option)?),
            "--notes-output" => output = Some(PathBuf::from(value)),
            _ => unreachable!("options were validated above"),
        }
        index += 1;
    }
    Ok(Arguments {
        root,
        tag: tag.ok_or("Missing required --tag")?,
        repository: repository.ok_or("Missing required --repository")?,
        commit: commit.ok_or("Missing required --commit")?,
        output: output.ok_or("Missing required --notes-output")?,
    })
}

fn manifest(path: &Path) -> Result<toml::Value, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("Cannot parse {}: {error}", path.display()))
}

fn valid_version(version: &str) -> bool {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\z")
            .expect("valid semantic version regex")
    });
    VERSION.is_match(version)
}

fn application_version(root: &Path) -> Result<String, String> {
    let app = manifest(&root.join("crates/app/Cargo.toml"))?;
    let package = app
        .get("package")
        .and_then(toml::Value::as_table)
        .filter(|package| {
            package.get("name").and_then(toml::Value::as_str) == Some("slimenrf-helper")
        })
        .ok_or("crates/app/Cargo.toml must describe the slimenrf-helper application")?;
    let version = match package.get("version") {
        Some(toml::Value::String(version)) => Some(version.clone()),
        Some(toml::Value::Table(inherited))
            if inherited.len() == 1
                && inherited.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
        {
            let workspace = manifest(&root.join("Cargo.toml"))?;
            workspace
                .get("workspace")
                .and_then(|value| value.get("package"))
                .and_then(|value| value.get("version"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        }
        _ => None,
    };
    version.filter(|value| valid_version(value)).ok_or_else(|| {
        "The application must have a valid literal or inherited Cargo version".into()
    })
}

fn release_heading(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("##")?;
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    rest.trim_start_matches([' ', '\t'])
        .strip_prefix('[')?
        .split_once(']')
}

fn next_section(line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("##")
        && (rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t'))
    {
        return true;
    }
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    if indentation > 3 {
        return false;
    }
    line[indentation..]
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(name, suffix)| !name.is_empty() && suffix.starts_with(':'))
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> u32 {
        bytes[range]
            .iter()
            .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
    };
    let year = number(0..4);
    let month = number(5..7);
    let day = number(8..10);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    year != 0 && day != 0 && day <= days
}

fn changelog_body(text: &str, version: &str) -> Result<String, String> {
    let lines: Vec<_> = text.lines().collect();
    let sections: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            release_heading(line)
                .filter(|(name, _)| *name == version)
                .map(|(_, suffix)| (index, suffix))
        })
        .collect();
    let [(start, suffix)] = sections.as_slice() else {
        return Err(format!(
            "CHANGELOG.md must contain exactly one [{version}] release section"
        ));
    };
    let date = suffix
        .strip_prefix(" - ")
        .filter(|date| {
            date.len() == 10
                && date.as_bytes()[4] == b'-'
                && date.as_bytes()[7] == b'-'
                && date
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        })
        .ok_or_else(|| {
            format!("The [{version}] changelog heading must have a YYYY-MM-DD release date")
        })?;
    if !valid_date(date) {
        return Err(format!("The [{version}] changelog release date is invalid"));
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| next_section(line).then_some(index))
        .unwrap_or(lines.len());
    let body = lines[start + 1..end].join("\n").trim().to_owned();
    if !body
        .lines()
        .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        return Err(format!(
            "The [{version}] changelog release section is empty"
        ));
    }
    Ok(body)
}

fn release_metadata(
    root: &Path,
    tag: &str,
    repository: &str,
    commit: &str,
) -> Result<ReleaseMetadata, String> {
    let version = application_version(root)?;
    if tag != format!("v{version}") {
        return Err(format!("Tag must be exactly v{version}, not {tag:?}"));
    }
    static REPOSITORY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[A-Za-z0-9-]+/[A-Za-z0-9_.-]+\z").expect("valid repository regex")
    });
    let valid_repository = REPOSITORY.is_match(repository);
    if !valid_repository
        || repository
            .split_once('/')
            .is_some_and(|(_, name)| matches!(name, "." | ".."))
    {
        return Err("Repository must be a GitHub OWNER/REPO, without URL or path syntax".into());
    }
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Release commit must be a full lowercase 40-character Git SHA".into());
    }
    if !root.join("CHANGELOG.zh-CN.md").is_file() {
        return Err("The Chinese changelog CHANGELOG.zh-CN.md is missing".into());
    }
    let changelog_path = root.join("CHANGELOG.md");
    let changelog = fs::read_to_string(&changelog_path)
        .map_err(|error| format!("Cannot read {}: {error}", changelog_path.display()))?;
    let body = changelog_body(&changelog, &version)?;
    let notes = format!(
        "{body}\n\n[简体中文](https://github.com/{repository}/blob/{commit}/CHANGELOG.zh-CN.md)\n"
    );
    Ok(ReleaseMetadata { version, notes })
}

fn execute(args: &[OsString]) -> Result<String, String> {
    let arguments = parse_arguments(args)?;
    let root = arguments.root.canonicalize().map_err(|error| {
        format!(
            "Cannot resolve source repository {}: {error}",
            arguments.root.display()
        )
    })?;
    if !arguments.output.is_absolute() {
        return Err(
            "Notes output must be an absolute temporary path outside the source repository".into(),
        );
    }
    let resolved = match arguments.output.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            let parent = arguments
                .output
                .parent()
                .ok_or("Notes output has no parent directory")?;
            let parent = parent.canonicalize().map_err(|error| {
                format!(
                    "Cannot resolve notes output directory {}: {error}",
                    parent.display()
                )
            })?;
            parent.join(
                arguments
                    .output
                    .file_name()
                    .ok_or("Notes output must name a file")?,
            )
        }
    };
    if resolved.starts_with(&root) {
        return Err(
            "Notes output must be an absolute temporary path outside the source repository".into(),
        );
    }
    let metadata = release_metadata(
        &root,
        &arguments.tag,
        &arguments.repository,
        &arguments.commit,
    )?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&arguments.output)
        .map_err(|error| {
            format!(
                "Cannot create notes {}: {error}",
                arguments.output.display()
            )
        })?;
    output
        .write_all(metadata.notes.as_bytes())
        .map_err(|error| format!("Cannot write notes {}: {error}", arguments.output.display()))?;
    Ok(metadata.version)
}

/// Extract commit-pinned prerelease notes and print the application version.
///
/// # Errors
/// Returns an error if arguments, release metadata, or exclusive output creation fail.
pub fn run(args: &[OsString]) -> Result<(), String> {
    if args.iter().any(|argument| {
        argument.as_os_str() == OsStr::new("--help") || argument.as_os_str() == OsStr::new("-h")
    }) {
        println!(
            "Usage: slimenrf-tools prepare-prerelease --tag TAG --repository OWNER/REPO --commit SHA --notes-output ABSOLUTE_PATH"
        );
        return Ok(());
    }
    let version =
        execute(args).map_err(|error| format!("Prerelease preparation failed: {error}"))?;
    println!("{version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::execute;

    const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";
    const BODY: &str =
        "### Added\n\n- A user-facing feature.\n\n### Fixed\n\n- A corrected behavior.";
    const WORKSPACE: &str = "[workspace]\nmembers = [\"crates/app\", \"crates/recording\", \"crates/firmware-image\", \"crates/terminal-core\"]\n[workspace.package]\nversion = \"1.2.3\"\n";
    const APP: &str = "[package]\nname = \"slimenrf-helper\"\nversion.workspace = true\n";

    struct Fixture {
        _temporary: TempDir,
        root: PathBuf,
        output: PathBuf,
        changelog: String,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("create release fixture");
            let root = temporary.path().join("source");
            fs::create_dir(&root).expect("create source fixture");
            let output = temporary.path().join("notes.md");
            let changelog = format!(
                "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- Not yet released.\n\n## [1.2.3] - 2026-10-26\n\n{BODY}\n\n## [1.2.2] - 2026-10-20\n\n### Fixed\n\n- An older change.\n\n[1.2.3]: https://example.invalid/releases/1.2.3\n"
            );
            let fixture = Self {
                _temporary: temporary,
                root,
                output,
                changelog,
            };
            fixture.put("Cargo.toml", WORKSPACE);
            fixture.put("crates/app/Cargo.toml", APP);
            for (name, version) in [
                ("recording", "2.0.0"),
                ("firmware-image", "3.0.0"),
                ("terminal-core", "4.0.0"),
            ] {
                fixture.put(
                    &format!("crates/{name}/Cargo.toml"),
                    &format!("[package]\nname = \"slimenrf-{name}\"\nversion = \"{version}\"\n"),
                );
            }
            fixture.put("CHANGELOG.md", &fixture.changelog);
            fixture.put(
                "CHANGELOG.zh-CN.md",
                "# 更新日志\n\n## [1.2.3] - 2026-10-26\n\n- 用户可见的变更。\n",
            );
            fixture
        }

        fn put(&self, relative: &str, text: &str) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().expect("fixture file parent"))
                .expect("create fixture parent");
            fs::write(path, text).expect("write fixture file");
        }

        fn invoke(&self, replacements: &[(&str, Option<&OsStr>)]) -> Result<String, String> {
            let defaults = [
                ("--root", Some(self.root.as_os_str())),
                ("--tag", Some(OsStr::new("v1.2.3"))),
                ("--repository", Some(OsStr::new("example/project"))),
                ("--commit", Some(OsStr::new(COMMIT))),
                ("--notes-output", Some(self.output.as_os_str())),
            ];
            let mut args = Vec::<OsString>::new();
            for (option, value) in defaults {
                let value = replacements
                    .iter()
                    .find(|(name, _)| *name == option)
                    .map_or(value, |(_, value)| *value);
                if let Some(value) = value {
                    args.push(option.into());
                    args.push(value.into());
                }
            }
            execute(&args)
        }

        fn refused(&self, replacements: &[(&str, Option<&OsStr>)]) {
            let result = self.invoke(replacements);
            assert!(result.is_err(), "expected refusal, got {result:?}");
            assert!(!self.output.exists(), "refusal created output");
        }
    }

    fn read_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn collect(root: &Path, path: &Path, result: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(path).expect("read fixture directory") {
                let path = entry.expect("fixture entry").path();
                if path.is_dir() {
                    collect(root, &path, result);
                } else {
                    result.push((
                        path.strip_prefix(root)
                            .expect("file below fixture")
                            .to_path_buf(),
                        fs::read(&path).expect("read fixture file"),
                    ));
                }
            }
        }
        let mut result = Vec::new();
        collect(root, root, &mut result);
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    #[test]
    fn workspace_version_and_exact_section_produce_commit_pinned_notes() {
        let fixture = Fixture::new();
        let originals = read_files(&fixture.root);
        assert_eq!(fixture.invoke(&[]).expect("prepare release"), "1.2.3");
        let expected = format!(
            "{BODY}\n\n[简体中文](https://github.com/example/project/blob/{COMMIT}/CHANGELOG.zh-CN.md)\n"
        );
        assert_eq!(
            fs::read(&fixture.output).expect("read notes"),
            expected.as_bytes()
        );
        assert_eq!(read_files(&fixture.root), originals, "source files changed");
    }

    #[test]
    fn app_literal_version_takes_precedence_over_workspace_and_members() {
        let fixture = Fixture::new();
        fixture.put(
            "crates/app/Cargo.toml",
            &APP.replace(
                "version.workspace = true",
                "version = \"2.0.0-rc.1+build.7\"",
            ),
        );
        fixture.put(
            "CHANGELOG.md",
            &fixture.changelog.replace("1.2.3", "2.0.0-rc.1+build.7"),
        );
        assert_eq!(
            fixture
                .invoke(&[("--tag", Some(OsStr::new("v2.0.0-rc.1+build.7")))])
                .expect("prepare literal release"),
            "2.0.0-rc.1+build.7"
        );
        assert!(
            fs::read_to_string(&fixture.output)
                .expect("read notes")
                .contains(BODY)
        );
    }

    #[test]
    fn missing_invalid_or_nonapplication_versions_fail_without_output() {
        let fixture = Fixture::new();
        let cases = [
            (
                "Cargo.toml",
                WORKSPACE.replace("[workspace.package]\nversion = \"1.2.3\"\n", ""),
            ),
            ("Cargo.toml", "[workspace.package]\nversion = 7\n".into()),
            ("Cargo.toml", "this is not TOML".into()),
            (
                "crates/app/Cargo.toml",
                APP.replace("version.workspace = true", "version.workspace = false"),
            ),
            (
                "crates/app/Cargo.toml",
                APP.replace("version.workspace = true", "version.workspace = 1"),
            ),
            (
                "crates/app/Cargo.toml",
                APP.replace("version.workspace = true", "version = 123"),
            ),
            (
                "crates/app/Cargo.toml",
                APP.replace("version.workspace = true", ""),
            ),
            (
                "crates/app/Cargo.toml",
                APP.replace(
                    "name = \"slimenrf-helper\"",
                    "name = \"some-other-package\"",
                ),
            ),
            (
                "crates/app/Cargo.toml",
                "[workspace]\nmembers = [\"crates/member\"]\n".into(),
            ),
            ("crates/app/Cargo.toml", "this is not TOML".into()),
        ];
        for (path, manifest) in cases {
            fixture.put("Cargo.toml", WORKSPACE);
            fixture.put("crates/app/Cargo.toml", APP);
            fixture.put(path, &manifest);
            fixture.refused(&[]);
        }
    }

    #[test]
    fn unsafe_or_nonsemantic_versions_cannot_enter_workflow_output() {
        let fixture = Fixture::new();
        for version in [
            "01.2.3",
            "1.2",
            "1.2.3-01",
            "1.2.3/other",
            "1.2.3\nversion=evil",
            "1.2.3+",
            "1.2.3 ",
        ] {
            let escaped = version.replace('\n', "\\n");
            fixture.put(
                "crates/app/Cargo.toml",
                &APP.replace(
                    "version.workspace = true",
                    &format!("version = \"{escaped}\""),
                ),
            );
            fixture.refused(&[("--tag", Some(OsStr::new(&format!("v{version}"))))]);
        }
    }

    #[test]
    fn tag_must_be_exactly_the_application_version() {
        let fixture = Fixture::new();
        for tag in [
            "1.2.3",
            "v1.2",
            "v1.2.30",
            "v1.2.3-rc.1",
            "v1.2.3\n",
            "refs/tags/v1.2.3",
            "v1.2.3/other",
        ] {
            fixture.refused(&[("--tag", Some(OsStr::new(tag)))]);
        }
    }

    #[test]
    fn final_section_stops_before_reference_definitions() {
        let fixture = Fixture::new();
        fixture.put("CHANGELOG.md", &format!("# Changelog\n\n## [1.2.3] - 2024-02-29\n\n{BODY}\n\n[Unreleased]: https://example.invalid/compare\n[1.2.3]: https://example.invalid/release\n"));
        fixture.invoke(&[]).expect("prepare final section");
        let notes = fs::read_to_string(&fixture.output).expect("read notes");
        assert!(notes.starts_with(&format!("{BODY}\n\n")));
        assert!(!notes.contains("example.invalid"));
        assert!(!notes.contains("## [1.2.3]"));
    }

    #[test]
    fn missing_duplicate_undated_invalid_or_empty_sections_are_rejected() {
        let fixture = Fixture::new();
        let cases = [
            fixture.changelog.replace("[1.2.3]", "[1.2.4]"),
            format!("{}\n## [1.2.3] - 2026-10-26\n\n{BODY}\n", fixture.changelog),
            format!("{}\n## [1.2.3]\n\n{BODY}\n", fixture.changelog),
            fixture
                .changelog
                .replace("## [1.2.3] - 2026-10-26", "## [1.2.3]"),
            fixture.changelog.replace("2026-10-26", "2026-02-29"),
            fixture.changelog.replace("2026-10-26", "2026-2-9"),
            fixture.changelog.replace("2026-10-26", "2026-10-26 extra"),
            fixture.changelog.replace(BODY, ""),
            fixture.changelog.replace(BODY, "### Added\n\n### Fixed"),
        ];
        for changelog in cases {
            fixture.put("CHANGELOG.md", &changelog);
            fixture.refused(&[]);
        }
    }

    #[test]
    fn unsafe_repository_and_nonpinned_commit_are_rejected() {
        let fixture = Fixture::new();
        for repository in [
            "https://github.com/example/project",
            "example/project/extra",
            "example/..",
            "example/project?ref=main",
            "example/project\n",
            "example/project#fragment",
        ] {
            fixture.refused(&[("--repository", Some(OsStr::new(repository)))]);
        }
        let commits = [
            "main".to_owned(),
            COMMIT[..12].to_owned(),
            "g".repeat(40),
            COMMIT.to_uppercase(),
            format!("{COMMIT}/other"),
            format!("{COMMIT}\n"),
        ];
        for commit in commits {
            fixture.refused(&[("--commit", Some(OsStr::new(&commit)))]);
        }
    }

    #[test]
    fn missing_chinese_changelog_is_not_published_as_a_broken_link() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.root.join("CHANGELOG.zh-CN.md")).expect("remove Chinese changelog");
        fixture.refused(&[]);
    }

    #[test]
    fn explicit_absolute_external_output_is_required() {
        let fixture = Fixture::new();
        fixture.refused(&[("--notes-output", None)]);
        let paths = [
            PathBuf::from("notes.md"),
            fixture.root.join("generated.md"),
            fixture.root.join("..").join("source").join("generated.md"),
            fixture.root.join("Cargo.toml"),
            fixture.root.join("crates/app/Cargo.toml"),
            fixture.root.join("CHANGELOG.md"),
        ];
        for destination in paths {
            fixture.refused(&[("--notes-output", Some(destination.as_os_str()))]);
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("Cargo.toml")).expect("workspace"),
            WORKSPACE
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("crates/app/Cargo.toml")).expect("app"),
            APP
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("CHANGELOG.md")).expect("changelog"),
            fixture.changelog
        );
        assert!(!fixture.root.join("generated.md").exists());
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let fixture = Fixture::new();
        fs::write(&fixture.output, b"previous notes\r\n").expect("create previous output");
        assert!(fixture.invoke(&[]).is_err());
        assert_eq!(
            fs::read(&fixture.output).expect("read previous output"),
            b"previous notes\r\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_refuses_symlinks_and_paths_resolving_inside_source() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let alias = fixture._temporary.path().join("source-alias");
        symlink(&fixture.root, &alias).expect("create source directory symlink");
        let destination = alias.join("generated.md");
        fixture.refused(&[("--notes-output", Some(destination.as_os_str()))]);
        assert!(!fixture.root.join("generated.md").exists());

        let changelog = fixture.root.join("CHANGELOG.md");
        symlink(&changelog, &fixture.output).expect("create output symlink");
        assert!(fixture.invoke(&[]).is_err());
        assert_eq!(
            fs::read_to_string(changelog).expect("read changelog"),
            fixture.changelog
        );
    }
}
