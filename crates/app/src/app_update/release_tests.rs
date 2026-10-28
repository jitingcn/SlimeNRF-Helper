use super::*;
use serde_json::json;

const X64_FULL: &str = "slimenrf-helper-1.2.3-windows-x64.exe";
const X64_NO_FIRMWARE: &str = "slimenrf-helper-1.2.3-windows-x64-no-firmware.exe";
const ARM64_FULL: &str = "slimenrf-helper-1.2.3-windows-arm64.exe";
const ARM64_NO_FIRMWARE: &str = "slimenrf-helper-1.2.3-windows-arm64-no-firmware.exe";
const DIGEST: &str = "sha256:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_BYTES: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];

fn api_release(tag: &str) -> ApiRelease {
    serde_json::from_value(json!({
        "id": 123,
        "tag_name": tag,
        "draft": false,
        "prerelease": false,
        "body": "Release notes",
        "unrelated_api_field": "ignored"
    }))
    .expect("release fixture should deserialize")
}

fn api_asset(name: &str) -> ApiAsset {
    serde_json::from_value(json!({
        "id": 456,
        "name": name,
        "state": "uploaded",
        "size": 12345,
        "digest": DIGEST,
        "unrelated_api_field": "ignored"
    }))
    .expect("asset fixture should deserialize")
}

fn selected(tag: &str, channel: Channel, current: &str) -> Option<Candidate> {
    candidate(
        api_release(tag),
        channel,
        &semver::Version::parse(current).expect("current version fixture should parse"),
    )
}

fn newer_release() -> Candidate {
    selected("v1.2.3", Channel::Stable, "1.0.0").expect("release should be newer")
}

fn x64_full() -> Target {
    Target {
        arch: Architecture::X64,
        variant: Variant::Full,
    }
}

fn frozen(candidate: Candidate, name: &str, target: Target) -> Release {
    make_release(candidate, vec![api_asset(name)], target)
        .expect("valid matching asset")
        .expect("matching asset should be present")
}

#[test]
fn tags_allow_only_one_optional_lowercase_v_and_preserve_the_original_tag() {
    for tag in ["1.2.3", "v1.2.3"] {
        let release = selected(tag, Channel::Stable, "1.0.0").expect("valid newer tag");
        assert_eq!(release.version, semver::Version::parse("1.2.3").unwrap());
        assert_eq!(release.tag, tag);
        assert!(!release.prerelease);
    }

    for tag in [
        "",
        "v",
        "vv1.2.3",
        "V1.2.3",
        "release-1.2.3",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.2.3-01",
        "1.2.3+bad!",
        " 1.2.3",
        "1.2.3 ",
        "18446744073709551616.0.0",
    ] {
        assert!(
            selected(tag, Channel::Preview, "0.0.0").is_none(),
            "invalid tag {tag:?} was accepted"
        );
    }
    let long_tag = format!("1.2.3-{}", "a".repeat(MAX_TAG_BYTES));
    assert!(selected(&long_tag, Channel::Preview, "0.0.0").is_none());
}

#[test]
fn channel_admission_checks_both_prerelease_signals_and_drafts() {
    let current = semver::Version::parse("1.0.0").unwrap();
    for (tag, api_prerelease, is_prerelease) in [
        ("v1.2.3", false, false),
        ("v1.2.3", true, true),
        ("v1.2.3-rc.1", false, true),
        ("v1.2.3-rc.1", true, true),
    ] {
        let mut stable = api_release(tag);
        stable.prerelease = api_prerelease;
        let stable = candidate(stable, Channel::Stable, &current);
        assert_eq!(
            stable.is_some(),
            !is_prerelease,
            "stable admission for {tag:?}, API flag {api_prerelease}"
        );
        if let Some(stable) = stable {
            assert!(!stable.prerelease);
        }

        let mut preview = api_release(tag);
        preview.prerelease = api_prerelease;
        let preview = candidate(preview, Channel::Preview, &current)
            .expect("preview should accept valid newer stable and prerelease tags");
        assert_eq!(preview.prerelease, is_prerelease, "tag {tag:?}");
    }

    for channel in [Channel::Stable, Channel::Preview] {
        let mut draft = api_release("v1.2.3");
        draft.draft = true;
        assert!(candidate(draft, channel, &current).is_none());

        let mut no_id = api_release("v1.2.3");
        no_id.id = 0;
        assert!(candidate(no_id, channel, &current).is_none());
    }
}

