use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    ops::Range,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};

use super::{GENERATOR_INPUTS, INPUT, MANIFESTS, REVIEW_GUIDE, VENDOR_LICENSES, digest};

pub(super) struct Loaded {
    pub(super) manifest: Value,
    pub(super) sources: BTreeMap<String, Vec<u8>>,
    pub(super) original: Vec<u8>,
    pub(super) candidate: Vec<u8>,
}

// serde_json's normal Value deserializer overwrites duplicate keys. The reviewed
// inventory must instead reject duplicates in every object, including nested ones.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueVisitor;

        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(Value::Array(values))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = object.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!(
                            "Duplicate key in reviewed inputs: {key}"
                        )));
                    }
                    let UniqueValue(value) = object.next_value()?;
                    values.insert(key, value);
                }
                Ok(Value::Object(values))
            }
        }

        deserializer.deserialize_any(UniqueVisitor).map(UniqueValue)
    }
}

fn reviewed_json(original: &[u8]) -> Result<Value, String> {
    let data = original.strip_prefix(b"\xef\xbb\xbf").unwrap_or(original);
    let mut deserializer = serde_json::Deserializer::from_slice(data);
    let UniqueValue(value) = UniqueValue::deserialize(&mut deserializer)
        .map_err(|error| format!("Cannot read {INPUT}: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("Cannot read {INPUT}: {error}"))?;
    Ok(value)
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .get(name)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("Missing or invalid reviewed input object: {name}"))
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("Missing or invalid reviewed input array: {name}"))
}

fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing or invalid reviewed input field: {name}"))
}

pub(super) fn relative_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(name);
    if candidate.has_root()
        || name.split('/').any(|part| part == "..")
        || name.contains(['\\', ':', '\r', '\n', '\0'])
    {
        return Err(format!("Nonportable input path: {name}"));
    }
    Ok(root.join(candidate))
}

fn stale_input(name: &str) -> String {
    if MANIFESTS.contains(&name) {
        format!(
            "Stale input hash: {name}. If only the profile or formatting changed, run \
             cargo run -p slimenrf-tools -- notices --check --refresh-inputs. \
             Dependency, package, or other changes require review: {REVIEW_GUIDE}"
        )
    } else {
        format!("Stale input hash: {name}. Review the changed input: {REVIEW_GUIDE}")
    }
}

fn read_input(root: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = relative_file(root, name)?;
    fs::read(path).map_err(|error| format!("Missing input {name}: {error}"))
}

fn read_checked(root: &Path, name: &str, expected: &str) -> Result<Vec<u8>, String> {
    let data = read_input(root, name)?;
    if digest(&data) != expected {
        return Err(stale_input(name));
    }
    Ok(data)
}

