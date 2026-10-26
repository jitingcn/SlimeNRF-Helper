#!/usr/bin/env bash
(
    set -euo pipefail
    source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/scripts/environment.sh"

    no_firmware=0
    force=0
    format=elf
    while (( $# )); do
        case "$1" in
            --no-firmware) no_firmware=1 ;;
            --force) force=1 ;;
            --format)
                if (( $# < 2 )); then
                    printf '%s\n' '--format requires elf, appimage or flatpak.' >&2
                    exit 2
                fi
                format=$2
                shift
                ;;
            -h|--help)
                printf '%s\n' \
                    'Usage: bash package.sh [--format elf|appimage|flatpak] [--no-firmware] [--force]' \
                    'Packages a Release Linux ELF (default), AppImage, or source-built Flatpak bundle.' \
                    'Requires Cargo and Python 3. Native ELF/AppImage respect Cargo target/cache configuration.' \
                    'AppImage also requires pinned linuxdeploy/appimagetool; setup diagnostics precede Cargo.' \
                    'Flatpak needs Python 3.11+, flatpak-builder and Freedesktop 26.08 runtime/SDK.' \
                    'Flatpak fetches the pinned official Rust toolchain and vendors locked sources, then builds offline.' \
                    'ELF uses host libraries. AppImage excludes libc/GPU drivers/services. Flatpak excludes runtimes.'
                exit 0
                ;;
            *) printf 'Unknown package argument: %s\n' "$1" >&2; exit 2 ;;
        esac
        shift
    done
    case "$format" in
        elf|appimage|flatpak) ;;
        *) printf 'Unknown package format: %s (expected elf, appimage or flatpak)\n' "$format" >&2; exit 2 ;;
    esac
    if [[ $(uname -s) != Linux ]]; then
        printf '%s\n' 'package.sh packages native Linux executables; use package.ps1 on Windows.' >&2
        exit 1
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        printf '%s\n' 'Python 3 is required to select the Cargo JSON executable safely.' >&2
        exit 1
    fi
    slimenrf_initialize_cargo
    if [[ $format == appimage ]]; then
        python3 "$SLIMENRF_PROJECT_ROOT/scripts/package_appimage.py" --check-tools
    elif [[ $format == flatpak ]]; then
        flatpak_options=(--check-tools)
        (( ! no_firmware )) || flatpak_options+=(--no-firmware)
        python3 "$SLIMENRF_PROJECT_ROOT/scripts/package_flatpak.py" "${flatpak_options[@]}"
    fi
    temporary=$(mktemp -d "${TMPDIR:-/tmp}/slimenrf-package.XXXXXXXX")
    trap 'rm -rf -- "$temporary"' EXIT
    cargo metadata --locked --no-deps --format-version 1 \
        --manifest-path "$SLIMENRF_PROJECT_ROOT/Cargo.toml" > "$temporary/metadata.json"
    if [[ $format != flatpak ]]; then
        slimenrf_cargo build release "$no_firmware" --message-format=json-render-diagnostics \
            > "$temporary/artifacts.jsonl"
    fi
    python3 "$SLIMENRF_PROJECT_ROOT/scripts/package-artifact.py" \
        "$SLIMENRF_PROJECT_ROOT" "$temporary/metadata.json" "$temporary/artifacts.jsonl" \
        "$no_firmware" "$force" "$format"
)
