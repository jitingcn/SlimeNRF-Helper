use super::*;

#[test]
fn malformed_render_fields_do_not_commit_refreshed_snapshot_or_output() {
    for case in [
        "scope missing",
        "scope malformed",
        "mpl_source missing",
        "mpl item missing name",
        "source_instructions missing",
        "maintainer_review missing",
        "package source_archive missing",
        "package target_roles missing",
        "material scope missing",
        "repository malformed",
        "policy malformed",
        "origin lines malformed",
    ] {
        let mut fixture = Fixture::new();
        let field = match case {
            "scope missing" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture document")
                    .remove("scope");
                "scope"
            }
            "scope malformed" => {
                fixture.document["scope"] = json!(null);
                "scope"
            }
            "mpl_source missing" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture document")
                    .remove("mpl_source");
                "mpl_source"
            }
            "mpl item missing name" => {
                fixture.document["mpl_source"] = json!([{"version": "1", "url": "https://example.invalid/source", "sha256": "abc"}]);
                "name"
            }
            "source_instructions missing" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture document")
                    .remove("source_instructions");
                "source_instructions"
            }
            "maintainer_review missing" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture document")
                    .remove("maintainer_review");
                "maintainer_review"
            }
            "package source_archive missing" => {
                fixture.document["packages"][0]
                    .as_object_mut()
                    .expect("fixture package")
                    .remove("source_archive");
                "source_archive"
            }
            "package target_roles missing" => {
                fixture.document["packages"][0]
                    .as_object_mut()
                    .expect("fixture package")
                    .remove("target_roles");
                "target_roles"
            }
            "material scope missing" => {
                fixture.document["materials"][0]
                    .as_object_mut()
                    .expect("fixture material")
                    .remove("scope");
                "scope"
            }
            "repository malformed" => {
                fixture.document["packages"][0]["repository"] = json!({"not": "a string"});
                "repository"
            }
            "policy malformed" => {
                fixture.document["packages"][0]["policy"] = json!({"not": "a string"});
                "policy"
            }
            "origin lines malformed" => {
                fixture.document["sources"][fixture.upstream_key.as_str()]["origins"][0]["lines"] =
                    json!([1]);
                "lines"
            }
            _ => unreachable!("known presentation corruption case"),
        };
        fixture.save();
        fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
        fixture.put("preserved output.txt", b"existing notice\n");
        let mut options = fixture.options(Some(fixture.path("preserved output.txt")));
        options.refresh_inputs = true;
        let before = file_snapshot(fixture.root());
        let error = execute(&options, &write_atomic)
            .expect_err("invalid presentation must not commit refresh");
        assert!(
            error.contains(field),
            "{case}: expected {field:?} in {error:?}"
        );
        assert_eq!(
            file_snapshot(fixture.root()),
            before,
            "{case}: failed render changed files"
        );
    }
}

#[test]
fn absent_null_and_empty_optional_package_fields_are_omitted_without_mutation() {
    for optional in [None, Some(json!(null)), Some(json!(""))] {
        let mut fixture = Fixture::new();
        for field in ["repository", "policy"] {
            if let Some(value) = &optional {
                fixture.document["packages"][0][field] = value.clone();
            }
        }
        fixture.save();
        let before = file_snapshot(fixture.root());
        let output = render(fixture.root(), &fixture.load()).expect("render optional fields");
        assert!(contains_bytes(&output, b"===== example@1.2.3 ====="));
        assert!(!contains_bytes(&output, b"Upstream repository:"));
        assert!(!contains_bytes(&output, b"Recorded handling:"));
        assert_eq!(
            file_snapshot(fixture.root()),
            before,
            "rendering optional fields changed reviewed inputs"
        );
    }
}

struct ExpectedSource {
    used_by: &'static str,
    origins: Vec<String>,
    original_len: usize,
    rendered: Vec<u8>,
    transcoded: bool,
}

fn line(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(value.as_bytes());
    output.push(b'\n');
}