pub(super) fn decode_source<'a>(
    data: &'a [u8],
    encoding: &str,
    key: &str,
) -> Result<Cow<'a, str>, String> {
    match encoding {
        "utf-8" => std::str::from_utf8(data)
            .map(Cow::Borrowed)
            .map_err(|_| format!("Invalid utf-8 notice source: {key}")),
        "windows-1252" => {
            // WHATWG maps these bytes to controls, whereas Python's strict
            // windows-1252 decoder treats them as undefined.
            if data
                .iter()
                .any(|&byte| matches!(byte, 0x81 | 0x8d | 0x8f | 0x90 | 0x9d))
            {
                return Err(format!("Invalid windows-1252 notice source: {key}"));
            }
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling_and_without_replacement(data)
                .ok_or_else(|| format!("Invalid windows-1252 notice source: {key}"))
        }
        _ => Err(format!("Unreviewed source encoding: {encoding}")),
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn load_inputs(root: &Path, refresh_inputs: bool) -> Result<Loaded, String> {
    let original = fs::read(relative_file(root, INPUT)?)
        .map_err(|error| format!("Cannot read {INPUT}: {error}"))?;
    let mut manifest = reviewed_json(&original)?;
    if manifest.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err("Unsupported notices input schema".to_owned());
    }

    let input_hashes = object(&manifest, "input_sha256")?;
    for name in MANIFESTS
        .iter()
        .copied()
        .chain(["Cargo.lock", "LICENSE-MIT", "LICENSE-APACHE"])
        .chain(VENDOR_LICENSES.iter().copied())
    {
        if !input_hashes.contains_key(name) {
            return Err("Missing mandatory application or vendor license input".to_owned());
        }
    }

    let source_metadata = object(&manifest, "sources")?;
    let mut sources = BTreeMap::new();
    for (key, metadata) in source_metadata {
        let encoded = string(metadata, "base64")?;
        let data = STANDARD
            .decode(encoded)
            .map_err(|error| format!("Invalid base64 notice source: {key}: {error}"))?;
        if digest(&data) != *key
            || metadata.get("sha256").and_then(Value::as_str) != Some(key.as_str())
            || metadata.get("bytes").and_then(Value::as_u64) != Some(data.len() as u64)
        {
            return Err(format!("Notice source hash or length mismatch: {key}"));
        }
        let encoding = string(metadata, "encoding")?;
        let rendered = decode_source(&data, encoding, key)?;
        if metadata.get("utf8_sha256").and_then(Value::as_str)
            != Some(digest(rendered.as_bytes()).as_str())
        {
            return Err(format!("Rendered notice hash mismatch: {key}"));
        }
        sources.insert(key.clone(), data);
    }

    // Validate paths before hashes, even for otherwise malformed metadata.
    for name in input_hashes.keys() {
        relative_file(root, name)?;
    }
    for (name, expected) in input_hashes {
        let expected = expected
            .as_str()
            .filter(|hash| valid_sha256(hash))
            .ok_or_else(|| {
                format!("Invalid input hash: {name}; review the input: {REVIEW_GUIDE}")
            })?;
        if !MANIFESTS.contains(&name.as_str()) {
            read_checked(root, name, expected)?;
        }
    }

    let fingerprints = manifest
        .get("manifest_semantic_sha256")
        .and_then(Value::as_object);
    if fingerprints.is_none_or(|fingerprints| {
        MANIFESTS.iter().any(|name| {
            fingerprints
                .get(*name)
                .and_then(Value::as_str)
                .is_none_or(|hash| !valid_sha256(hash))
        })
    }) {
        return Err(format!(
            "Missing or invalid reviewed manifest semantic fingerprints; review and update metadata: {REVIEW_GUIDE}"
        ));
    }
    let fingerprints = object(&manifest, "manifest_semantic_sha256")?;
    let mut changes = BTreeMap::new();
    for &name in MANIFESTS {
        let expected = input_hashes
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Invalid input hash: {name}"))?;
        let data = read_input(root, name)?;
        let actual = digest(&data);
        if actual != expected && !refresh_inputs {
            return Err(stale_input(name));
        }
        let semantic = semantic_manifest_hash(&data).map_err(|error| {
            format!("Cannot fingerprint manifest {name}; review it: {REVIEW_GUIDE}: {error}")
        })?;
        if fingerprints.get(name).and_then(Value::as_str) != Some(semantic.as_str()) {
            return Err(format!(
                "Manifest semantics changed: {name}; dependency, package, or other changes require review: {REVIEW_GUIDE}"
            ));
        }
        if actual != expected {
            changes.insert(name.to_owned(), actual);
        }
    }

    validate_coverage(root, &manifest, &sources)?;
    let candidate = if changes.is_empty() {
        original.clone()
    } else {
        replace_manifest_hashes(&original, &changes)?
    };
    if let Some(input_hashes) = manifest
        .get_mut("input_sha256")
        .and_then(Value::as_object_mut)
    {
        for (name, hash) in changes {
            input_hashes.insert(name, Value::String(hash));
        }
    }
    Ok(Loaded {
        manifest,
        sources,
        original,
        candidate,
    })
}

pub(super) fn semantic_manifest_hash(data: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(data).map_err(|error| error.to_string())?;
    let mut manifest: toml::Value = toml::from_str(text).map_err(|error| format!("{error}"))?;
    let table = manifest
        .as_table_mut()
        .ok_or_else(|| "TOML manifest must be an object".to_owned())?;
    table.remove("profile");
    let mut canonical = String::new();
    canonical_toml(&manifest, &mut canonical)?;
    Ok(digest(canonical.as_bytes()))
}

fn push_hex_escape(output: &mut String, code: u16) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        output.push(char::from(HEX[usize::from((code >> shift) & 0x0f)]));
    }
}

