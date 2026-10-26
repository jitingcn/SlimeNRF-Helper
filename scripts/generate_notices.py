#!/usr/bin/env python3
"""Render reviewed, frozen notice inputs without Cargo, a registry cache, or a network.

Requires Python 3.11 or newer. Run with --check before publishing. To review an
update, capture both locked target graphs with cargo metadata --locked --offline
--format-version 1 --filter-platform TARGET, following normal and build edges
from slimenrf-helper, excluding dev edges. A build or proc-macro edge marks a
compile-tool path. Preserve the conservative union, including feature unification.
Update notices-inputs.json only after reviewing every added package and source.
Each reviewed source contains base64 original bytes keyed by their SHA-256.
Keep existing source bytes, add reviewed exact sources, and freeze all new hashes.
Neither regeneration nor application compilation fetches license text.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
INPUT = "scripts/notices-inputs.json"
OUTPUT = "THIRD_PARTY_NOTICES.txt"
VENDOR_LICENSES = (
    "crates/recording/vendor/vqf/LICENSE",
    "crates/recording/vendor/vqf/LICENSE-SlimeVR",
    "crates/recording/vendor/vqf/cmsis/LICENSE",
)


class NoticeError(ValueError):
    """A reviewed input is absent, stale, or inconsistent."""


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def relative_file(root: Path, name: str) -> Path:
    candidate = Path(name)
    if candidate.is_absolute() or ".." in candidate.parts or "\\" in name or ":" in name:
        raise NoticeError(f"Nonportable input path: {name}")
    return root / candidate


def read_checked(root: Path, name: str, expected: str) -> bytes:
    try:
        data = relative_file(root, name).read_bytes()
    except OSError as error:
        raise NoticeError(f"Missing input {name}: {error.strerror}") from error
    if digest(data) != expected:
        raise NoticeError(f"Stale input hash: {name}. Review and freeze the new input first.")
    return data


def load_inputs(root: Path) -> tuple[dict, dict[str, bytes]]:
    try:
        manifest = json.loads(relative_file(root, INPUT).read_bytes())
    except (OSError, ValueError) as error:
        raise NoticeError(f"Cannot read {INPUT}: {error}") from error
    if manifest.get("schema") != 1:
        raise NoticeError("Unsupported notices input schema")
    required = {"Cargo.toml", "Cargo.lock", "crates/recording/Cargo.toml",
                "LICENSE-MIT", "LICENSE-APACHE", *VENDOR_LICENSES}
    if not required <= manifest["input_sha256"].keys():
        raise NoticeError("Missing mandatory application or vendor license input")
    for name, expected in manifest["input_sha256"].items():
        read_checked(root, name, expected)
    try:
        sources = {key: base64.b64decode(value["base64"], validate=True)
                   for key, value in manifest["sources"].items()}
    except ValueError as error:
        raise NoticeError(f"Invalid base64 notice source: {error}") from error
    for key, data in sources.items():
        source = manifest["sources"][key]
        if digest(data) != key or source["sha256"] != key or len(data) != source["bytes"]:
            raise NoticeError(f"Notice source hash or length mismatch: {key}")
        encoding = source["encoding"]
        if encoding not in ("utf-8", "windows-1252"):
            raise NoticeError(f"Unreviewed source encoding: {encoding}")
        try:
            rendered = data.decode(encoding).encode("utf-8")
        except UnicodeError as error:
            raise NoticeError(f"Invalid {encoding} notice source: {key}") from error
        if digest(rendered) != source["utf8_sha256"]:
            raise NoticeError(f"Rendered notice hash mismatch: {key}")
    validate_coverage(root, manifest, sources)
    return manifest, sources


def validate_coverage(root: Path, manifest: dict, sources: dict[str, bytes]) -> None:
    packages = manifest["packages"]
    identities = [f'{item["name"]}@{item["version"]}' for item in packages]
    if len(set(identities)) != len(identities):
        raise NoticeError("Duplicate package entry")
    graph_ids = {identity for graph in manifest["graphs"] for identity in graph["packages"]}
    if set(identities) != graph_ids:
        missing = sorted(graph_ids - set(identities))
        extra = sorted(set(identities) - graph_ids)
        raise NoticeError(f"Package coverage differs from frozen target graphs. Missing {missing}, extra {extra}")
    locked = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    checksums = {(p["name"], p["version"]): p.get("checksum") for p in locked["package"]}
    used = set()
    for item in packages:
        identity = f'{item["name"]}@{item["version"]}'
        if not item.get("license") or not item.get("notices"):
            raise NoticeError(f"Unhandled package license or notice: {identity}")
        if checksums.get((item["name"], item["version"])) != item["checksum"]:
            raise NoticeError(f"Locked archive checksum differs: {identity}")
        for key in item["notices"]:
            if key not in sources:
                raise NoticeError(f"Missing package notice source: {identity} {key}")
            used.add(key)
    for item in manifest["materials"]:
        if not item.get("notices"):
            raise NoticeError(f'Missing material notice: {item["name"]}')
        for key in item["notices"]:
            if key not in sources:
                raise NoticeError(f'Missing material notice source: {item["name"]} {key}')
            used.add(key)
    if used != sources.keys():
        raise NoticeError("Unreferenced reviewed notice source")
    origins = {origin.get("path") for source in manifest["sources"].values()
               for origin in source["origins"] if origin.get("kind") == "repository"}
    if not set(VENDOR_LICENSES) <= origins:
        raise NoticeError("Mandatory native kernel license omitted from delivered material")
    for source in manifest["sources"].values():
        for origin in source["origins"]:
            if origin.get("kind") == "repository":
                original = read_checked(root, origin["path"], origin["file_sha256"])
                lines = origin.get("lines")
                if lines:
                    original = b"".join(original.splitlines(keepends=True)[lines[0] - 1:lines[1]])
                if digest(original) not in sources or digest(original) != source["sha256"]:
                    raise NoticeError(f'Repository notice source differs: {origin["path"]}')


def render(root: Path, manifest: dict, sources: dict[str, bytes]) -> bytes:
    parts = []

    def text(value: str = "") -> None:
        parts.append((value + "\n").encode("utf-8"))

    text("SlimeNRF Helper licenses and third-party notices")
    text("Generated by python scripts/generate_notices.py. Do not edit this document.")
    text(f'Reviewed input SHA-256: {digest((root / INPUT).read_bytes())}')
    text(f'Cargo.lock SHA-256: {manifest["input_sha256"]["Cargo.lock"]}')
    text()
    for paragraph in manifest["scope"]:
        text(paragraph)
        text()
    text("Application license alternatives")
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        text(f"===== {name} =====")
        parts.append((root / name).read_bytes())
        text()
    text("MPL-2.0 covered source")
    for item in manifest["mpl_source"]:
        text(f'{item["name"]} {item["version"]}')
        text(f'Unmodified covered source archive: {item["url"]}')
        text(f'Archive SHA-256: {item["sha256"]}')
    for paragraph in manifest["source_instructions"]:
        text(paragraph)
    text()
    text("Package index")
    references = {key: [] for key in sources}
    for item in manifest["packages"]:
        identity = f'{item["name"]}@{item["version"]}'
        text(f'===== {identity} =====')
        text(f'Original license declaration: {item["license"]}')
        text(f'Source archive: {item["source_archive"]}')
        text(f'Archive SHA-256: {item["checksum"]}')
        if item.get("repository"):
            text(f'Upstream repository: {item["repository"]}')
        text("Captured roles: " + "; ".join(f'{target}: {", ".join(roles)}'
                                             for target, roles in sorted(item["target_roles"].items())))
        if item.get("policy"):
            text(f'Recorded handling: {item["policy"]}')
        text("Notice source SHA-256: " + ", ".join(item["notices"]))
        text()
        for key in item["notices"]:
            references[key].append(identity)
    text("Additional code, asset, and native kernel materials")
    for item in manifest["materials"]:
        text(item["name"])
        text(item["scope"])
        text("Notice source SHA-256: " + ", ".join(item["notices"]))
        text()
        for key in item["notices"]:
            references[key].append(item["name"])
    text("Complete notice texts")
    text("Identical source texts are reproduced once and referenced by SHA-256 above.")
    text("Alternative license texts are retained as evidence, not relicensing of other material.")
    for key in sorted(sources):
        source = manifest["sources"][key]
        text()
        text(f"===== Notice {key} =====")
        text("Used by: " + ", ".join(references[key]))
        for origin in source["origins"]:
            location = origin.get("url", origin.get("path", ""))
            suffix = f' lines {origin["lines"][0]}-{origin["lines"][1]}' if origin.get("lines") else ""
            text(f'Source: {location}{suffix}')
        if source["encoding"] != "utf-8":
            text(f'Original encoding: {source["encoding"]}. Transcoded to UTF-8 without changing the text.')
            text(f'UTF-8 SHA-256: {source["utf8_sha256"]}')
        text(f'----- Begin complete text ({source["bytes"]} original bytes) -----')
        parts.append(sources[key].decode(source["encoding"]).encode("utf-8"))
        text()
        text("----- End complete text -----")
    text()
    text("Maintainer review boundaries")
    for paragraph in manifest["maintainer_review"]:
        text(paragraph)
    return b"".join(parts)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="Fail if the committed document or any frozen input is stale")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    try:
        manifest, sources = load_inputs(args.root)
        expected = render(args.root, manifest, sources)
        destination = args.root / OUTPUT
        if args.check:
            if not destination.is_file() or destination.read_bytes() != expected:
                raise NoticeError(f"{OUTPUT} is stale. Run python scripts/generate_notices.py after reviewing the inputs.")
            print(f"Notices are current. {len(manifest['packages'])} locked package versions, {len(sources)} source texts.")
        else:
            destination.write_bytes(expected)
            print(f"Wrote {OUTPUT} ({len(expected)} bytes).")
    except (NoticeError, KeyError, TypeError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"Notice generation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
