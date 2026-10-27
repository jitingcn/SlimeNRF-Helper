#!/usr/bin/env python3
"""Render reviewed, frozen notice inputs without Cargo, a registry cache, or a network.

Requires Python 3.11 or newer. Use --check to validate the reviewed inputs, or
--output PATH to generate an artifact. --refresh-inputs may update raw manifest
hashes only when their reviewed non-profile semantics are unchanged. To review
an update, capture both locked target graphs with cargo metadata --locked --offline
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
import os
from pathlib import Path
import sys
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
INPUT = "scripts/notices-inputs.json"
OUTPUT = "THIRD_PARTY_NOTICES.txt"
MANIFESTS = ("Cargo.toml", "crates/recording/Cargo.toml")
REVIEW_GUIDE = "docs/DEVELOPMENT.md#license-notices"
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
    if (candidate.is_absolute() or ".." in candidate.parts or "\\" in name or ":" in name
            or any(char in name for char in "\r\n\0")):
        raise NoticeError(f"Nonportable input path: {name}")
    return root / candidate


def stale_input(name: str) -> NoticeError:
    if name in MANIFESTS:
        return NoticeError(
            f"Stale input hash: {name}. If only the profile or formatting changed, run "
            "python scripts/generate_notices.py --check --refresh-inputs. "
            f"Dependency, package, or other changes require review: {REVIEW_GUIDE}"
        )
    return NoticeError(f"Stale input hash: {name}. Review the changed input: {REVIEW_GUIDE}")


def read_checked(root: Path, name: str, expected: str) -> bytes:
    try:
        data = relative_file(root, name).read_bytes()
    except OSError as error:
        raise NoticeError(f"Missing input {name}: {error.strerror}") from error
    if digest(data) != expected:
        raise stale_input(name)
    return data


def semantic_manifest_hash(data: bytes) -> str:
    manifest = tomllib.loads(data.decode("utf-8"))
    manifest.pop("profile", None)
    canonical = json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return digest(canonical.encode("utf-8"))


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise NoticeError(f"Duplicate key in reviewed inputs: {key}")
        result[key] = value
    return result


def json_object_members(text: str, start: int):
    """Yield an object's parsed values and their positions without reformatting it."""
    decoder = json.JSONDecoder()

    def whitespace(index: int) -> int:
        while index < len(text) and text[index] in " \t\r\n":
            index += 1
        return index

    index = whitespace(start)
    if text[index] != "{":
        raise NoticeError("Expected an object in reviewed inputs")
    index = whitespace(index + 1)
    while text[index] != "}":
        key, index = decoder.raw_decode(text, index)
        index = whitespace(index)
        if text[index] != ":":
            raise NoticeError("Invalid object in reviewed inputs")
        start_value = whitespace(index + 1)
        value, end_value = decoder.raw_decode(text, start_value)
        yield key, value, start_value, end_value
        index = whitespace(end_value)
        if text[index] == ",":
            index = whitespace(index + 1)
        elif text[index] != "}":
            raise NoticeError("Invalid object in reviewed inputs")


def replace_manifest_hashes(original: bytes, changes: dict[str, str]) -> bytes:
    # Keep the reviewed JSON byte-for-byte unchanged outside the selected hashes.
    bom = b"\xef\xbb\xbf" if original.startswith(b"\xef\xbb\xbf") else b""
    text = original[len(bom):].decode("utf-8")
    input_start = next(start for key, _, start, _ in json_object_members(text, 0)
                       if key == "input_sha256")
    replacements = [(start, end, json.dumps(changes[key]))
                    for key, _, start, end in json_object_members(text, input_start)
                    if key in changes]
    if len(replacements) != len(changes):
        raise NoticeError("Missing manifest hash in reviewed inputs")
    for start, end, replacement in reversed(replacements):
        text = text[:start] + replacement + text[end:]
    return bom + text.encode("utf-8")


