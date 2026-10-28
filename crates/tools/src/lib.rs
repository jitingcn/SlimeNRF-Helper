//! Shared notice rendering and internal source, packaging, and oracle tools.

use std::path::{Path, PathBuf};

pub mod notices;

#[cfg(feature = "cli")]
pub mod packaging;
#[cfg(feature = "cli")]
pub mod prerelease;
#[cfg(feature = "cli")]
pub mod public;
#[cfg(feature = "cli")]
pub mod reference_oracle;

#[cfg(all(test, feature = "cli"))]
mod hook_tests;

/// Returns the workspace containing this tools crate.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tools crate must be located under the workspace crates directory")
        .to_path_buf()
}

pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn lower_hex_pads_each_byte() {
        assert_eq!(super::lower_hex(&[0x00, 0x0a, 0xaf, 0xff]), "000aafff");
        assert_eq!(super::lower_hex(&[]), "");
    }
}
