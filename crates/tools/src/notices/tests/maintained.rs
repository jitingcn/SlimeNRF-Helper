use super::*;

fn decode_windows_1252(data: &[u8]) -> String {
    const SPECIAL: [Option<char>; 32] = [
        Some('€'),
        None,
        Some('‚'),
        Some('ƒ'),
        Some('„'),
        Some('…'),
        Some('†'),
        Some('‡'),
        Some('ˆ'),
        Some('‰'),
        Some('Š'),
        Some('‹'),
        Some('Œ'),
        None,
        Some('Ž'),
        None,
        None,
        Some('‘'),
        Some('’'),
        Some('“'),
        Some('”'),
        Some('•'),
        Some('–'),
        Some('—'),
        Some('˜'),
        Some('™'),
        Some('š'),
        Some('›'),
        Some('œ'),
        None,
        Some('ž'),
        Some('Ÿ'),
    ];
    data.iter()
        .map(|&byte| match byte {
            0x80..=0x9f => SPECIAL[usize::from(byte - 0x80)]
                .expect("reviewed CP1252 source has no undefined bytes"),
            _ => char::from(byte),
        })
        .collect()
}

#[test]
fn reviewed_manifest_fingerprints_match_current_inputs() {
    let root = crate::workspace_root();
    let loaded = load_inputs(&root, true).expect("load maintained reviewed inventory");
    assert_eq!(
        loaded.candidate, loaded.original,
        "maintained manifest hashes must already be refreshed"
    );
}

#[test]
fn all_reviewed_source_texts_survive_rendering() {
    let root = crate::workspace_root();
    let loaded = load_inputs(&root, false).expect("load maintained reviewed inventory");
    let output = render(&root, &loaded).expect("render maintained reviewed inventory");
    let output = std::str::from_utf8(&output).expect("rendered inventory is UTF-8");
    let targets: BTreeSet<_> = loaded.manifest["graphs"]
        .as_array()
        .expect("reviewed target graphs")
        .iter()
        .map(|graph| graph["target"].as_str().expect("target triple"))
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from([
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
        ])
    );
    assert!(output.contains(
        "Captured roles: aarch64-apple-darwin: code_candidate; x86_64-apple-darwin: code_candidate\n"
    ));
    for (key, original) in &loaded.sources {
        let encoding = loaded.manifest["sources"][key.as_str()]["encoding"]
            .as_str()
            .expect("reviewed source encoding");
        let decoded;
        let source = match encoding {
            "utf-8" => std::str::from_utf8(original).expect("reviewed UTF-8 source"),
            "windows-1252" => {
                decoded = decode_windows_1252(original);
                decoded.as_str()
            }
            other => panic!("unexpected encoding {other:?} for source {key}"),
        };
        // For UTF-8 sources this searches the original bytes verbatim.
        assert!(
            !source.is_empty() && output.contains(source),
            "render omitted source {key}"
        );
    }
    let sources = loaded.manifest["sources"]
        .as_object()
        .expect("reviewed sources");
    let paths: BTreeSet<_> = sources
        .values()
        .flat_map(|source| {
            source["origins"]
                .as_array()
                .expect("source origins")
                .iter()
                .filter_map(|origin| origin["path"].as_str())
        })
        .collect();
    assert!(paths.contains("LICENSE-BoringSSL"));
    assert!(paths.contains("LICENSE-LUCIDE"));
    let materials = loaded.manifest["materials"]
        .as_array()
        .expect("reviewed materials");
    let material_names: BTreeSet<_> = materials
        .iter()
        .map(|item| item["name"].as_str().expect("material name"))
        .collect();
    assert!(material_names.contains("Microsoft rendering code MIT notice"));
    assert!(material_names.contains("shadcn theme palette MIT notice"));
    assert!(material_names.contains("display-link-derived Core Video glue MIT notice"));
    assert!(material_names.contains("Ghostty-derived macOS text-input setup MIT notice"));
    let mpl = loaded.manifest["mpl_source"]
        .as_array()
        .expect("reviewed MPL sources");
    let packages = loaded.manifest["packages"]
        .as_array()
        .expect("reviewed packages");
    for name in ["serialport", "option-ext", "cbindgen"] {
        let item = mpl
            .iter()
            .find(|item| item["name"] == name)
            .unwrap_or_else(|| panic!("missing MPL source {name}"));
        let package = packages
            .iter()
            .find(|item| item["name"] == name)
            .unwrap_or_else(|| panic!("missing reviewed package {name}"));
        assert_eq!(
            item["sha256"], package["checksum"],
            "MPL archive hash for {name}"
        );
        assert_eq!(
            item["url"], package["source_archive"],
            "MPL archive URL for {name}"
        );
    }
}
