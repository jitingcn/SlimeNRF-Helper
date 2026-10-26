#!/usr/bin/env bash
(
    set -euo pipefail
    source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/scripts/environment.sh"

    usage() {
        printf '%s\n' \
            'Usage: bash build.sh [build|run|test|check|clippy|fmt|clean]' \
            '                     [--debug|--release] [--no-firmware]' \
            '                     [--cargo-arguments <Cargo arguments...>]' \
            'Defaults: build uses Release; run/test/check/clippy use development.' \
            'Everything after --cargo-arguments is passed unchanged, including Cargo --.'
    }
    action=build
    action_set=0
    debug=0
    release=0
    no_firmware=0
    cargo_arguments=()
    while (( $# )); do
        case "$1" in
            build|run|test|check|clippy|fmt|clean)
                if (( action_set )); then
                    printf '%s\n' 'Specify only one build action.' >&2
                    exit 2
                fi
                action=$1
                action_set=1
                ;;
            --debug) debug=1 ;;
            --release) release=1 ;;
            --no-firmware) no_firmware=1 ;;
            --cargo-arguments)
                shift
                cargo_arguments=("$@")
                break
                ;;
            -h|--help) usage; exit 0 ;;
            *) printf 'Unknown build argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
        esac
        shift
    done
    if (( debug && release )); then
        printf '%s\n' 'Specify either --debug or --release, not both.' >&2
        exit 2
    fi
    profile=dev
    if (( release )) || { [[ $action == build ]] && (( ! debug )); }; then
        profile=release
    fi
    slimenrf_initialize_cargo
    slimenrf_cargo "$action" "$profile" "$no_firmware" "${cargo_arguments[@]}"
)