fn canonical_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{0c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\u{20}'..='\u{7e}' => output.push(character),
            _ => {
                let code = u32::from(character);
                if code <= 0xffff {
                    push_hex_escape(output, code as u16);
                } else {
                    let code = code - 0x10000;
                    push_hex_escape(output, 0xd800 | (code >> 10) as u16);
                    push_hex_escape(output, 0xdc00 | (code & 0x3ff) as u16);
                }
            }
        }
    }
    output.push('"');
}

// Match Python's shortest-float JSON spelling, including its choice of fixed
// notation and the sign and minimum width of scientific exponents.
fn canonical_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }

    let spelling = value.abs().to_string();
    let (mantissa, extra_exponent) = spelling
        .split_once('e')
        .or_else(|| spelling.split_once('E'))
        .map_or((spelling.as_str(), 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap_or_default())
        });
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let Some(first) = digits.bytes().position(|byte| byte != b'0') else {
        return "0.0".to_owned();
    };
    let last = digits.trim_end_matches('0').len();
    let digits = &digits[first..last];
    let exponent = decimal - first as i32 - 1 + extra_exponent;
    let mut output = String::new();
    if value.is_sign_negative() {
        output.push('-');
    }
    if !(-4..16).contains(&exponent) {
        output.push(char::from(digits.as_bytes()[0]));
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push_str(&format!("e{exponent:+03}"));
    } else if exponent >= 0 {
        let point = exponent as usize + 1;
        if point >= digits.len() {
            output.push_str(digits);
            for _ in digits.len()..point {
                output.push('0');
            }
            output.push_str(".0");
        } else {
            output.push_str(&digits[..point]);
            output.push('.');
            output.push_str(&digits[point..]);
        }
    } else {
        output.push_str("0.");
        for _ in 0..(-exponent - 1) {
            output.push('0');
        }
        output.push_str(digits);
    }
    output
}

