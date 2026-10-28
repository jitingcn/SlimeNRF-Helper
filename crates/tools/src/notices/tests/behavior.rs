use super::*;

#[test]
fn source_only_check_writes_nothing_without_a_root_artifact() {
    let fixture = Fixture::new();
    let before = file_snapshot(fixture.root());
    execute(&fixture.check(None), &write_atomic).expect("source-only check succeeds");
    assert!(!fixture.path(OUTPUT).exists());
    assert_eq!(file_snapshot(fixture.root()), before);
}

#[test]
fn source_only_check_does_not_consult_or_replace_a_root_artifact() {
    let fixture = Fixture::new();
    fixture.put(OUTPUT, b"unrelated old artifact\n");
    let before = file_snapshot(fixture.root());
    execute(&fixture.check(None), &write_atomic).expect("source-only check ignores root artifact");
    assert_eq!(file_snapshot(fixture.root()), before);
}

#[test]
fn write_requires_an_explicit_destination() {
    let fixture = Fixture::new();
    assert_execute_error_unchanged(&fixture, &fixture.options(None), "output");
    assert!(!fixture.path(OUTPUT).exists());
}

#[test]
fn external_output_with_spaces_is_deterministic_and_byte_preserving() {
    let fixture = Fixture::new();
    let external = TempDir::new();
    let output = external.path().join("generated notices.txt");
    let options = fixture.options(Some(output.clone()));
    execute(&options, &write_atomic).expect("write external notice");
    let before = fs::read(&output).expect("read generated notice");
    execute(&options, &write_atomic).expect("repeat notice generation");
    assert_eq!(fs::read(&output).expect("read repeated notice"), before);
    execute(&fixture.check(Some(output)), &write_atomic).expect("check exact external notice");
    assert!(!fixture.path(OUTPUT).exists());
    assert_eq!(
        before
            .windows(fixture.upstream.len())
            .filter(|part| *part == fixture.upstream.as_slice())
            .count(),
        1
    );
    assert!(contains_bytes(&before, b"Application MIT terms\n"));
    assert!(contains_bytes(&before, b"Application Apache terms\n"));
}

#[test]
fn explicit_check_rejects_missing_output_without_creating_it() {
    let fixture = Fixture::new();
    let output = fixture.path("missing notices.txt");
    assert_execute_error_unchanged(&fixture, &fixture.check(Some(output.clone())), "stale");
    assert!(!output.exists());
}

#[test]
fn explicit_check_rejects_stale_output_without_rewriting_it() {
    let fixture = Fixture::new();
    fixture.put("stale notices.txt", b"old document\n");
    assert_execute_error_unchanged(
        &fixture,
        &fixture.check(Some(fixture.path("stale notices.txt"))),
        "stale",
    );
}

#[test]
fn stale_input_refuses_generation_and_both_check_modes_without_mutation() {
    let fixture = Fixture::new();
    let output = fixture.path("explicit notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    fixture.put("Cargo.toml", b"changed dependency\n");
    for options in [
        fixture.options(Some(output.clone())),
        fixture.check(None),
        fixture.check(Some(output.clone())),
    ] {
        assert_execute_error_unchanged(&fixture, &options, "Stale input hash: Cargo.toml");
    }
    assert!(!fixture.path(OUTPUT).exists());
}

#[test]
fn profile_refresh_preserves_reviewed_content_and_renders_updated_snapshot() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    let old_output = fs::read(&output).expect("read baseline notice");
    let before = fs::read(fixture.path(INPUT)).expect("read reviewed snapshot");
    for &name in MANIFESTS {
        fixture.append(name, b"\n# formatting\n");
        if name == "Cargo.toml" {
            fixture.append(name, b"[profile.dev]\nopt-level = 1\n");
        }
    }
    assert_execute_error_unchanged(&fixture, &fixture.check(None), "Stale input hash");
    let mut stale_check = fixture.check(Some(output.clone()));
    stale_check.refresh_inputs = true;
    assert_execute_error_unchanged(&fixture, &stale_check, "stale");

    let mut refresh = fixture.options(Some(output.clone()));
    refresh.refresh_inputs = true;
    refresh.cargo_rerun = true;
    execute(&refresh, &write_atomic).expect("refresh profile-only manifest edits");
    let after = fs::read(fixture.path(INPUT)).expect("read refreshed snapshot");
    let mut expected = before.clone();
    for &name in MANIFESTS {
        let hash = digest(&fs::read(fixture.path(name)).expect("read changed manifest"));
        expected = replace_first(&expected, &fixture.inputs[name], &hash);
    }
    assert_eq!(
        after, expected,
        "only the reviewed raw manifest hashes may change"
    );
    assert_eq!(
        fs::read(&output).expect("read refreshed notice"),
        replace_first(&old_output, &digest(&before), &digest(&after)),
        "notice must fingerprint the candidate, not the old snapshot"
    );
    execute(&fixture.check(Some(output)), &write_atomic).expect("refreshed notice checks");
    assert_eq!(
        fs::read(fixture.path(INPUT)).expect("read refreshed snapshot"),
        after
    );
}

