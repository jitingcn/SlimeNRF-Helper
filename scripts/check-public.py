#!/usr/bin/env python3
"""Review Git publication candidates without walking the workspace.

Findings are JSON objects containing only path, line and category. Binary assets,
undecodable text and oversized inputs require separate review and fail the check.
This is a bounded pattern check, not a secret, hardware-identity or release audit.
"""
from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass, field
import html
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import unicodedata
from urllib.parse import unquote, urlsplit


MAX_TEXT_BYTES = 8 * 1024 * 1024
MAINTAINED = (
    "README.md", "CHANGELOG.md", "docs/USER_GUIDE.md", "docs/DEVELOPMENT.md",
    "docs/ARCHITECTURE.md", "docs/PROTOCOLS.md",
)
FORBIDDEN_PARTS = frozenset({
    ".git", ".local", "target", "build", "dist", "captures", "recordings",
    "firmwares", ".ssh", ".aws", ".azure", ".gnupg", ".venv", "__pycache__",
    ".pytest_cache", ".ruff_cache", ".claude", ".codex", ".omp", ".idea",
    "credentials", "secrets",
})
FORBIDDEN_SUFFIXES = frozenset({
    ".hidlog", ".slimecap", ".log", ".tmp", ".temp", ".key", ".pem", ".p12",
    ".pfx", ".jks", ".keystore", ".pyc", ".pyo", ".swp", ".swo",
})
BINARY_SUFFIXES = frozenset({
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".webp", ".bmp", ".avif",
    ".exe", ".dll", ".so", ".dylib", ".a", ".lib", ".o", ".obj", ".bin", ".uf2",
    ".zip", ".gz", ".xz", ".bz2", ".7z", ".rar", ".tar", ".pdf",
    ".ttf", ".otf", ".woff", ".woff2", ".wav", ".mp3", ".mp4", ".sqlite", ".db",
})
PUBLIC_LOG = "src/rssi_scan/fixtures/receiver_scan.log"
PAIR_PATHS = {path for english in MAINTAINED for path in (
    english, english.removesuffix(".md") + ".zh-CN.md",
)}


class BoundaryError(Exception):
    def __init__(self, category: str):
        super().__init__(category)
        self.category = category


@dataclass(frozen=True, order=True)
class Finding:
    path: str
    line: int
    category: str


@dataclass(frozen=True)
class Inventory:
    tracked: tuple[str, ...]
    untracked: tuple[str, ...]
    ignored_tracked: tuple[str, ...]

    @property
    def candidates(self) -> tuple[str, ...]:
        return tuple(sorted(set(self.tracked) | set(self.untracked)))


@dataclass
class Report:
    inventory: Inventory
    findings: set[Finding] = field(default_factory=set)
    counts: Counter = field(default_factory=Counter)


@dataclass(frozen=True)
class Document:
    links: tuple[tuple[int, str], ...]
    anchors: frozenset[str]
    explicit_anchors: frozenset[str]


def valid_relative(path: str) -> bool:
    return bool(path) and not path.startswith("/") and all(
        part not in {"", ".", ".."} and not any(char in part for char in "\\:\x00")
        and not part.endswith((" ", "."))
        and not re.fullmatch(r"(?:con|prn|aux|nul|com[1-9]|lpt[1-9])", part.split(".")[0], re.I)
        for part in path.split("/")
    )


def forbidden(path: str) -> bool:
    parts = tuple(part.casefold() for part in path.split("/"))
    name = parts[-1]
    return (
        any(part in FORBIDDEN_PARTS for part in parts)
        or (".vscode" in parts and path not in {
            ".vscode", ".vscode/settings.json", ".vscode/extensions.json",
        })
        or (name.startswith(".env") and name != ".env.example")
        or name in {"slimenrf-helper.local.yaml", "slimenrf-helper.local.yml",
                    "preferences.json", "device-registry.json", "remembered-devices.json",
                    "custom-shortcuts.json", "credentials", "credentials.toml", "credentials.json",
                    "credentials.yml", "credentials.yaml", ".git-credentials",
                    "id_rsa", "id_ed25519", "id_ecdsa", "id_dsa",
                    ".netrc", ".npmrc", ".pypirc", "thumbs.db", ".ds_store"}
        or name.endswith((".local.yaml", ".local.yml"))
        or (PurePosixPath(name).suffix in FORBIDDEN_SUFFIXES and path != PUBLIC_LOG)
    )


