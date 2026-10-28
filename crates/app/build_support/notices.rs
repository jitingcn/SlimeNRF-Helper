use std::{env, error::Error, path::PathBuf};

pub(super) fn build_notices() -> Result<(), Box<dyn Error>> {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?)
            .join("../..");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("THIRD_PARTY_NOTICES.txt");
    let profile = env::var("PROFILE").unwrap_or_default();
    slimenrf_tools::notices::write_build_notices(&root, &output, &profile)?;
    Ok(())
}