#[test]
fn check_can_refresh_without_an_output() {
    let fixture = Fixture::new();
    fixture.append("Cargo.toml", b"\n[profile.release]\nlto = true\n");
    let before = fs::read(fixture.path(INPUT)).expect("read original snapshot");
    let mut options = fixture.check(None);
    options.refresh_inputs = true;
    execute(&options, &write_atomic).expect("check and refresh without output");
    assert_ne!(
        fs::read(fixture.path(INPUT)).expect("read refreshed snapshot"),
        before
    );
    execute(&fixture.check(None), &write_atomic).expect("check refreshed snapshot");
    assert!(!fixture.path(OUTPUT).exists());
}

#[test]
fn check_with_matching_candidate_commits_after_comparison() {
    let fixture = Fixture::new();
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 2\n");
    let loaded = load_inputs(fixture.root(), true).expect("load profile change for refresh");
    let output = fixture.path("notices.txt");
    fs::write(
        &output,
        render(fixture.root(), &loaded).expect("render candidate"),
    )
    .expect("install matching candidate notice");
    assert_eq!(
        fs::read(fixture.path(INPUT)).expect("read snapshot"),
        loaded.original
    );
    let mut options = fixture.check(Some(output.clone()));
    options.refresh_inputs = true;
    execute(&options, &write_atomic).expect("matching candidate permits snapshot commit");
    assert_eq!(
        fs::read(fixture.path(INPUT)).expect("read snapshot"),
        loaded.candidate
    );
    execute(&fixture.check(Some(output)), &write_atomic).expect("committed notice checks");
}

#[test]
fn manifest_fingerprint_uses_canonical_non_profile_toml() {
    let manifest = b"[package]\nname = \"example\"\n[profile.dev]\nopt-level = 3\n";
    let canonical = b"{\"package\":{\"name\":\"example\"}}";
    assert_eq!(
        semantic_manifest_hash(manifest).expect("fingerprint manifest"),
        digest(canonical)
    );
    assert_ne!(
        semantic_manifest_hash(b"[package]\nname = \"other\"\n")
            .expect("fingerprint changed manifest"),
        digest(canonical)
    );
}

#[test]
fn manifest_fingerprint_uses_python_ascii_escaped_unicode_canonicalization() {
    let manifest =
        "[package]\nname = \"café 🚀\"\ndescription = \"\\t雪\"\n[profile.dev]\nopt-level = 2\n";
    let canonical = br#"{"package":{"description":"\t\u96ea","name":"caf\u00e9 \ud83d\ude80"}}"#;
    assert_eq!(
        semantic_manifest_hash(manifest.as_bytes()).expect("fingerprint Unicode TOML"),
        digest(canonical)
    );
}

#[test]
fn manifest_fingerprint_uses_python_float_and_nonfinite_notation() {
    let manifest = b"[workspace.metadata]\ndecimal = 1.0\nlarge = 1e16\nnegative_zero = -0.0\nregular = 0.0001\nsmall = 1e-7\n";
    let canonical = br#"{"workspace":{"metadata":{"decimal":1.0,"large":1e+16,"negative_zero":-0.0,"regular":0.0001,"small":1e-07}}}"#;
    assert_eq!(
        semantic_manifest_hash(manifest).expect("fingerprint finite float values"),
        digest(canonical)
    );
    let nonfinite =
        b"[workspace.metadata]\ninfinity = inf\nnegative_infinity = -inf\nnot_a_number = nan\n";
    let canonical = br#"{"workspace":{"metadata":{"infinity":Infinity,"negative_infinity":-Infinity,"not_a_number":NaN}}}"#;
    assert_eq!(
        semantic_manifest_hash(nonfinite).expect("fingerprint nonfinite float values"),
        digest(canonical)
    );
}