def checked_stat(path: Path) -> os.stat_result:
    try:
        info = path.lstat()
    except FileNotFoundError as error:
        raise BoundaryError("missing-path") from error
    except OSError as error:
        raise BoundaryError("unreadable-metadata") from error
    if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        raise BoundaryError("symlink-or-reparse-point")
    return info


def check_absolute(path: Path) -> None:
    for component in (*reversed(path.parents), path):
        info = checked_stat(component)
        if component != path and not stat.S_ISDIR(info.st_mode):
            raise BoundaryError("non-directory-component")


def candidate_stat(root: Path, relative: str) -> os.stat_result:
    if not valid_relative(relative):
        raise BoundaryError("invalid-candidate-path")
    if forbidden(relative):
        raise BoundaryError("forbidden-publication-path")
    current = root
    for index, part in enumerate(relative.split("/")):
        current = current / part
        info = checked_stat(current)
        if index < len(relative.split("/")) - 1 and not stat.S_ISDIR(info.st_mode):
            raise BoundaryError("non-directory-component")
    return info


def read_public_bytes(root: Path, relative: str) -> bytes:
    before = candidate_stat(root, relative)
    if not stat.S_ISREG(before.st_mode):
        raise BoundaryError("non-regular-candidate")
    if PurePosixPath(relative).suffix.casefold() in BINARY_SUFFIXES:
        raise BoundaryError("unreviewed-binary")
    if before.st_size > MAX_TEXT_BYTES:
        raise BoundaryError("unreviewed-oversized-file")
    try:
        descriptor = os.open(root / relative, os.O_RDONLY | getattr(os, "O_BINARY", 0)
                             | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            after = candidate_stat(root, relative)
            identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
            if not stat.S_ISREG(opened.st_mode) or identity(before) != identity(opened) or identity(before) != identity(after):
                raise BoundaryError("candidate-changed-during-read")
            data = stream.read(MAX_TEXT_BYTES + 1)
            if identity(opened) != identity(os.fstat(stream.fileno())):
                raise BoundaryError("candidate-changed-during-read")
    except OSError as error:
        raise BoundaryError("unreadable-candidate") from error
    if len(data) > MAX_TEXT_BYTES:
        raise BoundaryError("unreviewed-oversized-file")
    return data


def git(root: Path, *arguments: str) -> bytes:
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    environment.update({"GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0",
                        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})
    try:
        result = subprocess.run(
            ["git", "-c", "core.fsmonitor=false", "-C", str(root), *arguments],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment, check=False,
        )
    except OSError as error:
        raise BoundaryError("git-unavailable") from error
    if result.returncode:
        raise BoundaryError("git-command-failed")
    return result.stdout


def application_root(requested: str | None) -> Path:
    script = Path(os.path.abspath(__file__))
    root = script.parent.parent
    check_absolute(script)
    if script.name != "check-public.py" or script.parent.name != "scripts":
        raise BoundaryError("invalid-script-location")
    if requested is not None and os.path.normcase(os.path.abspath(requested)) != os.path.normcase(str(root)):
        raise BoundaryError("root-does-not-match-application")
    check_absolute(root)
    metadata = checked_stat(root / ".git")
    if not (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)):
        raise BoundaryError("invalid-git-metadata")
    top = os.fsdecode(git(root, "rev-parse", "--show-toplevel")).rstrip("\r\n")
    if os.path.normcase(os.path.abspath(top)) != os.path.normcase(str(root)):
        raise BoundaryError("git-root-does-not-match-application")
    for path, package in (("Cargo.toml", "slimenrf-helper"),
                          ("crates/recording/Cargo.toml", "slimenrf-recording")):
        try:
            text = read_public_bytes(root, path).decode("utf-8-sig")
        except UnicodeError as error:
            raise BoundaryError("invalid-application-sentinel") from error
        section = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", text)
        if section is None or not re.search(r'^name\s*=\s*"' + re.escape(package) + r'"\s*$', section[1], re.M):
            raise BoundaryError("invalid-application-sentinel")
    if not stat.S_ISREG(candidate_stat(root, "src/main.rs").st_mode):
        raise BoundaryError("invalid-application-sentinel")
    return root