#[test]
fn releases_must_be_strictly_newer_by_numeric_precedence_not_build_metadata() {
    let cases = [
        ("1.10.0", "1.9.0", true),
        ("1.9.0", "1.10.0", false),
        ("2.0.0", "1.99.99", true),
        ("1.99.99", "2.0.0", false),
        ("1.2.3", "1.2.3", false),
        ("v1.2.3", "1.2.3", false),
        ("1.2.3", "1.2.3-rc.1", true),
        ("1.2.3", "1.2.4-rc.1", false),
        ("1.2.3+new", "1.2.3+old", false),
        ("1.2.3+old", "1.2.3+new", false),
        ("1.2.3+build.9", "1.2.3", false),
        ("1.2.3", "1.2.3+build.9", false),
        ("1.3.0+build.1", "1.2.3+build.9", true),
    ];
    for channel in [Channel::Stable, Channel::Preview] {
        for (tag, current, expected) in cases {
            assert_eq!(
                selected(tag, channel, current).is_some(),
                expected,
                "channel {channel:?}, tag {tag:?}, current {current:?}"
            );
        }
    }
}

#[test]
fn preview_versions_follow_semver_prerelease_precedence() {
    let ordered = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
    ];
    for (current_index, current) in ordered.iter().enumerate() {
        for (tag_index, tag) in ordered.iter().enumerate() {
            assert_eq!(
                selected(tag, Channel::Preview, current).is_some(),
                tag_index > current_index,
                "tag {tag:?}, current {current:?}"
            );
        }
    }
    assert!(selected("2.0.0-rc.10", Channel::Preview, "2.0.0-rc.2").is_some());
    assert!(selected("2.0.0-rc.2", Channel::Preview, "2.0.0-rc.10").is_none());
    assert!(selected("2.0.0-rc.1+build.2", Channel::Preview, "2.0.0-rc.1+build.1").is_none());
}

#[test]
fn asset_selection_requires_the_exact_architecture_variant_and_filename() {
    let targets = [
        (x64_full(), X64_FULL),
        (
            Target {
                arch: Architecture::X64,
                variant: Variant::NoFirmware,
            },
            X64_NO_FIRMWARE,
        ),
        (
            Target {
                arch: Architecture::Arm64,
                variant: Variant::Full,
            },
            ARM64_FULL,
        ),
        (
            Target {
                arch: Architecture::Arm64,
                variant: Variant::NoFirmware,
            },
            ARM64_NO_FIRMWARE,
        ),
    ];
    let names = [
        X64_FULL,
        X64_NO_FIRMWARE,
        ARM64_FULL,
        ARM64_NO_FIRMWARE,
        "slimenrf-helper-1.2.3-windows-x64-upx.exe",
        "slimenrf-helper-1.2.3-windows-x64-no-firmware-upx.exe",
        "slimenrf-helper-1.2.3-windows-arm64-upx.exe",
        "slimenrf-helper-1.2.3-windows-arm64-no-firmware-upx.exe",
        "slimenrf-helper-v1.2.3-windows-x64.exe",
        "slimenrf-helper-1.2.2-windows-x64.exe",
        "slimenrf-helper-1.2.3-linux-x64.exe",
        "slimenrf-helper-1.2.3-windows-x86.exe",
        "slimenrf-helper-1.2.3-windows-x64.EXE",
        "slimenrf-helper-1.2.3-windows-x64.exe.zip",
        "other/slimenrf-helper-1.2.3-windows-x64.exe",
    ];

    for (target, expected) in targets {
        assert!(
            make_release(newer_release(), vec![], target)
                .expect("absence is not a metadata error")
                .is_none()
        );
        for name in names {
            if name == expected {
                continue;
            }
            assert!(
                make_release(newer_release(), vec![api_asset(name)], target)
                    .expect("foreign asset is not a metadata error")
                    .is_none(),
                "target {target:?} incorrectly accepted {name:?}"
            );
        }

        let mut foreign = api_asset("foreign.zip");
        foreign.id = 0;
        foreign.digest = None;
        assert!(
            make_release(
                newer_release(),
                vec![foreign, api_asset("foreign.zip")],
                target
            )
            .expect("duplicate foreign assets do not match")
            .is_none()
        );

        let mut irrelevant = api_asset("other.zip");
        irrelevant.id = 0;
        irrelevant.size = 0;
        irrelevant.state = "pending".into();
        irrelevant.digest = None;
        let release = make_release(
            newer_release(),
            vec![irrelevant, api_asset(expected)],
            target,
        )
        .expect("unrelated invalid metadata must not invalidate a matching asset")
        .expect("exact asset should be selected");
        assert_eq!(release.asset.name, expected);
    }
}

