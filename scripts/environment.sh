#!/usr/bin/env bash
# Source from the Linux entrypoints, which run inside a subshell.
slimenrf_initialize_cargo() {
    if ! command -v cargo >/dev/null 2>&1; then
        printf '%s\n' 'Cargo is required on PATH.' >&2
        return 1
    fi
    SLIMENRF_PROJECT_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || return
    if [[ -n ${CARGO_TARGET_DIR:-} && $CARGO_TARGET_DIR != /* ]]; then
        export CARGO_TARGET_DIR="$SLIMENRF_PROJECT_ROOT/$CARGO_TARGET_DIR"
    fi
    cd -- "$SLIMENRF_PROJECT_ROOT" || return
}

slimenrf_require_nextest() {
    if ! command -v cargo-nextest >/dev/null 2>&1; then
        printf '%s\n' 'cargo-nextest is required. Install it with: cargo install --locked cargo-nextest --version 0.9.150' >&2
        return 1
    fi
}

slimenrf_test_doc_only() {
    local argument
    for argument in "$@"; do
        [[ $argument != -- ]] || break
        [[ $argument != --doc ]] || return 0
    done
    return 1
}

slimenrf_cargo() {
    local action=$1 profile=$2 no_firmware=$3
    shift 3
    local argument has_selection=0
    local -a arguments=("$action" --manifest-path "$SLIMENRF_PROJECT_ROOT/Cargo.toml")
    # --doc uses Cargo directly; every other test argument uses nextest's CLI.
    if [[ $action == test ]] && ! slimenrf_test_doc_only "$@"; then
        slimenrf_require_nextest
        arguments=(nextest run --manifest-path "$SLIMENRF_PROJECT_ROOT/Cargo.toml")
    fi
    # Cargo's own -- belongs to the program, test harness, rustfmt or Clippy.
    for argument in "$@"; do
        [[ $argument != -- ]] || break
        case "$argument" in
            -p*|--package|--package=*|--workspace|--all|--exclude|--exclude=*)
                has_selection=1
                break
                ;;
        esac
    done
    if (( ! has_selection )); then
        case "$action" in
            build|run) arguments+=(--package slimenrf-helper) ;;
            test|check|clippy) arguments+=(--workspace) ;;
            fmt) arguments+=(--all) ;;
        esac
    fi
    [[ $action == fmt ]] || arguments+=(--locked)
    case "$action" in
        fmt|clean) ;;
        *)
            [[ $profile != release ]] || arguments+=(--release)
            (( ! no_firmware )) || arguments+=(--no-default-features)
            ;;
    esac
    cargo "${arguments[@]}" "$@"
}