def inventory(root: Path) -> Inventory:
    groups = []
    for arguments in (("--cached",), ("--others", "--exclude-standard"),
                      ("--cached", "--ignored", "--exclude-standard")):
        raw = git(root, "ls-files", *arguments, "-z", "--")
        if raw and not raw.endswith(b"\0"):
            raise BoundaryError("invalid-git-inventory")
        paths = tuple(sorted(set(os.fsdecode(value) for value in raw.split(b"\0") if value)))
        if any(not valid_relative(path) for path in paths):
            raise BoundaryError("invalid-git-inventory")
        groups.append(paths)
    return Inventory(*groups)


PATTERNS = (
    ("private-absolute-path", re.compile(
        r"(?<![\w])(?:[A-Za-z]:[\\/]+[^\s\"'<>),;]*|"
        r"/(?:home|Users|root|workspaces?|projects)/[^\s\"'<>),;]+|"
        r"/mnt/[a-z]/[^\s\"'<>),;]+|"
        r"(?<![:/])//[\w.-]+/[^\s\"'<>),;]+|"
        r"\\\\(?![.?]\\)[\w.-]+\\[^\s\"'<>),;]+)", re.I)),
    ("private-key", re.compile(r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----")),
    ("credential-token", re.compile(
        r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
        r"glpat-[A-Za-z0-9_-]{20,}|AKIA[A-Z0-9]{16}|"
        r"xox[baprs]-[A-Za-z0-9-]{15,}|sk-(?:proj-)?[A-Za-z0-9_-]{20,})\b")),
    ("authorization-value", re.compile(
        r"\bBearer\s+[A-Za-z0-9][A-Za-z0-9._~+/-]*={0,2}|"
        r"\bAuthorization:\s*Basic\s+[A-Za-z0-9+/]+={0,2}", re.I)),
    ("credential-assignment", re.compile(
        r"\b(?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|"
        r"client[_-]?secret|secret[_-]?key|token|secret|GH_TOKEN|GITHUB_TOKEN)\b[\"']?\s*[:=]\s*"
        r"[\"'][^\"'\r\n]+[\"']", re.I)),
    ("credential-assignment", re.compile(
        r"^\s*(?:export\s+)?(?:GH_TOKEN|GITHUB_TOKEN|API_KEY|ACCESS_TOKEN|CLIENT_SECRET|PASSWORD)"
        r"\s*=\s*[A-Za-z0-9_+/-]{8,}", re.I)),
)
URL = re.compile(r"\b(?:https?|ftp|ssh)://[^\s\"'<>\\)]+", re.I)
SIGNED_QUERY = re.compile(r"(?:[?&]|\b)(?:sig|signature|access_token|token|api_key|password|client_secret|X-Amz-Signature|X-Goog-Signature)=[A-Za-z0-9%_+./~-]+", re.I)

# Exact synthetic credential values reviewed in HTTP rejection/redaction tests.
# Neither an entire file nor an arbitrary replacement value is exempt.
REVIEWED = {
    ("src/firmware_source/http_tests.rs", "authorization-value"): {"bearer " + "fixture-secret"},
    ("src/firmware_source/tests.rs", "authorization-value"): {"Bearer " + "ghp_private_token"},
    ("src/firmware_source/http_tests.rs", "authenticated-url"): {
        "https://" + "user:password@api.github.com/credentials",
    },
    ("src/firmware_source/tests.rs", "authenticated-url"): {
        "https://" + "user:password@nightly.link/private" + "?sig" + "=private-signature",
        "https://" + "user:secret@api.github.com/a",
    },
    ("src/firmware_source/http_tests.rs", "signed-credential"): {"?sig" + "=fixture-signature"},
    ("src/firmware_source/tests.rs", "signed-credential"): {
        "?sig" + "=private-signature", "sig" + "=detached-signature",
    },
}

# Absolute paths require complete reviewed lines, never regex-token prefixes.
REVIEWED_LINES = {
    ("src/bundled_resources/tests.rs", "private-absolute-path"): {
        "ddf69c2c4e85fcd02698314d3fc0fe7f142d9cccb30793083674970efa693e39",
    },
    ("src/firmware_source/tests.rs", "private-absolute-path"): {
        "2c8c73153f7a4a4f442695e8d039ba3db64a8ca0ee68b4946714ca359366acef",
        "5a59ce6bfe62671241ec414aa91729988dc2c38e3deaa90c8bbd9dbbac91fcab",
    },
    ("src/uf2/linux.rs", "private-absolute-path"): {
        "d1320baad7a8a5d72539d347aa0c66a2f82e18916fe0935e85347217d3872b07",
        "66e66d60df53e41798e5f223537f737d7e3ccc4378ea45a19ba18a108bbacb3d",
        "68b8c9de1d9714feda4e7191d4963d5c366f92b79723e6a6b621f8f9427b8cce",
    },
    ("src/ui/flash/tests/images.rs", "private-absolute-path"): {
        "5a316d00bdb4939ead3667c550edfe23ea47b6a3f42380a1842719ca7dea8227",
        "6f072659367d7cedf4c746c972e1dc0cf05acbaa6e612c1ba32cfff1ab0fc6d6",
        "290386f05d64744dc7c21888da104676f03ed56cc38b0e0111f4775b66bb9651",
        "e6cfcb1a9a1e65f6c26b52447714f9d93e24c4c372f6741d8ab2575b396518b1",
        "2c7f400ef1511ab9ee560371c101c2c584b6833a3d6f58bf55d0b330a205ed26",
        "3f4dc059b78e46a964a260700d367ee42051ee52daea8b471596436b8c49ef23",
        "a21097572eae3a8d2e93d3d2a1fe81de0ea8ae2aad6d8eea50e697924b1f5dec",
        "85ffe7693debced9382c7f3512212b34fc3b35465db843d8d5c5b45f6e97bee9",
        "c705c6885bc0b74bb96080749820534dabcae575d760f56623728559645295cd",
        "c16b4d348f5ce69d1e0c567ac1449062547c13b02e4c4e5aaef98963be627656",
    },
    (".github/workflows/ci.yml", "private-absolute-path"): {
        "f0b25a20b3b15d855b6175d6cf904aac0e87838e9b83db6935cef53af088680d",
    },
    ("locales/en.yml", "credential-assignment"): {
        "7df7e1c2fb26008af0010be924ca39e9f106e781fbb69283ac51bfb5975314be",
    },
    ("locales/zh-CN.yml", "credential-assignment"): {
        "290aa464dc17edd4afb1ece58c910c83899a4294afea89297de35ac709354218",
    },
    ("scripts/test_generate_notices.py", "private-absolute-path"): {
        "af0911f59fafddcbacd6cb912c8887ffbf40f583839b22c09a437ea216583ced",
    },
}


def scan_text(path: str, text: str) -> set[Finding]:
    findings = set()
    for number, line in enumerate(text.splitlines(), 1):
        matches = [(category, match[0]) for category, pattern in PATTERNS
                   for match in pattern.finditer(line)]
        matches.extend(("signed-credential", match[0]) for match in SIGNED_QUERY.finditer(line))
        for match in URL.finditer(line):
            try:
                url = urlsplit(match[0])
            except ValueError:
                matches.append(("malformed-url", match[0]))
                continue
            if url.username is not None or url.password is not None:
                matches.append(("authenticated-url", match[0]))
        for category, value in matches:
            reviewed_line = hashlib.sha256(line.strip().encode("utf-8")).hexdigest() in REVIEWED_LINES.get((path, category), set())
            if value not in REVIEWED.get((path, category), set()) and not reviewed_line:
                findings.add(Finding(path, number, category))
    return findings


def visible_markdown(text: str) -> list[str]:
    lines = []
    fence = None
    for line in text.splitlines():
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if marker:
            token = marker[1]
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            lines.append("")
        elif fence is not None or line.startswith(("    ", "\t")):
            lines.append("")
        else:
            lines.append(line)
    return lines


def markdown_document(text: str) -> Document:
    lines = visible_markdown(text)
    links = []
    definitions = {}
    anchors = set()
    explicit = set()
    for number, line in enumerate(lines, 1):
        visible = re.sub(r"(`+).*?\1", "", line)
        for match in re.finditer(r"<[^>]+\b(?:id|name)\s*=\s*[\"']([^\"']+)[\"']", visible, re.I):
            explicit.add(html.unescape(match[1]))
        heading = re.match(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        title = heading[1] if heading else None
        if number < len(lines) and re.fullmatch(r" {0,3}(?:=+|-+)\s*", lines[number]) and line.strip():
            title = line.strip()
        if title is not None:
            title = re.sub(r"!?\[([^]]*)\]\([^)]*\)", r"\1", title)
            title = html.unescape(re.sub(r"<[^>]*>", "", title)).lower()
            slug = "".join(char for char in title if char in " -_" or unicodedata.category(char)[0] in "LN")
            slug = slug.replace(" ", "-")
            anchor, suffix = slug, 0
            while anchor in anchors:
                suffix += 1
                anchor = f"{slug}-{suffix}"
            anchors.add(anchor)
        line = visible
        definition = re.match(r"^ {0,3}\[([^]]+)\]:\s*(<[^>]*>|\S+)", line)
        if definition:
            definitions[definition[1].casefold()] = definition[2].strip("<>")
            links.append((number, definition[2].strip("<>")))
            continue
        for match in re.finditer(r"\]\(\s*(<[^>]*>|(?:[^\s()\\]|\\.|\([^()]*\))*)", line):
            destination = match[1].strip("<>")
            links.append((number, re.sub(r"\\([\\() ])", r"\1", destination)))
        for match in re.finditer(r"<[^>]+\b(?:href|src)\s*=\s*[\"']([^\"']+)[\"']", line, re.I):
            links.append((number, html.unescape(match[1])))
    for number, line in enumerate(lines, 1):
        line = re.sub(r"(`+).*?\1", "", line)
        if re.match(r"^ {0,3}\[[^]]+\]:", line):
            continue
        for match in re.finditer(r"\[([^]]+)\](?:\[([^]]*)\])?(?![\[(])", line):
            key = (match[2] or match[1]).casefold()
            if key in definitions:
                links.append((number, definitions[key]))
            elif match[2] is not None:
                links.append((number, "unresolved-reference:"))
    return Document(tuple(links), frozenset(anchors | explicit), frozenset(explicit))


def local_target(root: Path, source: str, destination: str) -> tuple[str, str] | None:
    try:
        destination = html.unescape(destination)
        url = urlsplit(destination)
        if url.scheme or url.netloc:
            if url.scheme and url.scheme.casefold() not in {"http", "https", "mailto", "ftp", "ssh"}:
                raise BoundaryError("markdown-unsafe-target")
            return None
        path = unquote(url.path, errors="strict")
        anchor = unquote(url.fragment, errors="strict")
    except (ValueError, UnicodeError) as error:
        raise BoundaryError("markdown-malformed-target") from error
    if path.startswith("/") or "\\" in path or "\x00" in path:
        raise BoundaryError("markdown-unsafe-target")
    if not path:
        return source, anchor
    parts = source.split("/")[:-1]
    for part in path.split("/"):
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise BoundaryError("markdown-target-outside-root")
            parts.pop()
        else:
            parts.append(part)
            candidate_stat(root, "/".join(parts))
    target = "/".join(parts)
    if not target:
        return ".", anchor
    candidate_stat(root, target)
    return target, anchor


def check_documents(root: Path, texts: dict[str, str], report: Report) -> None:
    documents = {path: markdown_document(text) for path, text in texts.items() if path.lower().endswith(".md")}
    published_directories = {
        parent.as_posix() for candidate in report.inventory.candidates
        for parent in PurePosixPath(candidate).parents
    }
    targets = {}
    for path, document in documents.items():
        targets[path] = []
        for number, destination in document.links:
            try:
                target = local_target(root, path, destination)
                if target is None:
                    continue
                relative, anchor = target
                targets[path].append((number, relative, anchor))
                if relative != ".":
                    info = candidate_stat(root, relative)
                    if stat.S_ISDIR(info.st_mode) and relative not in published_directories:
                        raise BoundaryError("markdown-target-not-published")
                    if stat.S_ISREG(info.st_mode) and relative not in texts:
                        raise BoundaryError("markdown-target-not-reviewed-text")
                if anchor and (relative not in documents or anchor not in documents[relative].anchors):
                    raise BoundaryError("markdown-missing-anchor")
                if path in PAIR_PATHS and relative in PAIR_PATHS:
                    counterpart = path.removesuffix(".zh-CN.md") + ".md" if path.endswith(".zh-CN.md") else path.removesuffix(".md") + ".zh-CN.md"
                    if relative != counterpart and path.endswith(".zh-CN.md") != relative.endswith(".zh-CN.md"):
                        raise BoundaryError("markdown-language-navigation")
            except BoundaryError as error:
                report.findings.add(Finding(path, number, error.category))
    for english in MAINTAINED:
        chinese = english.removesuffix(".md") + ".zh-CN.md"
        for path, counterpart in ((english, chinese), (chinese, english)):
            if path not in documents:
                report.findings.add(Finding(path, 0, "missing-language-document"))
            elif not any(number <= 5 and target == counterpart and not anchor
                         for number, target, anchor in targets[path]):
                report.findings.add(Finding(path, 1, "missing-reciprocal-language-link"))
        if english in documents and chinese in documents and documents[english].explicit_anchors != documents[chinese].explicit_anchors:
            report.findings.add(Finding(english, 0, "language-explicit-anchor-mismatch"))
            report.findings.add(Finding(chinese, 0, "language-explicit-anchor-mismatch"))


def audit(requested: str | None = None) -> Report:
    root = application_root(requested)
    report = Report(inventory(root))
    texts = {}
    for path in report.inventory.candidates:
        try:
            data = read_public_bytes(root, path)
            if b"\0" in data or any(value < 32 and value not in {9, 10, 12, 13} for value in data):
                raise BoundaryError("unreviewed-binary")
            try:
                text = data.decode("utf-8-sig")
            except UnicodeError as error:
                raise BoundaryError("unreviewed-undecodable") from error
            texts[path] = text
            report.findings.update(scan_text(path, text))
            report.counts["scanned_text"] += 1
        except BoundaryError as error:
            report.findings.add(Finding(path, 0, error.category))
            report.counts["unreviewed_or_forbidden"] += 1
    check_documents(root, texts, report)
    return report


class Arguments(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise BoundaryError("invalid-arguments")


def main(arguments: list[str] | None = None) -> int:
    parser = Arguments(description=__doc__)
    parser.add_argument("--root", help="Require the script's application root to match this directory")
    try:
        options = parser.parse_args(arguments)
        report = audit(options.root)
    except BoundaryError as error:
        print(json.dumps({"path": ".", "line": 0, "category": error.category}))
        return 2
    for finding in sorted(report.findings):
        print(json.dumps({"path": finding.path, "line": finding.line, "category": finding.category}, ensure_ascii=True))
    print(json.dumps({"inventory": {
        "tracked": len(report.inventory.tracked),
        "nonignored_untracked": len(report.inventory.untracked),
        "ignored_tracked": len(report.inventory.ignored_tracked),
        "candidates": len(report.inventory.candidates),
        **report.counts,
        "findings": len(report.findings),
    }}, sort_keys=True))
    return int(bool(report.findings))


if __name__ == "__main__":
    sys.exit(main())
