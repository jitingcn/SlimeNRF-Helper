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
                    'Requires Cargo and Python 3.11+. Native ELF/AppImage respect Cargo target/cache configuration.' \
                    'SLIMENRF_PYTHON selects one interpreter; otherwise python3, python, then installed uv Python are tried.' \
                    'AppImage also requires pinned linuxdeploy/appimagetool; setup diagnostics precede Cargo.' \
                    'Flatpak needs flatpak-builder and Freedesktop 26.08 runtime/SDK with Python 3.11+.' \
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
    python_probe='import sys; sys.version_info >= (3, 11) or sys.exit("Python 3.11+ is required."); import tomllib; print(sys.executable)'
    temporary=
    if [[ ${SLIMENRF_PYTHON+x} ]]; then
        if ! selected_python=$("$SLIMENRF_PYTHON" -c "$python_probe"); then
            printf 'A working Python 3.11+ interpreter with tomllib is required: %s\n' "$SLIMENRF_PYTHON" >&2
            exit 1
        fi
    else
        python_found=0
        for python in python3 python; do
            if selected_python=$("$python" -c "$python_probe"); then
                python_found=1
                break
            fi
        done
        if (( ! python_found )); then
            temporary=$(mktemp -d "${TMPDIR:-/tmp}/slimenrf-package.XXXXXXXX")
            trap 'rm -rf -- "$temporary"' EXIT
            if ! uv python find --no-project --system --no-python-downloads --offline --no-config '>=3.11' \
                > "$temporary/uv-python"; then
                printf '%s\n' 'A working Python 3.11+ interpreter with tomllib is required; uv could not find an installed interpreter. The lookup never downloads Python.' >&2
                exit 1
            fi
            mapfile -t uv_paths < "$temporary/uv-python"
            if IFS= read -r -d '' _ < "$temporary/uv-python" ||
                [[ ! ${uv_paths[0]+set} || ${uv_paths[1]+set} ]]; then
                printf '%s\n' 'uv must return one absolute interpreter path.' >&2
                exit 1
            fi
            python=${uv_paths[0]}
            if [[ $python != /* || $python == *$'\r'* ]]; then
                printf '%s\n' 'uv must return one absolute interpreter path.' >&2
                exit 1
            fi
            if ! selected_python=$("$python" -c "$python_probe"); then
                printf 'The Python interpreter found by uv failed its Python 3.11+/tomllib probe: %s\n' "$python" >&2
                exit 1
            fi
        fi
    fi
    export SLIMENRF_PYTHON=$selected_python
    slimenrf_initialize_cargo
    if [[ $format == appimage ]]; then
        "$SLIMENRF_PYTHON" "$SLIMENRF_PROJECT_ROOT/scripts/package_appimage.py" --check-tools
    elif [[ $format == flatpak ]]; then
        flatpak_options=(--check-tools)
        (( ! no_firmware )) || flatpak_options+=(--no-firmware)
        "$SLIMENRF_PYTHON" "$SLIMENRF_PROJECT_ROOT/scripts/package_flatpak.py" "${flatpak_options[@]}"
    fi
    if [[ -z $temporary ]]; then
        temporary=$(mktemp -d "${TMPDIR:-/tmp}/slimenrf-package.XXXXXXXX")
        trap 'rm -rf -- "$temporary"' EXIT
    fi
    cargo metadata --locked --no-deps --format-version 1 \
        --manifest-path "$SLIMENRF_PROJECT_ROOT/Cargo.toml" > "$temporary/metadata.json"
    if [[ $format != flatpak ]]; then
        slimenrf_cargo build release "$no_firmware" --message-format=json-render-diagnostics \
            > "$temporary/artifacts.jsonl"
    fi
    "$SLIMENRF_PYTHON" "$SLIMENRF_PROJECT_ROOT/scripts/package-artifact.py" \
        "$SLIMENRF_PROJECT_ROOT" "$temporary/metadata.json" "$temporary/artifacts.jsonl" \
        "$no_firmware" "$force" "$format"
)