def load_inputs(root: Path, *, refresh_inputs: bool = False) -> tuple[dict, dict[str, bytes], bytes, bytes]:
    try:
        original = relative_file(root, INPUT).read_bytes()
        manifest = json.loads(original, object_pairs_hook=unique_object)
    except (OSError, ValueError) as error:
        raise NoticeError(f"Cannot read {INPUT}: {error}") from error
    if manifest.get("schema") != 1:
        raise NoticeError("Unsupported notices input schema")
    required = {*MANIFESTS, "Cargo.lock", "LICENSE-MIT", "LICENSE-APACHE", *VENDOR_LICENSES}
    if not required <= manifest["input_sha256"].keys():
        raise NoticeError("Missing mandatory application or vendor license input")

    # Do not bless a manifest hash before checking the frozen sources and coverage.
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

    for name, expected in manifest["input_sha256"].items():
        if name not in MANIFESTS:
            read_checked(root, name, expected)

    changes = {}
    fingerprints = manifest.get("manifest_semantic_sha256")
    if (not isinstance(fingerprints, dict) or any(
        not isinstance(fingerprints.get(name), str)
        or len(fingerprints[name]) != 64
        or any(char not in "0123456789abcdef" for char in fingerprints[name])
        for name in MANIFESTS
    )):
        raise NoticeError(
            f"Missing or invalid reviewed manifest semantic fingerprints; review and update metadata: {REVIEW_GUIDE}"
        )
    for name in MANIFESTS:
        try:
            data = relative_file(root, name).read_bytes()
        except OSError as error:
            raise NoticeError(f"Missing input {name}: {error.strerror}") from error
        actual = digest(data)
        if actual != manifest["input_sha256"][name] and not refresh_inputs:
            raise stale_input(name)
        try:
            semantic = semantic_manifest_hash(data)
        except (ValueError, TypeError, UnicodeError) as error:
            raise NoticeError(f"Cannot fingerprint manifest {name}; review it: {REVIEW_GUIDE}: {error}") from error
        if semantic != fingerprints[name]:
            raise NoticeError(
                f"Manifest semantics changed: {name}; dependency, package, or other changes "
                f"require review: {REVIEW_GUIDE}"
            )
        if actual != manifest["input_sha256"][name]:
            changes[name] = actual
    validate_coverage(root, manifest, sources)
    manifest["input_sha256"].update(changes)
    candidate = replace_manifest_hashes(original, changes) if changes else original
    return manifest, sources, original, candidate


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


def render(root: Path, manifest: dict, sources: dict[str, bytes], input_bytes: bytes | None = None) -> bytes:
    parts = []

    def text(value: str = "") -> None:
        parts.append((value + "\n").encode("utf-8"))

    text("SlimeNRF Helper licenses and third-party notices")
    text("Generated by python scripts/generate_notices.py. Do not edit this document.")
    text(f'Reviewed input SHA-256: {digest(input_bytes if input_bytes is not None else (root / INPUT).read_bytes())}')
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


def cargo_rerun_paths(root: Path, manifest: dict) -> list[Path]:
    names = {"scripts/generate_notices.py", INPUT, *manifest["input_sha256"]}
    names.update(origin["path"] for source in manifest["sources"].values()
                 for origin in source["origins"] if origin.get("kind") == "repository")
    return [relative_file(root, name) for name in sorted(names)]


def write_atomic(path: Path, data: bytes) -> None:
    # A sibling file keeps replacement atomic on the destination filesystem.
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
        if path.exists():
            os.chmod(temporary, path.stat().st_mode)
        else:
            # Notice artifacts are public; respect the process umask for new files.
            mask = os.umask(0)
            os.umask(mask)
            os.chmod(temporary, 0o666 & ~mask)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def warn(message: str, cargo_rerun: bool) -> None:
    print(f"Warning: {message}", file=sys.stderr)
    if cargo_rerun:
        print(f"cargo:warning={' '.join(message.splitlines())}")


def fallback_rerun_paths(root: Path) -> list[Path]:
    # Even malformed metadata must leave enough watches for a repaired checkout to rebuild.
    paths = {root / name for name in (
        INPUT, "scripts/generate_notices.py", *MANIFESTS, "Cargo.lock",
        "LICENSE-MIT", "LICENSE-APACHE", *VENDOR_LICENSES, "crates/recording/vendor/vqf",
    )}
    try:
        manifest = json.loads((root / INPUT).read_bytes())
        paths.update(cargo_rerun_paths(root, manifest))
    except (OSError, ValueError, TypeError, KeyError, AttributeError):
        pass
    return sorted(paths)


