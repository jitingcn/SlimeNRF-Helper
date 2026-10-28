use super::*;
use std::cell::Cell;

#[test]
fn refresh_rejects_dependency_package_lock_and_vendor_changes() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    for (name, addition, message) in [
        (
            "Cargo.toml",
            b"\n[workspace.dependencies]\nnew = \"1\"\n".as_slice(),
            "Manifest semantics changed",
        ),
        (
            "Cargo.toml",
            b"\n[workspace.metadata]\nversion = \"new\"\n".as_slice(),
            "Manifest semantics changed",
        ),
        (
            "crates/app/Cargo.toml",
            b"\n[dependencies]\nnew = \"1\"\n".as_slice(),
            "Manifest semantics changed",
        ),
        (
            "crates/recording/Cargo.toml",
            b"version = \"2.0.0\"\n".as_slice(),
            "Manifest semantics changed",
        ),
        (
            "crates/tools/Cargo.toml",
            b"\n[dependencies]\nnew = \"1\"\n".as_slice(),
            "Manifest semantics changed",
        ),
        (
            "Cargo.lock",
            b"\n# changed lock\n".as_slice(),
            "Stale input hash: Cargo.lock",
        ),
        (
            VENDOR_LICENSES[0],
            b"changed source\n".as_slice(),
            "Stale input hash:",
        ),
    ] {
        let original = fs::read(fixture.path(name)).expect("read fixture input");
        fixture.append(name, addition);
        let mut options = fixture.options(Some(output.clone()));
        options.refresh_inputs = true;
        let before = file_snapshot(fixture.root());
        let error = execute(&options, &write_atomic).expect_err("changed reviewed input must fail");
        assert!(
            error.contains(message),
            "{name}: expected {message:?} in {error:?}"
        );
        assert!(
            error.contains(REVIEW_GUIDE),
            "{name}: missing review instructions in {error:?}"
        );
        assert_eq!(
            file_snapshot(fixture.root()),
            before,
            "{name}: failed refresh wrote files"
        );
        fixture.put(name, &original);
    }
}

#[test]
fn refresh_never_blesses_corruption_missing_metadata_or_coverage() {
    for case in ["source", "coverage", "fingerprint", "render"] {
        let mut fixture = Fixture::new();
        fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 2\n");
        let message = match case {
            "source" => {
                fixture.document["sources"][fixture.upstream_key.as_str()]["base64"] =
                    json!(base64(b"corrupted source"));
                "Notice source hash or length mismatch"
            }
            "coverage" => {
                fixture.document["packages"] = json!([]);
                "Package coverage differs"
            }
            "fingerprint" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture object")
                    .remove("manifest_semantic_sha256");
                "semantic"
            }
            "render" => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture object")
                    .remove("scope");
                "scope"
            }
            _ => unreachable!("known corruption case"),
        };
        fixture.save();
        let mut options = fixture.check(None);
        options.refresh_inputs = true;
        assert_execute_error_unchanged(&fixture, &options, message);
    }
}

#[test]
fn missing_or_corrupt_fingerprint_fails_strict_check_read_only() {
    for fingerprint in [None, Some("not-a-sha256".to_owned()), Some("f".repeat(64))] {
        let mut fixture = Fixture::new();
        match fingerprint {
            None => {
                fixture
                    .document
                    .as_object_mut()
                    .expect("fixture object")
                    .remove("manifest_semantic_sha256");
            }
            Some(value) => {
                fixture.document["manifest_semantic_sha256"]["Cargo.toml"] = json!(value)
            }
        }
        fixture.save();
        let before = file_snapshot(fixture.root());
        let error =
            execute(&fixture.check(None), &write_atomic).expect_err("fingerprint must be rejected");
        assert!(error.contains("semantic"), "{error}");
        assert!(error.contains(REVIEW_GUIDE), "{error}");
        assert_eq!(file_snapshot(fixture.root()), before);
    }
}

#[test]
fn changed_manifest_requires_review() {
    let fixture = Fixture::new();
    fixture.put("Cargo.toml", b"changed dependency\n");
    assert_load_error_unchanged(&fixture, false, "Stale input hash: Cargo.toml");
}

#[test]
fn changed_lock_requires_review() {
    let fixture = Fixture::new();
    fixture.put("Cargo.lock", b"changed lock\n");
    assert_load_error_unchanged(&fixture, false, "Stale input hash: Cargo.lock");
}

#[test]
fn missing_graph_package_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["packages"] = json!([]);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Package coverage differs");
}

#[test]
fn extra_and_duplicate_packages_are_rejected() {
    let mut fixture = Fixture::new();
    let package = fixture.document["packages"][0].clone();
    fixture.document["packages"]
        .as_array_mut()
        .expect("fixture packages")
        .push(package.clone());
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Duplicate package");
    fixture.document["packages"][1]["name"] = json!("extra");
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Package coverage differs");
}

