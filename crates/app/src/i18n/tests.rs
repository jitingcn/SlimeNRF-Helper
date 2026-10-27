use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

type Messages = BTreeMap<String, String>;
type Catalogs = BTreeMap<String, Messages>;

fn read_resources(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut resources = Vec::new();
    for file in fs::read_dir(root).map_err(|error| error.to_string())? {
        let file = file.map_err(|error| error.to_string())?;
        if !file.path().is_file() {
            return Err(format!("Expected a locale file: {}", file.path().display()));
        }
        let name = file.file_name().to_string_lossy().into_owned();
        let content =
            fs::read_to_string(file.path()).map_err(|error| format!("{name}: {error}"))?;
        resources.push((name, content));
    }
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(resources)
}

fn context_segment(segment: &str) -> bool {
    segment
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_lowercase)
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn placeholders(value: &str) -> Result<BTreeSet<&str>, String> {
    let mut names = BTreeSet::new();
    let mut remainder = value;
    while let Some((_, tail)) = remainder.split_once("%{") {
        let (name, rest) = tail
            .split_once('}')
            .ok_or_else(|| format!("Unclosed placeholder in {value:?}"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("Invalid placeholder in {value:?}"));
        }
        names.insert(name);
        remainder = rest;
    }
    Ok(names)
}

fn flatten_mapping(
    mapping: &serde_yaml::Mapping,
    prefix: &str,
    path: &str,
    messages: &mut Messages,
) -> Result<(), String> {
    if mapping.is_empty() {
        return Err(format!("Empty section in {path}: {prefix}"));
    }
    for (segment, value) in mapping {
        let segment = segment
            .as_str()
            .filter(|segment| context_segment(segment))
            .ok_or_else(|| format!("Invalid context segment in {path}: {segment:?}"))?;
        let key = if prefix.is_empty() {
            segment.to_owned()
        } else {
            format!("{prefix}.{segment}")
        };
        match value {
            serde_yaml::Value::Mapping(children) => {
                flatten_mapping(children, &key, path, messages)?;
            }
            serde_yaml::Value::String(value) if !prefix.is_empty() => {
                placeholders(value).map_err(|error| format!("{path}: {key}: {error}"))?;
                if messages.insert(key.clone(), value.clone()).is_some() {
                    return Err(format!("Flattened key collision in {path}: {key}"));
                }
            }
            _ => {
                return Err(format!(
                    "Expected nested string translation in {path}: {key}"
                ));
            }
        }
    }
    Ok(())
}

fn validate_resources(resources: &[(String, String)]) -> Result<Catalogs, String> {
    let mut catalogs = Catalogs::new();
    for (path, content) in resources {
        let locale = match path.as_str() {
            "en.yml" => "en",
            "zh-CN.yml" => "zh-CN",
            _ => return Err(format!("Unsupported catalog path: {path}")),
        };
        // Mapping deserialization rejects duplicate YAML keys at every nesting depth.
        let mapping: serde_yaml::Mapping =
            serde_yaml::from_str(content).map_err(|error| format!("{path}: {error}"))?;
        let mut messages = Messages::new();
        flatten_mapping(&mapping, "", path, &mut messages)?;
        if catalogs.insert(locale.to_owned(), messages).is_some() {
            return Err(format!("Duplicate locale catalog: {path}"));
        }
    }
    let english = catalogs
        .get("en")
        .ok_or_else(|| "Missing English catalog".to_owned())?;
    let chinese = catalogs
        .get("zh-CN")
        .ok_or_else(|| "Missing Chinese catalog".to_owned())?;
    if !english.keys().eq(chinese.keys()) {
        let english_keys: BTreeSet<_> = english.keys().collect();
        let chinese_keys: BTreeSet<_> = chinese.keys().collect();
        return Err(format!(
            "Key mismatch: English-only {:?}, Chinese-only {:?}",
            english_keys.difference(&chinese_keys).collect::<Vec<_>>(),
            chinese_keys.difference(&english_keys).collect::<Vec<_>>()
        ));
    }
    for (key, value) in english {
        if placeholders(value)? != placeholders(&chinese[key])? {
            return Err(format!("Placeholder mismatch: {key}"));
        }
    }
    Ok(catalogs)
}