def reject_input_output(output: Path, inputs: list[Path]) -> None:
    resolved = output.resolve()
    for path in inputs:
        if resolved == path.resolve() or (output.exists() and path.exists() and output.samefile(path)):
            raise NoticeError(f"Output must not overwrite a reviewed input: {path}")


def development_pending(root: Path, output: Path, error: Exception, cargo_rerun: bool) -> int:
    message = (
        f"DEVELOPMENT LICENSE REVIEW PENDING: {error}. Review dependencies and sources: {REVIEW_GUIDE}. "
        "This development build is not redistribution-ready; the notice inventory is unavailable."
    )
    try:
        reject_input_output(output, fallback_rerun_paths(root))
        write_atomic(output, (message + "\n").encode("utf-8"))
    except (OSError, NoticeError) as write_error:
        print(f"Notice generation failed: {write_error}", file=sys.stderr)
        return 1
    warn(message, cargo_rerun)
    if cargo_rerun:
        for path in fallback_rerun_paths(root):
            print(f"cargo:rerun-if-changed={path}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true",
                        help="Validate frozen inputs and rendering; with --output, also compare artifact bytes without writing")
    parser.add_argument("--output", type=Path, help="Explicit generated artifact destination (required unless --check)")
    parser.add_argument("--refresh-inputs", action="store_true",
                        help="Refresh raw manifest hashes only when reviewed non-profile semantics are unchanged")
    parser.add_argument("--development", action="store_true",
                        help="Emit a non-redistributable diagnostic instead of notices if reviewed inputs are invalid")
    parser.add_argument("--cargo-rerun", action="store_true",
                        help="Emit Cargo rerun-if-changed records for every reviewed repository input")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if not args.check and args.output is None:
        parser.error("--output is required unless --check is used")
    if args.check and args.development:
        parser.error("--development cannot be combined with --check")
    root = args.root.resolve()
    try:
        manifest, sources, original, candidate = load_inputs(root, refresh_inputs=args.refresh_inputs)
        paths = cargo_rerun_paths(root, manifest)
        expected = render(root, manifest, sources, candidate)
    except (ValueError, KeyError, TypeError, AttributeError, IndexError, OSError) as error:
        if args.development:
            return development_pending(root, args.output, error, args.cargo_rerun)
        print(f"Notice generation failed: {error}. Review dependencies and sources: {REVIEW_GUIDE}", file=sys.stderr)
        return 1

    previous_output = None
    output_existed = False
    try:
        if args.check:
            if args.output is not None and (
                not args.output.is_file() or args.output.read_bytes() != expected
            ):
                raise NoticeError(
                    f"{args.output} is missing or stale. Regenerate with --output PATH after reviewing the inputs."
                )
        else:
            reject_input_output(args.output, paths)
            output_existed = args.output.is_file()
            if output_existed and candidate != original:
                previous_output = args.output.read_bytes()
            write_atomic(args.output, expected)
    except (OSError, ValueError) as error:
        print(f"Notice generation failed: {error}", file=sys.stderr)
        return 1

    if candidate != original:
        try:
            input_path = root / INPUT
            if input_path.read_bytes() != original:
                raise NoticeError("Reviewed inputs changed during generation; retry after review")
            write_atomic(input_path, candidate)
        except (OSError, NoticeError) as error:
            if not args.check:
                try:
                    if output_existed:
                        write_atomic(args.output, previous_output)
                    else:
                        args.output.unlink()
                except OSError as restore_error:
                    print(f"Notice generation failed while restoring output: {restore_error}", file=sys.stderr)
                    return 1
            if args.development:
                return development_pending(root, args.output, error, args.cargo_rerun)
            print(f"Notice generation failed: {error}. Review inputs: {REVIEW_GUIDE}", file=sys.stderr)
            return 1
        warn(
            "Refreshed reviewed manifest hashes for profile/format-only changes; "
            f"review and commit {INPUT}. Dependency or source changes still require review: {REVIEW_GUIDE}",
            args.cargo_rerun,
        )
    if args.check:
        print(f"Notices validated. {len(manifest['packages'])} locked package versions, {len(sources)} source texts.")
    else:
        print(f"Wrote {args.output} ({len(expected)} bytes).")
    if args.cargo_rerun:
        for path in paths:
            print(f"cargo:rerun-if-changed={path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