fn canonical_toml(value: &toml::Value, output: &mut String) -> Result<(), String> {
    match value {
        toml::Value::String(value) => canonical_string(value, output),
        toml::Value::Integer(value) => output.push_str(&value.to_string()),
        toml::Value::Float(value) => output.push_str(&canonical_float(*value)),
        toml::Value::Boolean(value) => output.push_str(if *value { "true" } else { "false" }),
        toml::Value::Datetime(_) => {
            return Err("TOML datetime cannot be represented in canonical JSON".to_owned());
        }
        toml::Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical_toml(value, output)?;
            }
            output.push(']');
        }
        toml::Value::Table(values) => {
            // Sort explicitly even when another dependency enables TOML's
            // preserve_order feature through Cargo feature unification.
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical_string(key, output);
                output.push(':');
                canonical_toml(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn skip_whitespace(data: &[u8], index: &mut usize) {
    while matches!(data.get(*index), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        *index += 1;
    }
}

fn json_token<'a, T: Deserialize<'a>>(data: &'a [u8], start: usize) -> Result<(T, usize), String> {
    let bytes = data
        .get(start..)
        .ok_or_else(|| "Invalid object in reviewed inputs".to_owned())?;
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<T>();
    let value = stream
        .next()
        .ok_or_else(|| "Invalid object in reviewed inputs".to_owned())?
        .map_err(|error| format!("Invalid object in reviewed inputs: {error}"))?;
    Ok((value, start + stream.byte_offset()))
}

fn object_spans(data: &[u8], start: usize) -> Result<Vec<(String, Range<usize>)>, String> {
    let mut index = start;
    skip_whitespace(data, &mut index);
    if data.get(index) != Some(&b'{') {
        return Err("Expected an object in reviewed inputs".to_owned());
    }
    index += 1;
    let mut members = Vec::new();
    loop {
        skip_whitespace(data, &mut index);
        if data.get(index) == Some(&b'}') {
            return Ok(members);
        }
        let (key, after_key) = json_token::<String>(data, index)?;
        index = after_key;
        skip_whitespace(data, &mut index);
        if data.get(index) != Some(&b':') {
            return Err("Invalid object in reviewed inputs".to_owned());
        }
        index += 1;
        skip_whitespace(data, &mut index);
        let value_start = index;
        let (_, value_end) = json_token::<IgnoredAny>(data, index)?;
        members.push((key, value_start..value_end));
        index = value_end;
        skip_whitespace(data, &mut index);
        match data.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(members),
            _ => return Err("Invalid object in reviewed inputs".to_owned()),
        }
    }
}

fn replace_manifest_hashes(
    original: &[u8],
    changes: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    let start = if original.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let input_start = object_spans(original, start)?
        .into_iter()
        .find_map(|(name, span)| (name == "input_sha256").then_some(span.start))
        .ok_or_else(|| "Missing manifest hash in reviewed inputs".to_owned())?;
    let mut replacements = Vec::new();
    for (name, span) in object_spans(original, input_start)? {
        if let Some(hash) = changes.get(&name) {
            replacements.push((span, hash));
        }
    }
    if replacements.len() != changes.len() {
        return Err("Missing manifest hash in reviewed inputs".to_owned());
    }
    let mut candidate = Vec::with_capacity(original.len());
    let mut previous = 0;
    for (span, hash) in replacements {
        candidate.extend_from_slice(&original[previous..span.start]);
        candidate.push(b'"');
        candidate.extend_from_slice(hash.as_bytes());
        candidate.push(b'"');
        previous = span.end;
    }
    candidate.extend_from_slice(&original[previous..]);
    Ok(candidate)
}

fn repository_excerpt<'a>(
    data: &'a [u8],
    lines: Option<&Value>,
    name: &str,
) -> Result<&'a [u8], String> {
    let lines = match lines {
        None | Some(Value::Null) => return Ok(data),
        Some(Value::Array(lines)) if lines.is_empty() => return Ok(data),
        Some(Value::Array(lines)) if lines.len() == 2 => lines,
        _ => return Err(format!("Invalid repository notice lines: {name}")),
    };
    let first = lines[0]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("Invalid repository notice lines: {name}"))?;
    let last = lines[1]
        .as_u64()
        .filter(|value| *value >= first)
        .ok_or_else(|| format!("Invalid repository notice lines: {name}"))?;
    let mut position = 0;
    let mut number = 1u64;
    let mut start = None;
    while position < data.len() {
        if number == first {
            start = Some(position);
        }
        let end = match data[position..]
            .iter()
            .position(|&byte| matches!(byte, b'\r' | b'\n'))
        {
            Some(offset) => {
                let newline = position + offset;
                if data[newline] == b'\r' && data.get(newline + 1) == Some(&b'\n') {
                    newline + 2
                } else {
                    newline + 1
                }
            }
            None => data.len(),
        };
        if number == last {
            return start
                .and_then(|start| data.get(start..end))
                .ok_or_else(|| format!("Invalid repository notice lines: {name}"));
        }
        position = end;
        number += 1;
    }
    Err(format!("Invalid repository notice lines: {name}"))
}