#[test]
fn actual_catalogs_match_compiled_translations_and_english_fallback() {
    let resources = read_resources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales"))
        .expect("Read application catalogs");
    let catalogs = validate_resources(&resources).expect("Valid application catalog contract");
    let resource_locales: BTreeSet<_> = catalogs.keys().map(String::as_str).collect();
    let compiled_locales = crate::_rust_i18n_available_locales();
    assert_eq!(
        compiled_locales
            .iter()
            .map(|locale| locale.as_ref())
            .collect::<BTreeSet<_>>(),
        resource_locales
    );
    for (locale, messages) in &catalogs {
        for (key, expected) in messages {
            let key = key.as_str();
            // Check the exact locale backend too: fallback must not hide missing data.
            assert_eq!(
                crate::_rust_i18n_backend()
                    .translate(locale, key)
                    .as_deref(),
                Some(expected.as_str()),
                "Compiled catalog differs: {locale}: {key}"
            );
            assert_eq!(rust_i18n::t!(key, locale = locale), expected.as_str());
            if locale == "en" {
                assert_eq!(
                    rust_i18n::t!(key, locale = "fr-CA"),
                    expected.as_str(),
                    "English fallback differs: {key}"
                );
            }
        }
    }
    // Explicit locale arguments avoid racing tests that change the process-wide locale.
    let missing = "contract_probe.missing_translation";
    assert!(crate::_rust_i18n_try_translate("zh-CN", missing).is_none());
    assert_eq!(rust_i18n::t!(missing, locale = "zh-CN"), missing);
}

fn fixture(resources: &[(&str, &str)]) -> Result<Catalogs, String> {
    validate_resources(
        &resources
            .iter()
            .map(|(path, content)| ((*path).to_owned(), (*content).to_owned()))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn duplicate_yaml_keys_and_ambiguous_flattened_paths_are_rejected() {
    for content in [
        "sample: {label: First}\nsample: {other: Second}",
        "sample:\n  label: First\n  label: Second",
        "sample:\n  section:\n    label: First\n    label: Second",
    ] {
        assert!(
            fixture(&[("en.yml", content)])
                .unwrap_err()
                .contains("duplicate entry")
        );
    }
    // Dotted YAML keys cannot alias a path in the required nested hierarchy.
    assert!(
        fixture(&[("en.yml", "sample: {label: First}\nsample.label: Second",)])
            .unwrap_err()
            .contains("Invalid context segment")
    );
}

#[test]
fn exactly_two_root_locale_files_and_matching_keys_are_required() {
    assert!(fixture(&[]).is_err());
    assert!(
        fixture(&[("en.yml", "sample: {label: English}")])
            .unwrap_err()
            .contains("Missing Chinese catalog")
    );
    assert!(
        fixture(&[("zh-CN.yml", "sample: {label: 中文}")])
            .unwrap_err()
            .contains("Missing English catalog")
    );
    for path in ["fr.yml", "sample/en.yml", "app.yml", "en.yaml"] {
        assert!(
            fixture(&[(path, "sample: {label: Text}")])
                .unwrap_err()
                .contains("Unsupported catalog path")
        );
    }
    assert!(
        fixture(&[
            ("en.yml", "sample: {label: English}"),
            ("zh-CN.yml", "sample: {other: 中文}"),
        ])
        .unwrap_err()
        .contains("Key mismatch")
    );
    assert!(
        fixture(&[
            ("en.yml", "sample: {label: First}"),
            ("en.yml", "sample: {label: Second}"),
        ])
        .unwrap_err()
        .contains("Duplicate locale catalog")
    );
}

#[test]
fn only_nested_context_keys_with_string_leaves_are_accepted() {
    for content in [
        "English sentence: Text",
        "sample: {BadCase: Text}",
        "sample: {'': Text}",
        "sample: {label: {en: Text, zh-CN: 中文}}",
        "sample: {label: 42}",
        "sample: {label: null}",
        "sample: {label: [Text]}",
        "sample: {section: {}}",
        "sample: Text",
        "sample.label: Text",
        "_version: 2",
        "{}",
    ] {
        assert!(fixture(&[("en.yml", content)]).is_err(), "{content}");
    }
}

#[test]
fn nested_keys_flatten_and_placeholders_match_by_name() {
    assert!(
        fixture(&[
            ("en.yml", "sample: {label: 'Count %{count}'}"),
            ("zh-CN.yml", "sample: {label: '数量 %{total}'}"),
        ])
        .unwrap_err()
        .contains("Placeholder mismatch")
    );
    assert!(
        fixture(&[("en.yml", "sample: {label: 'Count %{count'}")])
            .unwrap_err()
            .contains("Unclosed placeholder")
    );
    let catalogs = fixture(&[
        (
            "en.yml",
            "sample: {section: {label: '%{first} then %{second}'}}",
        ),
        (
            "zh-CN.yml",
            "sample: {section: {label: '%{second}，然后 %{first}'}}",
        ),
    ])
    .expect("Nested catalogs with reordered placeholders");
    assert_eq!(
        catalogs["en"]
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["sample.section.label"]
    );
}
