#!/usr/bin/env python3
"""Prepare changelog-based prerelease notes with Python 3.11+, without Git or network I/O."""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import date
from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_ID = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
VERSION = re.compile(
    rf"{NUMBER}\.{NUMBER}\.{NUMBER}"
    rf"(?:-{PRERELEASE_ID}(?:\.{PRERELEASE_ID})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)


@dataclass(frozen=True)
class ReleaseMetadata:
    version: str
    notes: str


def application_version(root: Path) -> str:
    with (root / "crates/app/Cargo.toml").open("rb") as source:
        manifest = tomllib.load(source)
    package = manifest.get("package")
    if not isinstance(package, dict) or package.get("name") != "slimenrf-helper":
        raise ValueError("crates/app/Cargo.toml must describe the slimenrf-helper application")
    version = package.get("version")
    if isinstance(version, dict) and set(version) == {"workspace"} and version["workspace"] is True:
        with (root / "Cargo.toml").open("rb") as source:
            workspace_manifest = tomllib.load(source)
        workspace = workspace_manifest.get("workspace", {})
        workspace_package = workspace.get("package", {}) if isinstance(workspace, dict) else {}
        version = workspace_package.get("version") if isinstance(workspace_package, dict) else None
    if not isinstance(version, str) or VERSION.fullmatch(version) is None:
        raise ValueError("The application must have a valid literal or inherited Cargo version")
    return version


def changelog_body(text: str, version: str) -> str:
    lines = text.splitlines()
    sections = []
    for index, line in enumerate(lines):
        heading = re.fullmatch(r"##[ \t]+\[([^\]]+)\](.*)", line)
        if heading and heading[1] == version:
            sections.append((index, heading[2]))
    if len(sections) != 1:
        raise ValueError(f"CHANGELOG.md must contain exactly one [{version}] release section")
    start, suffix = sections[0]
    dated = re.fullmatch(r" - ([0-9]{4}-[0-9]{2}-[0-9]{2})", suffix)
    if dated is None:
        raise ValueError(f"The [{version}] changelog heading must have a YYYY-MM-DD release date")
    try:
        date.fromisoformat(dated[1])
    except ValueError as error:
        raise ValueError(f"The [{version}] changelog release date is invalid") from error
    end = len(lines)
    for index in range(start + 1, len(lines)):
        if re.match(r"^##(?:[ \t]|$)", lines[index]) or re.match(r"^ {0,3}\[[^\]]+\]:", lines[index]):
            end = index
            break
    body = "\n".join(lines[start + 1:end]).strip()
    if not any(line.strip() and not line.lstrip().startswith("#") for line in body.splitlines()):
        raise ValueError(f"The [{version}] changelog release section is empty")
    return body


def release_metadata(root: Path, tag: str, repository: str, commit: str) -> ReleaseMetadata:
    version = application_version(root)
    if tag != f"v{version}":
        raise ValueError(f"Tag must be exactly v{version}, not {tag!r}")
    if (re.fullmatch(r"[A-Za-z0-9-]+/[A-Za-z0-9_.-]+", repository) is None
            or repository.split("/")[1] in {".", ".."}):
        raise ValueError("Repository must be a GitHub OWNER/REPO, without URL or path syntax")
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise ValueError("Release commit must be a full lowercase 40-character Git SHA")
    if not (root / "CHANGELOG.zh-CN.md").is_file():
        raise ValueError("The Chinese changelog CHANGELOG.zh-CN.md is missing")
    body = changelog_body((root / "CHANGELOG.md").read_text(encoding="utf-8"), version)
    chinese = f"https://github.com/{repository}/blob/{commit}/CHANGELOG.zh-CN.md"
    return ReleaseMetadata(version, f"{body}\n\n[简体中文]({chinese})\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repository", required=True, help="GitHub OWNER/REPO")
    parser.add_argument("--commit", required=True, help="Full release commit SHA")
    parser.add_argument("--notes-output", type=Path, required=True,
                        help="New absolute temporary output path outside the source repository")
    args = parser.parse_args(argv)
    try:
        root = args.root.resolve()
        destination = args.notes_output
        if not destination.is_absolute() or destination.resolve().is_relative_to(root):
            raise ValueError("Notes output must be an absolute temporary path outside the source repository")
        metadata = release_metadata(root, args.tag, args.repository, args.commit)
        # Exclusive creation also refuses symlinks and never overwrites a prior run's notes.
        with destination.open("x", encoding="utf-8", newline="\n") as output:
            output.write(metadata.notes)
        print(metadata.version)
    except (ValueError, OSError, UnicodeError) as error:
        print(f"Prerelease preparation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