#[test]
fn full_document_preserves_order_attribution_original_text_and_candidate_fingerprint() {
    let mut fixture = Fixture::new();
    let origin_name = "native/source with spaces.c";
    let origin = b"header\r\nCopyright Example\r\nPermission granted\r\nimplementation body\r\n";
    let excerpt = b"Copyright Example\r\nPermission granted\r\n";
    fixture.put(origin_name, origin);
    let excerpt_key = fixture.source(excerpt, excerpt, "utf-8", json!({
        "kind": "repository", "path": origin_name, "file_sha256": digest(origin), "lines": [2, 3]
    }));
    fixture.document["sources"][excerpt_key.as_str()]["origins"]
        .as_array_mut()
        .expect("excerpt origins")
        .push(json!({"kind": "published_crate", "url": "https://example.invalid/excerpt"}));
    let legacy = b"Patent \x93license\x94.\r\n";
    let transcoded = "Patent “license”.\r\n".as_bytes();
    let legacy_key = fixture.source(
        legacy,
        transcoded,
        "windows-1252",
        json!({
            "kind": "published_crate", "url": "https://example.invalid/PATENTS"
        }),
    );
    fixture
        .package_notices()
        .extend([json!(excerpt_key), json!(legacy_key)]);
    fixture.material_notices().push(json!(excerpt_key));
    fixture.document["packages"][0]["repository"] = json!("https://example.invalid/repository");
    fixture.document["packages"][0]["policy"] = json!("Fixture policy");
    fixture.document["scope"] = json!(["Fixture scope", "Second fixture scope"]);
    fixture.document["mpl_source"] = json!([{
        "name": "covered-crate", "version": "4.2.0",
        "url": "https://example.invalid/covered.crate", "sha256": "archive-hash"
    }]);
    fixture.document["source_instructions"] =
        json!(["Retrieve covered source.", "Retain the archive."]);
    fixture.document["maintainer_review"] =
        json!(["Fixture review boundary", "Second review boundary"]);
    fixture.save();
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let loaded = load_inputs(fixture.root(), true).expect("load reviewed fixture and candidate");
    assert_ne!(
        loaded.original, loaded.candidate,
        "fixture needs a changed candidate"
    );

    let mut sources = BTreeMap::new();
    sources.insert(
        fixture.upstream_key.clone(),
        ExpectedSource {
            used_by: "example@1.2.3",
            origins: vec!["https://example.invalid/LICENSE".to_owned()],
            original_len: fixture.upstream.len(),
            rendered: fixture.upstream.clone(),
            transcoded: false,
        },
    );
    sources.insert(
        excerpt_key.clone(),
        ExpectedSource {
            used_by: "example@1.2.3, Native kernels",
            origins: vec![
                format!("{origin_name} lines 2-3"),
                "https://example.invalid/excerpt".to_owned(),
            ],
            original_len: excerpt.len(),
            rendered: excerpt.to_vec(),
            transcoded: false,
        },
    );
    sources.insert(
        legacy_key.clone(),
        ExpectedSource {
            used_by: "example@1.2.3",
            origins: vec!["https://example.invalid/PATENTS".to_owned()],
            original_len: legacy.len(),
            rendered: transcoded.to_vec(),
            transcoded: true,
        },
    );
    let mut material_keys = Vec::new();
    for &name in VENDOR_LICENSES {
        let bytes = fs::read(fixture.path(name)).expect("read fixture vendor source");
        let key = digest(&bytes);
        material_keys.push(key.clone());
        sources.insert(
            key,
            ExpectedSource {
                used_by: "Native kernels",
                origins: vec![name.to_owned()],
                original_len: bytes.len(),
                rendered: bytes,
                transcoded: false,
            },
        );
    }
    material_keys.push(excerpt_key.clone());

    let mut expected = Vec::new();
    line(
        &mut expected,
        "SlimeNRF Helper licenses and third-party notices",
    );
    line(
        &mut expected,
        "Generated by slimenrf-tools notices. Do not edit this document.",
    );
    line(
        &mut expected,
        &format!("Reviewed input SHA-256: {}", digest(&loaded.candidate)),
    );
    line(
        &mut expected,
        &format!("Cargo.lock SHA-256: {}", fixture.inputs["Cargo.lock"]),
    );
    line(&mut expected, "");
    line(&mut expected, "Fixture scope");
    line(&mut expected, "");
    line(&mut expected, "Second fixture scope");
    line(&mut expected, "");
    line(&mut expected, "Application license alternatives");
    line(&mut expected, "===== LICENSE-MIT =====");
    expected.extend_from_slice(b"Application MIT terms\n");
    line(&mut expected, "");
    line(&mut expected, "===== LICENSE-APACHE =====");
    expected.extend_from_slice(b"Application Apache terms\n");
    line(&mut expected, "");
    line(&mut expected, "MPL-2.0 covered source");
    line(&mut expected, "covered-crate 4.2.0");
    line(
        &mut expected,
        "Unmodified covered source archive: https://example.invalid/covered.crate",
    );
    line(&mut expected, "Archive SHA-256: archive-hash");
    line(&mut expected, "Retrieve covered source.");
    line(&mut expected, "Retain the archive.");
    line(&mut expected, "");
    line(&mut expected, "Package index");
    line(&mut expected, "===== example@1.2.3 =====");
    line(
        &mut expected,
        "Original license declaration: MIT AND BSD-3-Clause",
    );
    line(
        &mut expected,
        "Source archive: https://example.invalid/example-1.2.3.crate",
    );
    line(&mut expected, "Archive SHA-256: abc123");
    line(
        &mut expected,
        "Upstream repository: https://example.invalid/repository",
    );
    line(
        &mut expected,
        "Captured roles: linux: compile_tool; windows: code_candidate",
    );
    line(&mut expected, "Recorded handling: Fixture policy");
    line(
        &mut expected,
        &format!(
            "Notice source SHA-256: {}, {excerpt_key}, {legacy_key}",
            fixture.upstream_key
        ),
    );
    line(&mut expected, "");
    line(
        &mut expected,
        "Additional code, asset, and native kernel materials",
    );
    line(&mut expected, "Native kernels");
    line(&mut expected, "Both variants");
    line(
        &mut expected,
        &format!("Notice source SHA-256: {}", material_keys.join(", ")),
    );
    line(&mut expected, "");
    line(&mut expected, "Complete notice texts");
    line(
        &mut expected,
        "Identical source texts are reproduced once and referenced by SHA-256 above.",
    );
    line(
        &mut expected,
        "Alternative license texts are retained as evidence, not relicensing of other material.",
    );
    for (key, source) in &sources {
        line(&mut expected, "");
        line(&mut expected, &format!("===== Notice {key} ====="));
        line(&mut expected, &format!("Used by: {}", source.used_by));
        for origin in &source.origins {
            line(&mut expected, &format!("Source: {origin}"));
        }
        if source.transcoded {
            line(
                &mut expected,
                "Original encoding: windows-1252. Transcoded to UTF-8 without changing the text.",
            );
            line(
                &mut expected,
                &format!("UTF-8 SHA-256: {}", digest(&source.rendered)),
            );
        }
        line(
            &mut expected,
            &format!(
                "----- Begin complete text ({} original bytes) -----",
                source.original_len
            ),
        );
        expected.extend_from_slice(&source.rendered);
        line(&mut expected, "");
        line(&mut expected, "----- End complete text -----");
    }
    line(&mut expected, "");
    line(&mut expected, "Maintainer review boundaries");
    line(&mut expected, "Fixture review boundary");
    line(&mut expected, "Second review boundary");

    assert_eq!(
        render(fixture.root(), &loaded).expect("render golden document"),
        expected
    );
    let output = fixture.path("rendered notice.txt");
    let mut expected_files = file_snapshot(fixture.root());
    expected_files.insert(PathBuf::from(INPUT), loaded.candidate);
    expected_files.insert(PathBuf::from("rendered notice.txt"), expected.clone());
    let mut options = fixture.options(Some(output.clone()));
    options.refresh_inputs = true;
    execute(&options, &write_atomic).expect("commit golden candidate and notice");
    assert_eq!(fs::read(output).expect("read rendered notice"), expected);
    assert_eq!(file_snapshot(fixture.root()), expected_files);
}