#[test]
fn exact_asset_name_uses_normalized_full_semver_including_suffixes() {
    for (tag, expected, prerelease) in [
        ("v1.2.3", X64_FULL, false),
        (
            "v1.2.3-rc.1",
            "slimenrf-helper-1.2.3-rc.1-windows-x64.exe",
            true,
        ),
        (
            "v1.2.3+build.1",
            "slimenrf-helper-1.2.3+build.1-windows-x64.exe",
            false,
        ),
    ] {
        let candidate = selected(tag, Channel::Preview, "1.0.0").expect("newer version");
        let release = frozen(candidate, expected, x64_full());
        assert_eq!(release.tag, tag);
        assert_eq!(release.version, tag.strip_prefix('v').unwrap());
        assert_eq!(release.prerelease, prerelease);
        assert_eq!(release.asset.name, expected);
    }

    let mut api = api_release("v1.2.3");
    api.prerelease = true;
    let current = semver::Version::parse("1.0.0").unwrap();
    let candidate = candidate(api, Channel::Preview, &current).expect("newer release");
    let release = frozen(candidate, X64_FULL, x64_full());
    assert!(
        release.prerelease,
        "GitHub prerelease flag must be retained"
    );
}

#[test]
fn duplicate_exact_assets_fail_even_if_one_has_invalid_metadata() {
    let mut invalid = api_asset(X64_FULL);
    invalid.id = 0;
    assert!(
        make_release(
            newer_release(),
            vec![api_asset(X64_FULL), invalid],
            x64_full()
        )
        .is_err()
    );
    let mut invalid = api_asset(X64_FULL);
    invalid.digest = None;
    assert!(
        make_release(
            newer_release(),
            vec![invalid, api_asset(X64_FULL)],
            x64_full()
        )
        .is_err()
    );
    assert!(
        make_release(
            newer_release(),
            vec![api_asset(X64_FULL), api_asset(X64_FULL)],
            x64_full()
        )
        .is_err()
    );
}