#[test]
fn missing_package_notice_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["packages"][0]["notices"] = json!([]);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Unhandled package");
}

#[test]
fn changed_archive_checksum_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["packages"][0]["checksum"] = json!("wrong");
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Locked archive checksum differs");
}

#[test]
fn corrupted_source_bytes_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["sources"][fixture.upstream_key.as_str()]["base64"] =
        json!(base64(b"altered"));
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Notice source hash or length mismatch");
}

#[test]
fn malformed_source_metadata_is_rejected_before_refresh() {
    for (field, value, message) in [
        ("base64", json!("not base64!"), "base64"),
        (
            "sha256",
            json!("incorrect"),
            "Notice source hash or length mismatch",
        ),
        ("bytes", json!(999), "Notice source hash or length mismatch"),
        (
            "utf8_sha256",
            json!("incorrect"),
            "Rendered notice hash mismatch",
        ),
        (
            "encoding",
            json!("unreviewed-encoding"),
            "Unreviewed source encoding",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
        fixture.document["sources"][fixture.upstream_key.as_str()][field] = value;
        fixture.save();
        assert_load_error_unchanged(&fixture, true, message);
    }
}

#[test]
fn missing_source_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["sources"]
        .as_object_mut()
        .expect("fixture sources")
        .remove(&fixture.upstream_key);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Missing package notice source");
}

#[test]
fn missing_material_and_unreferenced_sources_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.document["materials"][0]["notices"] = json!([]);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Missing material notice");

    let mut fixture = Fixture::new();
    fixture
        .material_notices()
        .push(json!("not-a-reviewed-source"));
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Missing material notice source");

    let mut fixture = Fixture::new();
    let unused = b"unreferenced reviewed text\n";
    fixture.source(
        unused,
        unused,
        "utf-8",
        json!({"kind": "published_crate", "url": "https://example.invalid/unused"}),
    );
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Unreferenced reviewed notice source");
}

#[test]
fn vendor_licenses_are_mandatory() {
    for &name in VENDOR_LICENSES {
        let mut fixture = Fixture::new();
        fixture.document["input_sha256"]
            .as_object_mut()
            .expect("input hashes")
            .remove(name);
        fixture.save();
        assert_load_error_unchanged(&fixture, false, "Missing mandatory");
    }
}

#[test]
fn vendor_notice_cannot_be_replaced_with_unrelated_text() {
    let mut fixture = Fixture::new();
    let key = fixture.document["materials"][0]["notices"][0]
        .as_str()
        .expect("vendor source key")
        .to_owned();
    fixture.document["sources"][key.as_str()]["origins"][0]["path"] = json!("LICENSE-MIT");
    fixture.document["sources"][key.as_str()]["origins"][0]["file_sha256"] =
        json!(fixture.inputs["LICENSE-MIT"]);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Mandatory native kernel license omitted");
}

#[test]
fn non_utf8_original_is_preserved_and_explicitly_transcoded() {
    let mut fixture = Fixture::new();
    let original = b"Patent terms call this \x93License\x94.\r\n";
    let utf8 = "Patent terms call this “License”.\r\n".as_bytes();
    let key = fixture.source(
        original,
        utf8,
        "windows-1252",
        json!({"kind": "published_crate", "url": "https://example.invalid/PATENTS"}),
    );
    fixture.package_notices().push(json!(key));
    fixture.save();
    let loaded = fixture.load();
    let output = render(fixture.root(), &loaded).expect("render transcoded source");
    assert_eq!(loaded.sources[&key].as_slice(), original.as_slice());
    assert!(contains_bytes(&output, utf8));
    assert!(contains_bytes(
        &output,
        b"Original encoding: windows-1252. Transcoded to UTF-8"
    ));
    assert!(contains_bytes(&output, key.as_bytes()));
}

#[test]
fn undefined_windows_1252_bytes_and_invalid_utf8_are_rejected() {
    for byte in [0x81, 0x8d, 0x8f, 0x90, 0x9d] {
        let mut fixture = Fixture::new();
        let key = fixture.source(
            &[byte],
            b"",
            "windows-1252",
            json!({"kind": "published_crate", "url": "https://example.invalid/invalid"}),
        );
        fixture.package_notices().push(json!(key));
        fixture.save();
        assert_load_error_unchanged(&fixture, false, "Invalid windows-1252 notice source");
    }
    let mut fixture = Fixture::new();
    let key = fixture.source(
        &[0xff],
        b"",
        "utf-8",
        json!({"kind": "published_crate", "url": "https://example.invalid/invalid"}),
    );
    fixture.package_notices().push(json!(key));
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Invalid utf-8 notice source");
}