#[test]
fn extracted_manifests_are_reviewed_and_watched() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    let paths =
        cargo_rerun_paths(fixture.root(), &fixture.load().manifest).expect("list cargo watches");
    for name in [
        "crates/firmware-image/Cargo.toml",
        "crates/terminal-core/Cargo.toml",
        "crates/tools/Cargo.toml",
    ] {
        assert!(
            paths.contains(&fixture.path(name)),
            "missing watch for {name}"
        );
        let original = fs::read(fixture.path(name)).expect("read member manifest");
        fixture.append(name, b"\n[dependencies]\nnew = \"1\"\n");
        assert_execute_error_unchanged(
            &fixture,
            &fixture.options(Some(output.clone())),
            &format!("Stale input hash: {name}"),
        );
        let mut refresh = fixture.options(Some(output.clone()));
        refresh.refresh_inputs = true;
        assert_execute_error_unchanged(
            &fixture,
            &refresh,
            &format!("Manifest semantics changed: {name}"),
        );
        fixture.put(name, &original);
    }
}

#[test]
fn extracted_manifests_require_inventory_and_semantic_fingerprints() {
    for section in ["input_sha256", "manifest_semantic_sha256"] {
        for name in [
            "crates/firmware-image/Cargo.toml",
            "crates/terminal-core/Cargo.toml",
            "crates/tools/Cargo.toml",
        ] {
            let mut fixture = Fixture::new();
            fixture.document[section]
                .as_object_mut()
                .expect("fixture metadata object")
                .remove(name);
            fixture.save();
            let mut options = fixture.check(None);
            options.refresh_inputs = true;
            let message = if section == "input_sha256" {
                "Missing mandatory"
            } else {
                "semantic"
            };
            assert_execute_error_unchanged(&fixture, &options, message);
            if section == "input_sha256" {
                assert!(fallback_rerun_paths(fixture.root()).contains(&fixture.path(name)));
                let output = fixture.path("pending.txt");
                let mut development = fixture.options(Some(output.clone()));
                development.development = true;
                development.cargo_rerun = true;
                let mut expected = file_snapshot(fixture.root());
                execute(&development, &write_atomic).expect("debug emits pending diagnostic");
                let pending = fs::read(output).expect("read diagnostic");
                assert!(contains_bytes(
                    &pending,
                    b"DEVELOPMENT LICENSE REVIEW PENDING"
                ));
                expected.insert(PathBuf::from("pending.txt"), pending);
                assert_eq!(file_snapshot(fixture.root()), expected);
            }
        }
    }
}

