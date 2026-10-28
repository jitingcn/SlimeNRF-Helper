use std::{env, error::Error};

const MARKER: &str = "SLIMENRF_OFFICIAL_RELEASE";
const TAG_OUTPUT: &str = "SLIMENRF_BUILD_OFFICIAL_TAG";
const PRERELEASE_OUTPUT: &str = "SLIMENRF_BUILD_OFFICIAL_PRERELEASE";

pub(super) fn emit_official_release() -> Result<(), Box<dyn Error>> {
    for name in [
        MARKER,
        "SLIMENRF_CONFIG_PATH",
        "SLIMENRF_FIRMWARE_ROOT",
        "GITHUB_ACTIONS",
        "GITHUB_EVENT_NAME",
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_REF_TYPE",
        "GITHUB_REF_NAME",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let Some(marker) = env::var_os(MARKER) else {
        // Override any same-named values inherited by rustc, including on cache hits.
        println!("cargo:rustc-env={TAG_OUTPUT}=");
        println!("cargo:rustc-env={PRERELEASE_OUTPUT}=");
        return Ok(());
    };
    let marker = marker
        .to_str()
        .ok_or("official release marker is not UTF-8")?;
    let (channel, tag) = marker
        .split_once(':')
        .ok_or("official release marker must be release:<tag> or prerelease:<tag>")?;
    let prerelease = match channel {
        "release" => false,
        "prerelease" => true,
        _ => return Err("official release marker has an unknown channel".into()),
    };
    let version = env::var("CARGO_PKG_VERSION")?;
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        || tag.strip_prefix('v') != Some(version.as_str())
    {
        return Err(format!("official release tag {tag:?} must equal v{version}").into());
    }
    if env::var("TARGET").as_deref() != Ok("x86_64-pc-windows-msvc")
        || env::var("PROFILE").as_deref() != Ok("release")
        || env::var("DEBUG").as_deref() != Ok("false")
        || env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some()
    {
        return Err(
            "official release marker requires a non-debug Windows x64 MSVC release build".into(),
        );
    }
    for name in ["SLIMENRF_CONFIG_PATH", "SLIMENRF_FIRMWARE_ROOT"] {
        if env::var_os(name).is_some() {
            return Err(format!("official release marker cannot be combined with {name}").into());
        }
    }
    if env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || env::var("GITHUB_EVENT_NAME").as_deref() != Ok("push")
        || env::var("GITHUB_REPOSITORY").as_deref() != Ok("jitingcn/SlimeNRF-Helper")
        || env::var("GITHUB_REF_TYPE").as_deref() != Ok("tag")
        || env::var("GITHUB_REF_NAME").as_deref() != Ok(tag)
        || !matches!(
            env::var("GITHUB_REF").as_deref(),
            Ok(reference) if reference.strip_prefix("refs/tags/") == Some(tag)
        )
    {
        return Err(
            "official release marker requires the corresponding tagged official GitHub Actions push"
                .into(),
        );
    }

    println!("cargo:rustc-env={TAG_OUTPUT}={tag}");
    println!("cargo:rustc-env={PRERELEASE_OUTPUT}={prerelease}");
    Ok(())
}