fn validate_coverage(
    root: &Path,
    manifest: &Value,
    sources: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let packages = array(manifest, "packages")?;
    let identities: Vec<String> = packages
        .iter()
        .map(|item| {
            Ok(format!(
                "{}@{}",
                string(item, "name")?,
                string(item, "version")?
            ))
        })
        .collect::<Result<_, String>>()?;
    let package_ids: BTreeSet<&str> = identities.iter().map(String::as_str).collect();
    if package_ids.len() != identities.len() {
        return Err("Duplicate package entry".to_owned());
    }
    let mut graph_ids = BTreeSet::new();
    for graph in array(manifest, "graphs")? {
        let mut graph_packages = BTreeSet::new();
        for identity in array(graph, "packages")? {
            let identity = identity
                .as_str()
                .ok_or_else(|| "Invalid package ID in target graph".to_owned())?;
            if !graph_packages.insert(identity) {
                return Err(format!("Duplicate package ID in target graph: {identity}"));
            }
            graph_ids.insert(identity);
        }
    }
    if package_ids != graph_ids {
        let missing: Vec<_> = graph_ids.difference(&package_ids).copied().collect();
        let extra: Vec<_> = package_ids.difference(&graph_ids).copied().collect();
        return Err(format!(
            "Package coverage differs from frozen target graphs. Missing {missing:?}, extra {extra:?}"
        ));
    }

    let lock_data = read_input(root, "Cargo.lock")?;
    let lock_text = std::str::from_utf8(&lock_data)
        .map_err(|error| format!("Cannot read Cargo.lock: {error}"))?;
    let lock: toml::Value =
        toml::from_str(lock_text).map_err(|error| format!("Cannot parse Cargo.lock: {error}"))?;
    let locked = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "Missing package entries in Cargo.lock".to_owned())?;
    let mut checksums = BTreeMap::new();
    for item in locked {
        let name = item
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| "Invalid package name in Cargo.lock".to_owned())?;
        let version = item
            .get("version")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| "Invalid package version in Cargo.lock".to_owned())?;
        let checksum = match item.get("checksum") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| format!("Invalid locked checksum: {name}@{version}"))?,
            ),
        };
        if checksums.insert((name, version), checksum).is_some() {
            return Err(format!(
                "Duplicate package ID in Cargo.lock: {name}@{version}"
            ));
        }
    }
    let mut used = BTreeSet::new();
    for (item, identity) in packages.iter().zip(&identities) {
        let name = string(item, "name")?;
        let version = string(item, "version")?;
        let notices = array(item, "notices")?;
        if string(item, "license")?.is_empty() || notices.is_empty() {
            return Err(format!("Unhandled package license or notice: {identity}"));
        }
        let checksum = string(item, "checksum")?;
        if checksums.get(&(name, version)).copied().flatten() != Some(checksum) {
            return Err(format!("Locked archive checksum differs: {identity}"));
        }
        for key in notices {
            let key = key
                .as_str()
                .ok_or_else(|| format!("Invalid package notice source: {identity}"))?;
            if !sources.contains_key(key) {
                return Err(format!("Missing package notice source: {identity} {key}"));
            }
            used.insert(key);
        }
    }
    for item in array(manifest, "materials")? {
        let name = string(item, "name")?;
        let notices = array(item, "notices")?;
        if notices.is_empty() {
            return Err(format!("Missing material notice: {name}"));
        }
        for key in notices {
            let key = key
                .as_str()
                .ok_or_else(|| format!("Invalid material notice source: {name}"))?;
            if !sources.contains_key(key) {
                return Err(format!("Missing material notice source: {name} {key}"));
            }
            used.insert(key);
        }
    }
    if used.len() != sources.len() {
        return Err("Unreferenced reviewed notice source".to_owned());
    }

    let metadata = object(manifest, "sources")?;
    let mut origins = BTreeSet::new();
    for source in metadata.values() {
        for origin in array(source, "origins")? {
            if string(origin, "kind")? == "repository" {
                let name = string(origin, "path")?;
                relative_file(root, name)?;
                origins.insert(name);
            }
        }
    }
    if VENDOR_LICENSES.iter().any(|name| !origins.contains(name)) {
        return Err("Mandatory native kernel license omitted from delivered material".to_owned());
    }
    for (key, source) in metadata {
        for origin in array(source, "origins")? {
            if string(origin, "kind")? == "repository" {
                let name = string(origin, "path")?;
                let expected = string(origin, "file_sha256")?;
                let data = read_checked(root, name, expected)?;
                let excerpt = repository_excerpt(&data, origin.get("lines"), name)?;
                if digest(excerpt) != *key {
                    return Err(format!("Repository notice source differs: {name}"));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn cargo_rerun_paths(root: &Path, manifest: &Value) -> Result<Vec<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for name in GENERATOR_INPUTS.iter().copied().chain([INPUT]) {
        paths.insert(relative_file(root, name)?);
    }
    for name in object(manifest, "input_sha256")?.keys() {
        paths.insert(relative_file(root, name)?);
    }
    for source in object(manifest, "sources")?.values() {
        for origin in array(source, "origins")? {
            if string(origin, "kind")? == "repository" {
                paths.insert(relative_file(root, string(origin, "path")?)?);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub(super) fn fallback_rerun_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for name in GENERATOR_INPUTS
        .iter()
        .copied()
        .chain([INPUT])
        .chain(MANIFESTS.iter().copied())
        .chain(["Cargo.lock", "LICENSE-MIT", "LICENSE-APACHE"])
        .chain(VENDOR_LICENSES.iter().copied())
        .chain(["crates/recording/vendor/vqf"])
    {
        paths.insert(root.join(name));
    }
    if let Ok(original) = fs::read(root.join(INPUT))
        && let Ok(manifest) = reviewed_json(&original)
        && let Ok(extra) = cargo_rerun_paths(root, &manifest)
    {
        paths.extend(extra);
    }
    paths.into_iter().collect()
}
