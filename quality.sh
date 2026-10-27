#!/usr/bin/env bash
(
    set -euo pipefail
    source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/scripts/environment.sh"

    no_firmware=0
    format_only=0
    while (( $# )); do
        case "$1" in
            --no-firmware) no_firmware=1 ;;
            --format-only) format_only=1 ;;
            -h|--help)
                printf '%s\n' 'Usage: bash quality.sh [--no-firmware] [--format-only]'
                exit 0
                ;;
            *) printf 'Unknown quality argument: %s\n' "$1" >&2; exit 2 ;;
        esac
        shift
    done
    slimenrf_initialize_cargo
    if (( ! format_only )); then
        slimenrf_require_nextest
    fi
    slimenrf_cargo fmt dev 0 -- --check
    if (( format_only )); then
        exit 0
    fi
    slimenrf_cargo clippy dev "$no_firmware" --all-targets -- -D warnings
    slimenrf_cargo test dev "$no_firmware" --all-targets
    slimenrf_cargo test dev "$no_firmware" --doc
)
