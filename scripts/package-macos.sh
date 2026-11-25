#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf '%s\n' "$1" >&2
    exit 1
}

arch=
force=0
for argument in "$@"; do
    case "$argument" in
        arm64|x64)
            [[ -z $arch ]] || fail 'Specify only one architecture.'
            arch=$argument
            ;;
        --force) force=1 ;;
        -h|--help)
            printf '%s\n' \
                'Usage: bash scripts/package-macos.sh [arm64|x64] [--force]' \
                'Builds the no-firmware Release app for the chosen architecture (default: native).' \
                'Signs the app ad hoc and writes a verified, read-only preview DMG to dist/.'
            exit 0
            ;;
        *) fail "Unknown package argument: $argument" ;;
    esac
done

[[ $(uname -s) == Darwin ]] || fail 'macOS preview packaging requires macOS.'
case "${arch:-$(uname -m)}" in
    arm64) arch=arm64; target=aarch64-apple-darwin; macho_arch=arm64 ;;
    x64|x86_64) arch=x64; target=x86_64-apple-darwin; macho_arch=x86_64 ;;
    *) fail 'Unsupported macOS host architecture.' ;;
esac

# Keep the resulting image path as the only standard output for CI.
exec 3>&1 1>&2
# shellcheck source=scripts/environment.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/environment.sh"
slimenrf_initialize_cargo
command -v jq >/dev/null || fail 'jq is required on PATH.'
command -v cmake >/dev/null || fail 'CMake is required on PATH.'
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
template="$SLIMENRF_PROJECT_ROOT/scripts/macos/Info.plist"
# Use GPUI Kit's macOS floor for Rust, native libraries, and Launch Services.
MACOSX_DEPLOYMENT_TARGET=$(plutil -extract LSMinimumSystemVersion raw -expect string "$template")
export SDKROOT MACOSX_DEPLOYMENT_TARGET
host=$("${RUSTC:-rustc}" --print host-tuple)
version=$(cargo metadata --locked --no-deps --format-version 1 | jq -er \
    '.packages[] | select(.name == "slimenrf-helper") | .version')
bundle_version=${version%%[-+]*}
dist="$SLIMENRF_PROJECT_ROOT/dist"
name="slimenrf-helper-$version-macos-$arch-no-firmware.dmg"
destination="$dist/$name"
if [[ -e $destination || -L $destination ]]; then
    [[ -f $destination && ! -L $destination ]] || fail "Not a regular file: $destination"
    (( force )) || fail "Already exists: $destination (use --force to replace it)."
fi

mkdir -p "$dist"
staging=$(mktemp -d "$dist/.macos.XXXXXXXX")
mountpoint="$staging/mounted"
mounted=0
detach_image() {
    hdiutil detach "$mountpoint" || hdiutil detach "$mountpoint" -force || return 1
    mounted=0
}
cleanup() {
    if (( mounted )); then
        detach_image || return 1
    fi
    rm -rf -- "$staging"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# GPUI Kit enables runtime_shaders: the build embeds stitched Metal source in
# the executable, and Metal compiles it at startup. No external shaders ship.
slimenrf_cargo build release 1 --target "$target" --bin slimenrf-helper \
    --message-format=json-render-diagnostics > "$staging/artifacts.jsonl"
binary=$(jq -ser '
    [.[] | select(.reason == "compiler-artifact" and .target.name == "slimenrf-helper"
        and .target.kind == ["bin"] and .executable != null
        and (.features | index("embed-firmware") | not)) | .executable]
    | if length == 1 then .[0] else error("expected one no-firmware application executable") end
' "$staging/artifacts.jsonl")

app_name='SlimeNRF Helper'
source="$staging/image"
app="$source/$app_name.app"
contents="$app/Contents"
licenses="$contents/Resources/licenses"
mkdir -p "$contents/MacOS" "$licenses"
install -m 755 "$binary" "$contents/MacOS/slimenrf-helper"
install -m 644 "$template" "$contents/Info.plist"
plutil -insert CFBundleShortVersionString -string "$bundle_version" "$contents/Info.plist"
plutil -insert CFBundleVersion -string "$bundle_version" "$contents/Info.plist"
install -m 644 LICENSE-MIT LICENSE-APACHE "$licenses/"
cargo run --quiet --locked --package slimenrf-tools --target "$host" -- \
    notices --output "$licenses/THIRD_PARTY_NOTICES.txt"
codesign --force --sign - --timestamp=none "$app"
ln -s /Applications "$source/Applications"

image="$staging/$name"
hdiutil create -srcfolder "$source" -volname "$app_name" -fs HFS+ -format UDZO "$image"
hdiutil verify "$image"
mkdir "$mountpoint"
hdiutil attach "$image" -readonly -nobrowse -mountpoint "$mountpoint"
mounted=1

app="$mountpoint/$app_name.app"
contents="$app/Contents"
plist="$contents/Info.plist"
plutil -lint "$plist"
executable=$(plutil -extract CFBundleExecutable raw -expect string "$plist")
binary="$contents/MacOS/$executable"
[[ -x $binary ]] || fail "Missing bundle executable: $binary"
[[ $(xcrun lipo -archs "$binary") == "$macho_arch" ]] || fail "Expected a thin $macho_arch Mach-O."
linked_minimum=$(xcrun vtool -show-build "$binary" | awk '
    $1 == "platform" { platform = $2 }
    $1 == "minos" && platform == "MACOS" { print $2 }
')
[[ $linked_minimum == "$MACOSX_DEPLOYMENT_TARGET" ]] ||
    fail "Mach-O minimum '${linked_minimum:-missing}' differs from $MACOSX_DEPLOYMENT_TARGET."
xcrun otool -L "$binary" | awk '
    NR > 1 && NF && $1 !~ /^\/(System\/Library|usr\/lib)\// {
        print "Unexpected linked library: " $1
        invalid = 1
    }
    END { exit invalid }
'
for key in NSBluetoothAlwaysUsageDescription NSRemovableVolumesUsageDescription; do
    usage=$(plutil -extract "$key" raw -expect string "$plist")
    [[ -n $usage ]] || fail "Missing app usage description: $key"
done
codesign --verify --strict --verbose=2 "$app"
signature=$(codesign --display --verbose=2 "$app" 2>&1)
awk -F= '$1 == "Signature" && $2 == "adhoc" { found=1 } END { exit !found }' <<< "$signature" ||
    fail 'Expected an ad hoc app signature.'
[[ $(readlink "$mountpoint/Applications") == /Applications ]] || fail 'Missing Applications link.'
for license in LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.txt; do
    [[ -s $contents/Resources/licenses/$license ]] || fail "Missing bundle license: $license"
done

detach_image
mv -f -- "$image" "$destination"
printf '%s\n' "$destination" >&3