#[test]
fn empty_display_strings_preserve_frozen_rendering_behavior() {
    let mut fixture = Fixture::new();
    fixture.document["packages"][0]["source_archive"] = json!("");
    fixture.document["packages"][0]["repository"] = json!("");
    fixture.document["packages"][0]["policy"] = json!("");
    fixture.document["sources"][fixture.upstream_key.as_str()]["origins"][0]["url"] = json!("");
    fixture.document["scope"] = json!(["", "Fixture scope"]);
    fixture.document["source_instructions"] = json!([""]);
    fixture.document["maintainer_review"] = json!([""]);
    fixture.save();
    let original = fs::read(fixture.path(INPUT)).expect("read fixture input");
    let before = file_snapshot(fixture.root());
    let first = render(fixture.root(), &fixture.load()).expect("render empty display strings");
    assert_eq!(
        file_snapshot(fixture.root()),
        before,
        "render unexpectedly wrote fixture files"
    );
    assert!(contains_bytes(&first, b"Source archive: \n"));
    assert!(contains_bytes(&first, b"Source: \n"));
    assert!(!contains_bytes(&first, b"Upstream repository:"));
    assert!(!contains_bytes(&first, b"Recorded handling:"));
    assert!(contains_bytes(
        &first,
        b"\n\n\nFixture scope\n\nApplication license alternatives\n"
    ));
    assert!(first.ends_with(b"Maintainer review boundaries\n\n"));

    fixture.document["packages"][0]
        .as_object_mut()
        .expect("fixture package")
        .remove("repository");
    fixture.document["packages"][0]
        .as_object_mut()
        .expect("fixture package")
        .remove("policy");
    fixture.save();
    let updated = fs::read(fixture.path(INPUT)).expect("read updated fixture input");
    let second = render(fixture.root(), &fixture.load()).expect("render absent optional strings");
    assert_eq!(
        second,
        replace_first(&first, &digest(&original), &digest(&updated))
    );
    let mut expected = file_snapshot(fixture.root());
    expected.insert(PathBuf::from("notice.txt"), second);
    execute(
        &fixture.options(Some(fixture.path("notice.txt"))),
        &write_atomic,
    )
    .expect("write notice with empty display strings");
    assert_eq!(file_snapshot(fixture.root()), expected);
}

#[test]
fn absent_null_and_empty_origin_ranges_render_identically() {
    let mut fixture = Fixture::new();
    let vendor_bytes = fs::read(fixture.path(VENDOR_LICENSES[0])).expect("read vendor source");
    let vendor_key = digest(&vendor_bytes);
    let original = fs::read(fixture.path(INPUT)).expect("read original input");
    let baseline = render(fixture.root(), &fixture.load()).expect("render origins without ranges");
    for value in [Value::Null, json!([])] {
        fixture.document["sources"][fixture.upstream_key.as_str()]["origins"][0]["lines"] =
            value.clone();
        fixture.document["sources"][vendor_key.as_str()]["origins"][0]["lines"] = value;
        fixture.save();
        let updated = fs::read(fixture.path(INPUT)).expect("read updated input");
        let before = file_snapshot(fixture.root());
        let rendered = render(fixture.root(), &fixture.load()).expect("render empty origin ranges");
        assert_eq!(
            rendered,
            replace_first(&baseline, &digest(&original), &digest(&updated))
        );
        execute(&fixture.check(None), &write_atomic).expect("validate empty origin ranges");
        assert_eq!(file_snapshot(fixture.root()), before);
    }
}