#[test]
fn cargo_records_cover_hashed_and_repository_origin_input_changes() {
    let mut fixture = Fixture::new();
    let hashed_name = "reviewed inputs/additional input.txt";
    let hashed = b"Additional reviewed input\n";
    fixture.put(hashed_name, hashed);
    fixture.document["input_sha256"][hashed_name] = json!(digest(hashed));
    let origin_name = "native kernels/source with spaces.c";
    let origin = b"Copyright Example\nPermission granted\nimplementation body\n";
    let excerpt = b"Copyright Example\nPermission granted\n";
    fixture.put(origin_name, origin);
    let key = fixture.source(excerpt, excerpt, "utf-8", json!({
        "kind": "repository", "path": origin_name, "file_sha256": digest(origin), "lines": [1, 2]
    }));
    fixture.material_notices().push(json!(key));
    fixture.save();
    let output = fixture.path("explicit notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    let paths =
        cargo_rerun_paths(fixture.root(), &fixture.load().manifest).expect("list cargo watches");
    let actual: BTreeSet<_> = paths.iter().cloned().collect();
    let mut expected = BTreeSet::from([
        fixture.path(INPUT),
        fixture.path("crates/tools/src/notices.rs"),
        fixture.path("crates/tools/src/notices/model.rs"),
        fixture.path("crates/tools/src/notices/render.rs"),
        fixture.path("crates/tools/src/notices/atomic.rs"),
        fixture.path(origin_name),
    ]);
    expected.extend(
        fixture.document["input_sha256"]
            .as_object()
            .expect("input hashes")
            .keys()
            .map(|name| fixture.path(name)),
    );
    assert_eq!(actual, expected);
    assert_eq!(
        paths.len(),
        expected.len(),
        "cargo watches must be deduplicated"
    );
    assert!(
        paths.windows(2).all(|pair| pair[0] < pair[1]),
        "cargo watches must be sorted"
    );
    for (name, original) in [
        (hashed_name, hashed.as_slice()),
        (origin_name, origin.as_slice()),
    ] {
        fixture.append(name, b"unreviewed change\n");
        assert_execute_error_unchanged(
            &fixture,
            &fixture.options(Some(output.clone())),
            &format!("Stale input hash: {name}"),
        );
        fixture.put(name, original);
    }
}

#[test]
fn failed_refresh_write_restores_output_and_development_emits_pending() {
    let fixture = Fixture::new();
    let output = fixture.path("notices.txt");
    execute(&fixture.options(Some(output.clone())), &write_atomic).expect("write baseline notice");
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let snapshot = fixture.path(INPUT);
    let before = file_snapshot(fixture.root());
    let fail_snapshot = |destination: &Path, data: &[u8]| {
        if destination == snapshot {
            Err("fixture cannot refresh reviewed inputs".to_owned())
        } else {
            write_atomic(destination, data)
        }
    };
    let mut options = fixture.options(Some(output.clone()));
    options.refresh_inputs = true;
    let error = execute(&options, &fail_snapshot).expect_err("snapshot write must fail");
    assert!(
        error.contains("fixture cannot refresh reviewed inputs"),
        "{error}"
    );
    assert_eq!(
        file_snapshot(fixture.root()),
        before,
        "failed refresh must roll back the old notice"
    );
    options.development = true;
    execute(&options, &fail_snapshot).expect("development emits diagnostic after failed refresh");
    let pending = fs::read(&output).expect("read pending diagnostic");
    assert!(contains_bytes(
        &pending,
        b"DEVELOPMENT LICENSE REVIEW PENDING"
    ));
    assert!(!contains_bytes(&pending, &fixture.upstream));
    assert_eq!(
        fs::read(&snapshot).expect("read snapshot"),
        before[Path::new(INPUT)]
    );
    let mut expected = before;
    expected.insert(PathBuf::from("notices.txt"), pending);
    assert_eq!(file_snapshot(fixture.root()), expected);
}

#[test]
fn failed_refresh_write_removes_a_new_output() {
    let fixture = Fixture::new();
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let output = fixture.path("new notices.txt");
    let snapshot = fixture.path(INPUT);
    let before = file_snapshot(fixture.root());
    let fail_snapshot = |destination: &Path, data: &[u8]| {
        if destination == snapshot {
            Err("snapshot is read-only".to_owned())
        } else {
            write_atomic(destination, data)
        }
    };
    let mut options = fixture.options(Some(output.clone()));
    options.refresh_inputs = true;
    assert!(execute(&options, &fail_snapshot).is_err());
    assert!(!output.exists());
    assert_eq!(file_snapshot(fixture.root()), before);
}

#[test]
fn output_failure_never_refreshes_input() {
    let fixture = Fixture::new();
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let output = fixture.path("missing directory/notices.txt");
    let mut options = fixture.options(Some(output.clone()));
    options.development = true;
    options.refresh_inputs = true;
    assert_execute_error_unchanged(&fixture, &options, "");
    assert!(!output.exists());
}

#[test]
fn development_diagnostic_is_not_a_notice_inventory() {
    let mut fixture = Fixture::new();
    fixture.document["sources"][fixture.upstream_key.as_str()]["base64"] =
        json!(base64(b"unreviewed"));
    fixture.save();
    let output = fixture.path("notices.txt");
    let mut options = fixture.options(Some(output.clone()));
    options.refresh_inputs = true;
    assert_execute_error_unchanged(&fixture, &options, "Notice source hash or length mismatch");
    let snapshot = fs::read(fixture.path(INPUT)).expect("read reviewed snapshot");
    let mut expected = file_snapshot(fixture.root());
    options.development = true;
    options.cargo_rerun = true;
    execute(&options, &write_atomic).expect("debug emits pending diagnostic");
    let pending = fs::read(output).expect("read pending diagnostic");
    assert!(contains_bytes(
        &pending,
        b"DEVELOPMENT LICENSE REVIEW PENDING"
    ));
    assert!(contains_bytes(&pending, b"not redistribution-ready"));
    assert!(contains_bytes(&pending, b"notice inventory is unavailable"));
    assert!(!contains_bytes(&pending, &fixture.upstream));
    assert!(!contains_bytes(&pending, b"unreviewed"));
    assert!(!contains_bytes(&pending, b"Package index"));
    assert_eq!(
        fs::read(fixture.path(INPUT)).expect("read snapshot"),
        snapshot
    );
    expected.insert(PathBuf::from("notices.txt"), pending);
    assert_eq!(file_snapshot(fixture.root()), expected);
    let mut check = fixture.check(None);
    check.development = true;
    assert_execute_error_unchanged(&fixture, &check, "development");
}

#[test]
fn pending_diagnostic_cannot_overwrite_reviewed_inputs() {
    let fixture = Fixture::new();
    fixture.put(INPUT, b"{ invalid json");
    for name in std::iter::once(INPUT).chain(VENDOR_LICENSES.iter().copied()) {
        let mut options = fixture.options(Some(fixture.path(name)));
        options.development = true;
        assert_execute_error_unchanged(&fixture, &options, "must not overwrite");
    }
    let paths = fallback_rerun_paths(fixture.root());
    for name in std::iter::once(INPUT)
        .chain(MANIFESTS.iter().copied())
        .chain(VENDOR_LICENSES.iter().copied())
    {
        assert!(
            paths.contains(&fixture.path(name)),
            "fallback omitted {name}"
        );
    }
    assert_eq!(paths.len(), paths.iter().collect::<BTreeSet<_>>().len());
}

#[test]
fn build_options_distinguish_debug_from_release() {
    let fixture = Fixture::new();
    let output = fixture.path("build output.txt");
    fixture.append("Cargo.toml", b"\n[profile.dev]\nopt-level = 1\n");
    let snapshot = fixture.path(INPUT);
    let before = fs::read(&snapshot).expect("read original snapshot");
    let mut debug = fixture.options(Some(output.clone()));
    debug.refresh_inputs = true;
    debug.development = true;
    debug.cargo_rerun = true;
    execute(&debug, &write_atomic).expect("debug profile refreshes profile-only edit");
    assert!(contains_bytes(
        &fs::read(&output).expect("read debug notice"),
        &fixture.upstream
    ));
    assert_ne!(
        fs::read(&snapshot).expect("read refreshed snapshot"),
        before
    );
    fixture.append("Cargo.toml", b"\n[workspace.dependencies]\nnew = \"1\"\n");
    fs::remove_file(&output).expect("remove debug notice");
    for profile in ["release", "custom-release"] {
        let mut release = fixture.options(Some(output.clone()));
        release.refresh_inputs = true;
        release.cargo_rerun = true;
        assert_execute_error_unchanged(&fixture, &release, "Manifest semantics changed");
        assert!(!output.exists(), "{profile} emitted an unreviewed notice");
    }
    let before = fs::read(&snapshot).expect("read snapshot before pending");
    let mut expected = file_snapshot(fixture.root());
    execute(&debug, &write_atomic).expect("debug tolerates pending review");
    let pending = fs::read(output).expect("read pending notice");
    assert!(contains_bytes(
        &pending,
        b"DEVELOPMENT LICENSE REVIEW PENDING"
    ));
    assert!(!contains_bytes(&pending, &fixture.upstream));
    assert_eq!(fs::read(snapshot).expect("read unchanged snapshot"), before);
    expected.insert(PathBuf::from("build output.txt"), pending);
    assert_eq!(file_snapshot(fixture.root()), expected);
}
