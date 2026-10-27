use super::FlashPreset;

pub(super) fn valid_board_token(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn identity_fragment(part: &str) -> bool {
    [
        "nosd",
        "s132",
        "s140",
        "sense",
        "spi",
        "i2c",
        "tracker",
        "receiver",
        "datacollect",
        "cdc",
        "hid",
        "mcuboot",
        "update",
    ]
    .iter()
    .any(|identity| part.eq_ignore_ascii_case(identity))
        || (part
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("nrf"))
            && part.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
}

fn application_version_identifier(part: &str) -> bool {
    if part.is_empty() {
        return false;
    }
    if part.bytes().all(|byte| byte.is_ascii_digit()) || part == "dirty" {
        return true;
    }
    if let Some(hash) = part.strip_prefix('g')
        && !hash.is_empty()
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return true;
    }
    [
        "alpha", "beta", "rc", "pre", "preview", "dev", "nightly", "build",
    ]
    .iter()
    .any(|&prefix| {
        part.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.is_empty() || suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn valid_version(version: &str) -> bool {
    let version = version.strip_prefix('v').unwrap_or(version);
    let (release, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    let (numbers, prerelease) = release
        .split_once('-')
        .map_or((release, None), |(a, b)| (a, Some(b)));
    let mut parts = numbers.split('.');
    for _ in 0..3 {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    if parts.next().is_some() {
        return false;
    }
    // An arbitrary SemVer prerelease word can be a hardware profile (Pro,
    // Lite, P00, etc.). Only recognizable version identifiers may be removed.
    [prerelease, build]
        .into_iter()
        .flatten()
        .all(|value| value.split(['.', '-']).all(application_version_identifier))
}

fn valid_bootloader_version(version: &str) -> bool {
    !version.is_empty()
        && version.as_bytes()[0].is_ascii_alphanumeric()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        && !version.split(['.', '-', '+']).any(identity_fragment)
}

/// Only the self-updating, no-SoftDevice UF2 artifact establishes this identity.
/// Raw HEX and SoftDevice packages deliberately have no updater identity.
pub(crate) fn bootloader_board(filename: &str) -> Option<&str> {
    if filename.contains(['/', '\\', ':']) {
        return None;
    }
    let artifact = filename
        .strip_prefix("update-")?
        .strip_suffix("_nosd.uf2")?;
    let (board, version) = artifact.split_once("_bootloader-")?;
    (valid_board_token(board) && valid_bootloader_version(version)).then_some(board)
}

pub(super) fn matches_bootloader(preset: &FlashPreset, filename: &str) -> bool {
    let Some(board) = bootloader_board(filename) else {
        return false;
    };
    preset
        .bootloader_board
        .as_deref()
        .or_else(|| {
            preset
                .bootloader
                .as_ref()
                .and_then(|resource| resource.filename())
                .and_then(bootloader_board)
        })
        .is_some_and(|expected| expected.eq_ignore_ascii_case(board))
}

fn artifact_parts(filename: &str) -> Option<(&str, Option<&str>)> {
    if filename.is_empty()
        || filename.chars().any(|ch| ch.is_control())
        || filename.contains(['/', '\\', ':', '*', '?', '[', ']'])
        || matches!(filename, "." | "..")
    {
        return None;
    }
    match filename.rsplit_once('.') {
        Some((stem, extension)) if matches!(extension, "uf2" | "hex" | "bin" | "zip") => {
            Some((stem, Some(extension)))
        }
        _ => Some((filename, None)),
    }
}

pub(super) fn valid_firmware_alias(alias: &str) -> bool {
    artifact_parts(alias).is_some() && super::filename_variant(alias) != "Bootloader"
}

fn unversioned_stem(stem: &str) -> (&str, &str) {
    // These markers are identities, not version decorations. A version may be
    // adjacent to either end of a known format/category suffix.
    for suffix in [
        "_datacollect_cdc",
        "_datacollect_hid",
        "-datacollect-cdc",
        "-datacollect-hid",
        "_datacollect-cdc",
        "-datacollect_cdc",
        "_datacollect-hid",
        "-datacollect_hid",
        "_datacollect",
        "-datacollect",
        "_update",
        "_mcuboot",
    ] {
        if let Some(prefix) = stem.strip_suffix(suffix) {
            return (strip_version(prefix), suffix);
        }
    }
    (strip_version(stem), "")
}

fn strip_version(stem: &str) -> &str {
    for (index, character) in stem.char_indices() {
        if matches!(character, '_' | '-') && valid_version(&stem[index + 1..]) {
            return &stem[..index];
        }
    }
    stem
}

pub(super) fn matches_application_stem(expected: &[&str], stem: &str) -> bool {
    let (prefix, suffix) = unversioned_stem(stem);
    expected
        .iter()
        .flat_map(|part| part.bytes())
        .eq(prefix.bytes().chain(suffix.bytes()))
}

pub(super) fn matches_firmware_alias(alias: &str, filename: &str) -> bool {
    let alias = alias.to_ascii_lowercase();
    let filename = filename.to_ascii_lowercase();
    if super::filename_variant(&filename) == "Bootloader" {
        return false;
    }
    let Some((alias_stem, alias_extension)) = artifact_parts(&alias) else {
        return false;
    };
    let Some((stem, extension)) = artifact_parts(&filename) else {
        return false;
    };
    let container_matches = match alias_extension {
        Some(expected) => extension == Some(expected),
        None => matches!(extension, Some("uf2" | "hex")),
    };
    let (alias_prefix, alias_suffix) = unversioned_stem(alias_stem);
    let (prefix, suffix) = unversioned_stem(stem);
    container_matches
        && alias_prefix
            .bytes()
            .chain(alias_suffix.bytes())
            .eq(prefix.bytes().chain(suffix.bytes()))
}