#[test]
fn repository_grant_excerpt_is_checked_against_actual_source() {
    let mut fixture = Fixture::new();
    let name = "native/header.c";
    let original =
        b"implementation header\nCopyright Example\nPermission granted\nimplementation body\n";
    let excerpt = b"Copyright Example\nPermission granted\n";
    fixture.put(name, original);
    fixture.document["input_sha256"][name] = json!(digest(original));
    let key = fixture.source(
        excerpt,
        excerpt,
        "utf-8",
        json!({
            "kind": "repository", "path": name, "file_sha256": digest(original), "lines": [2, 3]
        }),
    );
    fixture.material_notices().push(json!(key));
    fixture.save();
    let output = render(fixture.root(), &fixture.load()).expect("render reviewed excerpt");
    assert!(contains_bytes(&output, excerpt));
    assert!(!contains_bytes(&output, b"implementation body"));
    fixture.document["sources"][key.as_str()]["origins"][0]["lines"] = json!([1, 2]);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Repository notice source differs");
}

#[test]
fn paths_cannot_depend_on_local_absolute_directories() {
    let absolute_drive_path = ["C:", "/absolute/license"].concat();
    for name in [
        absolute_drive_path.as_str(),
        "/absolute/license",
        "../license",
        "source/../license",
        "source\\license",
        "source:license",
        "source\rlicense",
        "source\nlicense",
        "source\0license",
    ] {
        let mut fixture = Fixture::new();
        fixture.document["input_sha256"][name] = json!("unknown");
        fixture.save();
        assert_load_error_unchanged(&fixture, false, "Nonportable input path");
        let before = file_snapshot(fixture.root());
        assert!(
            cargo_rerun_paths(fixture.root(), &fixture.document).is_err(),
            "watch accepted {name:?}"
        );
        assert_eq!(file_snapshot(fixture.root()), before);
    }
}

#[test]
fn duplicate_json_keys_and_unsupported_schema_are_rejected_without_writes() {
    let mut fixture = Fixture::new();
    let original = fs::read(fixture.path(INPUT)).expect("read fixture input");
    let mut duplicate = b"{\"schema\":1,".to_vec();
    duplicate.extend_from_slice(&original[1..]);
    fixture.put(INPUT, &duplicate);
    assert_load_error_unchanged(&fixture, false, "Duplicate key");
    fixture.document["schema"] = json!(2);
    fixture.save();
    assert_load_error_unchanged(&fixture, false, "Unsupported notices input schema");
}

#[test]
fn duplicate_nested_json_keys_are_rejected_before_refresh_writes() {
    for location in ["input_sha256", "source"] {
        let fixture = Fixture::new();
        let original = fs::read(fixture.path(INPUT)).expect("read fixture input");
        let duplicate = if location == "input_sha256" {
            replace_first(
                &original,
                "\"input_sha256\":{",
                "\"input_sha256\":{\"Cargo.toml\":\"duplicate\",",
            )
        } else {
            let entry = format!("\"sha256\":\"{}\"", fixture.upstream_key);
            replace_first(&original, &entry, &format!("{entry},{entry}"))
        };
        fixture.put(INPUT, &duplicate);
        fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
        fixture.put("preserved output.txt", b"previous output\n");
        let mut options = fixture.options(Some(fixture.path("preserved output.txt")));
        options.refresh_inputs = true;
        assert_execute_error_unchanged(&fixture, &options, "Duplicate key");
    }
}

#[test]
fn malformed_mandatory_raw_manifest_hash_is_not_refreshed() {
    for value in [
        Value::Null,
        json!(123),
        json!("f".repeat(63)),
        json!("z".repeat(64)),
        json!("A".repeat(64)),
    ] {
        let mut fixture = Fixture::new();
        fixture.document["input_sha256"]["Cargo.toml"] = value;
        fixture.save();
        fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
        fixture.put("preserved output.txt", b"previous output\n");
        let mut options = fixture.options(Some(fixture.path("preserved output.txt")));
        options.refresh_inputs = true;
        let before = file_snapshot(fixture.root());
        let calls = Cell::new(0);
        let writer = |path: &Path, bytes: &[u8]| {
            calls.set(calls.get() + 1);
            write_atomic(path, bytes)
        };
        execute(&options, &writer).expect_err("malformed raw hash must be rejected");
        assert_eq!(calls.get(), 0, "malformed hash reached the writer");
        assert_eq!(
            file_snapshot(fixture.root()),
            before,
            "malformed hash changed files"
        );
    }
}