#[test]
fn matching_asset_requires_uploaded_nonzero_bounded_metadata() {
    let mut no_id = api_asset(X64_FULL);
    no_id.id = 0;
    assert!(make_release(newer_release(), vec![no_id], x64_full()).is_err());

    for state in ["", "new", "pending", "UPLOADED"] {
        let mut asset = api_asset(X64_FULL);
        asset.state = state.into();
        assert!(
            make_release(newer_release(), vec![asset], x64_full()).is_err(),
            "asset state {state:?} was accepted"
        );
    }
    for size in [0, MAX_ASSET_SIZE + 1, u64::MAX] {
        let mut asset = api_asset(X64_FULL);
        asset.size = size;
        assert!(
            make_release(newer_release(), vec![asset], x64_full()).is_err(),
            "asset size {size} was accepted"
        );
    }

    for size in [1, MAX_ASSET_SIZE] {
        let mut asset = api_asset(X64_FULL);
        asset.size = size;
        let release = make_release(newer_release(), vec![asset], x64_full())
            .expect("sizes within the limit are valid")
            .expect("the asset should match");
        assert_eq!(release.asset.size, size);
    }
}

#[test]
fn matching_asset_requires_an_exact_sha256_digest() {
    let invalid = [
        None,
        Some(String::new()),
        Some("sha256:".into()),
        Some(format!("sha256:{}", "0".repeat(63))),
        Some(format!("sha256:{}", "0".repeat(65))),
        Some(format!("sha256:{}g", "0".repeat(63))),
        Some(format!("sha256:{}é", "0".repeat(62))),
        Some(format!("SHA256:{}", "0".repeat(64))),
        Some(format!("sha512:{}", "0".repeat(64))),
        Some(format!(" {DIGEST}")),
        Some(format!("{DIGEST} ")),
    ];
    for digest in invalid {
        let mut asset = api_asset(X64_FULL);
        asset.digest = digest.clone();
        assert!(
            make_release(newer_release(), vec![asset], x64_full()).is_err(),
            "invalid digest {digest:?} was accepted"
        );
    }

    let missing_digest: ApiAsset = serde_json::from_value(json!({
        "id": 456,
        "name": X64_FULL,
        "state": "uploaded",
        "size": 12345
    }))
    .expect("GitHub can omit an optional digest");
    assert!(make_release(newer_release(), vec![missing_digest], x64_full()).is_err());
}

#[test]
fn release_freezes_asset_identity_size_and_case_insensitive_hex_bytes() {
    let mut asset = api_asset(X64_FULL);
    asset.id = 987654;
    asset.size = 42;
    asset.digest =
        Some("sha256:000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F".into());
    let release = make_release(newer_release(), vec![asset], x64_full())
        .expect("valid metadata")
        .expect("exact asset");
    assert_eq!(release.version, "1.2.3");
    assert_eq!(release.tag, "v1.2.3");
    assert!(!release.prerelease);
    assert_eq!(release.asset.name, X64_FULL);
    assert_eq!(release.asset.id, 987654);
    assert_eq!(release.asset.size, 42);
    assert_eq!(release.asset.sha256, DIGEST_BYTES);
}

#[test]
fn notes_are_bounded_on_utf8_boundaries_and_optional_notes_are_empty() {
    const NOTES_LIMIT: usize = 64 * 1024;
    let current = semver::Version::parse("1.0.0").unwrap();

    let mut without_notes = api_release("v1.2.3");
    without_notes.body = None;
    let release = frozen(
        candidate(without_notes, Channel::Stable, &current).expect("newer release"),
        X64_FULL,
        x64_full(),
    );
    assert_eq!(release.notes, "");

    let mut at_limit = api_release("v1.2.3");
    let notes = "a".repeat(NOTES_LIMIT);
    at_limit.body = Some(notes.clone());
    let release = frozen(
        candidate(at_limit, Channel::Stable, &current).expect("newer release"),
        X64_FULL,
        x64_full(),
    );
    assert_eq!(release.notes, notes);

    let mut over_limit = api_release("v1.2.3");
    let expected = "a".repeat(NOTES_LIMIT - 1);
    over_limit.body = Some(format!("{expected}中tail"));
    let release = frozen(
        candidate(over_limit, Channel::Stable, &current).expect("newer release"),
        X64_FULL,
        x64_full(),
    );
    assert_eq!(release.notes, expected);
}