#[test]
fn refresh_preserves_bom_and_unrelated_json_bytes() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.path(INPUT)).expect("read fixture input");
    let mut with_bom = b"\xef\xbb\xbf".to_vec();
    with_bom.extend_from_slice(&original);
    fixture.put(INPUT, &with_bom);
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let loaded = load_inputs(fixture.root(), true).expect("load UTF-8 BOM input");
    let hash = digest(&fs::read(fixture.path("Cargo.toml")).expect("read changed manifest"));
    assert_eq!(loaded.original, with_bom);
    assert_eq!(
        loaded.candidate,
        replace_first(&with_bom, &fixture.inputs["Cargo.toml"], &hash)
    );
    let mut options = fixture.check(None);
    options.refresh_inputs = true;
    execute(&options, &write_atomic).expect("commit exact BOM-preserving candidate");
    assert_eq!(
        fs::read(fixture.path(INPUT)).expect("read refreshed input"),
        loaded.candidate
    );
}

#[cfg(any(unix, windows))]
fn symlink_if_supported(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    match result {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::PermissionDenied | ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(1314) =>
        {
            false
        }
        Err(error) => panic!("create fixture symlink {}: {error}", link.display()),
    }
}

#[cfg(not(any(unix, windows)))]
fn symlink_if_supported(_: &Path, _: &Path) -> bool {
    false
}

#[test]
fn strict_and_pending_outputs_cannot_overwrite_input_paths_or_aliases() {
    let fixture = Fixture::new();
    let mut targets: Vec<_> = [
        INPUT,
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE-MIT",
        VENDOR_LICENSES[0],
    ]
    .into_iter()
    .map(|name| fixture.path(name))
    .collect();
    let hard_link = fixture.path("input hard link.txt");
    fs::hard_link(fixture.path(INPUT), &hard_link).expect("create same-volume fixture hard link");
    targets.push(hard_link);
    let symlink = fixture.path("input symbolic link.txt");
    if symlink_if_supported(&fixture.path(INPUT), &symlink) {
        targets.push(symlink);
    }
    for path in &targets {
        assert_execute_error_unchanged(
            &fixture,
            &fixture.options(Some(path.clone())),
            "must not overwrite",
        );
    }
    fixture.put(INPUT, b"{ invalid json");
    for path in &targets {
        let mut options = fixture.options(Some(path.clone()));
        options.development = true;
        assert_execute_error_unchanged(&fixture, &options, "must not overwrite");
    }
    let missing_input = fixture.path("Cargo.lock");
    fs::remove_file(&missing_input).expect("remove fixture lockfile");
    let mut options = fixture.options(Some(missing_input.clone()));
    options.development = true;
    assert_execute_error_unchanged(&fixture, &options, "must not overwrite");
    assert!(
        !missing_input.exists(),
        "pending output recreated a missing reviewed input"
    );
}

#[test]
fn concurrently_changed_snapshot_is_not_overwritten_and_output_is_restored() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline output");
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let before_output = fs::read(&output).expect("read baseline output");
    let mut expected = file_snapshot(fixture.root());
    let snapshot = fixture.path(INPUT);
    let external = b"external edit during generation\n";
    let injected = Cell::new(false);
    let writer = |destination: &Path, data: &[u8]| {
        write_atomic(destination, data)?;
        if destination == output && !injected.replace(true) {
            fs::write(&snapshot, external).map_err(|error| error.to_string())?;
        }
        Ok(())
    };
    let mut options = fixture.options(Some(output.clone()));
    options.refresh_inputs = true;
    let error = execute(&options, &writer).expect_err("concurrent snapshot edit must fail");
    assert!(error.contains("changed during generation"), "{error}");
    assert_eq!(fs::read(&snapshot).expect("read external edit"), external);
    assert_eq!(
        fs::read(output).expect("read restored output"),
        before_output
    );
    expected.insert(PathBuf::from(INPUT), external.to_vec());
    assert_eq!(file_snapshot(fixture.root()), expected);
}

#[test]
fn atomic_write_replaces_existing_bytes_and_leaves_no_temporary_files() {
    let fixture = Fixture::new();
    let output = fixture.path("atomic destination.txt");
    fs::write(&output, b"old\n").expect("create existing output");
    let mut expected = file_snapshot(fixture.root());
    expected.insert(
        PathBuf::from("atomic destination.txt"),
        b"replacement\n".to_vec(),
    );
    write_atomic(&output, b"replacement\n").expect("replace existing output");
    assert_eq!(file_snapshot(fixture.root()), expected);
    let missing = fixture.path("absent directory/output.txt");
    assert!(write_atomic(&missing, b"unused").is_err());
    assert_eq!(file_snapshot(fixture.root()), expected);
}

#[test]
fn public_write_notices_is_strict_and_never_refreshes() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    write_notices(fixture.root(), &output).expect("write strict notice for valid inputs");
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let before = file_snapshot(fixture.root());
    let error = write_notices(fixture.root(), &output)
        .expect_err("strict write must reject stale manifest");
    assert!(error.contains("Stale input hash: Cargo.toml"), "{error}");
    assert_eq!(file_snapshot(fixture.root()), before);
}
