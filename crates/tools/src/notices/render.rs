use super::{
    digest,
    model::{Loaded, decode_source},
};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn invalid_field(field: &str) -> String {
    format!("Missing or invalid notice field: {field}")
}

fn string_value<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.as_str().ok_or_else(|| invalid_field(field))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_field(field))
}

fn optional_string_field<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(entry) => string_value(entry, field).map(Some),
    }
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid_field(field))
}

fn object_field<'a>(value: &'a Value, field: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_field(field))
}

fn text(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(value.as_bytes());
    output.push(b'\n');
}

// Formatting into a byte vector preserves the reviewed license bytes verbatim.
macro_rules! line {
    ($output:expr, $($args:tt)*) => {
        writeln!($output, $($args)*)
            .map_err(|error| format!("Cannot render notices: {error}"))?
    };
}

#[derive(Clone, Copy)]
enum Reference<'a> {
    Package { name: &'a str, version: &'a str },
    Material(&'a str),
}

impl Reference<'_> {
    fn append_to(self, output: &mut Vec<u8>) {
        match self {
            Self::Package { name, version } => {
                output.extend_from_slice(name.as_bytes());
                output.push(b'@');
                output.extend_from_slice(version.as_bytes());
            }
            Self::Material(name) => output.extend_from_slice(name.as_bytes()),
        }
    }
}

fn notice_references<'a>(
    output: &mut Vec<u8>,
    notices: &'a [Value],
    references: &mut BTreeMap<&'a str, Vec<Reference<'a>>>,
    reference: Reference<'a>,
) -> Result<(), String> {
    output.extend_from_slice(b"Notice source SHA-256: ");
    for (index, notice) in notices.iter().enumerate() {
        let key = string_value(notice, "notices")?;
        let users = references
            .get_mut(key)
            .ok_or_else(|| format!("Missing notice source: {key}"))?;
        if index != 0 {
            output.extend_from_slice(b", ");
        }
        output.extend_from_slice(key.as_bytes());
        users.push(reference);
    }
    output.extend_from_slice(b"\n\n");
    Ok(())
}

fn roles(output: &mut Vec<u8>, item: &Value) -> Result<(), String> {
    let roles = object_field(item, "target_roles")?;
    let mut targets: Vec<_> = roles.iter().collect();
    targets.sort_unstable_by(|left, right| left.0.cmp(right.0));
    output.extend_from_slice(b"Captured roles: ");
    for (index, (target, roles)) in targets.into_iter().enumerate() {
        let roles = roles
            .as_array()
            .ok_or_else(|| invalid_field("target_roles"))?;
        if index != 0 {
            output.extend_from_slice(b"; ");
        }
        output.extend_from_slice(target.as_bytes());
        output.extend_from_slice(b": ");
        for (role_index, role) in roles.iter().enumerate() {
            if role_index != 0 {
                output.extend_from_slice(b", ");
            }
            output.extend_from_slice(string_value(role, "target_roles")?.as_bytes());
        }
    }
    output.push(b'\n');
    Ok(())
}

fn origin(output: &mut Vec<u8>, origin: &Value) -> Result<(), String> {
    let location = match origin.get("url") {
        Some(url) => string_value(url, "origin.url")?,
        None => string_field(origin, "path")?,
    };
    output.extend_from_slice(b"Source: ");
    output.extend_from_slice(location.as_bytes());
    match origin.get("lines") {
        None | Some(Value::Null) => {}
        Some(Value::Array(lines)) if lines.is_empty() => {}
        Some(Value::Array(lines)) => {
            let [first, last] = lines.as_slice() else {
                return Err(invalid_field("origin.lines"));
            };
            let first = first
                .as_u64()
                .ok_or_else(|| invalid_field("origin.lines"))?;
            let last = last.as_u64().ok_or_else(|| invalid_field("origin.lines"))?;
            if first == 0 || last < first {
                return Err(invalid_field("origin.lines"));
            }
            write!(output, " lines {first}-{last}")
                .map_err(|error| format!("Cannot render notices: {error}"))?;
        }
        Some(_) => return Err(invalid_field("origin.lines")),
    }
    output.push(b'\n');
    Ok(())
}

pub(super) fn render(root: &Path, loaded: &Loaded) -> Result<Vec<u8>, String> {
    let manifest = &loaded.manifest;
    let mut output = Vec::with_capacity(loaded.candidate.len());

    text(
        &mut output,
        "SlimeNRF Helper licenses and third-party notices",
    );
    text(
        &mut output,
        "Generated by slimenrf-tools notices. Do not edit this document.",
    );
    line!(
        output,
        "Reviewed input SHA-256: {}",
        digest(&loaded.candidate)
    );
    let hashes = manifest
        .get("input_sha256")
        .ok_or_else(|| invalid_field("input_sha256"))?;
    line!(
        output,
        "Cargo.lock SHA-256: {}",
        string_field(hashes, "Cargo.lock")?
    );
    output.push(b'\n');

    for paragraph in array_field(manifest, "scope")? {
        text(&mut output, string_value(paragraph, "scope")?);
        output.push(b'\n');
    }

    text(&mut output, "Application license alternatives");
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        line!(output, "===== {name} =====");
        let bytes =
            fs::read(root.join(name)).map_err(|error| format!("Cannot read {name}: {error}"))?;
        output.extend_from_slice(&bytes);
        output.push(b'\n');
    }

    text(&mut output, "MPL-2.0 covered source");
    for item in array_field(manifest, "mpl_source")? {
        line!(
            output,
            "{} {}",
            string_field(item, "name")?,
            string_field(item, "version")?
        );
        line!(
            output,
            "Unmodified covered source archive: {}",
            string_field(item, "url")?
        );
        line!(output, "Archive SHA-256: {}", string_field(item, "sha256")?);
    }
    for paragraph in array_field(manifest, "source_instructions")? {
        text(&mut output, string_value(paragraph, "source_instructions")?);
    }
    output.push(b'\n');

    text(&mut output, "Package index");
    let mut references: BTreeMap<&str, Vec<Reference<'_>>> = loaded
        .sources
        .keys()
        .map(|key| (key.as_str(), Vec::new()))
        .collect();
    for item in array_field(manifest, "packages")? {
        let name = string_field(item, "name")?;
        let version = string_field(item, "version")?;
        line!(output, "===== {name}@{version} =====");
        line!(
            output,
            "Original license declaration: {}",
            string_field(item, "license")?
        );
        line!(
            output,
            "Source archive: {}",
            string_field(item, "source_archive")?
        );
        line!(
            output,
            "Archive SHA-256: {}",
            string_field(item, "checksum")?
        );
        if let Some(repository) =
            optional_string_field(item, "repository")?.filter(|value| !value.is_empty())
        {
            line!(output, "Upstream repository: {repository}");
        }
        roles(&mut output, item)?;
        if let Some(policy) =
            optional_string_field(item, "policy")?.filter(|value| !value.is_empty())
        {
            line!(output, "Recorded handling: {policy}");
        }
        notice_references(
            &mut output,
            array_field(item, "notices")?,
            &mut references,
            Reference::Package { name, version },
        )?;
    }

    text(
        &mut output,
        "Additional code, asset, and native kernel materials",
    );
    for item in array_field(manifest, "materials")? {
        let name = string_field(item, "name")?;
        text(&mut output, name);
        text(&mut output, string_field(item, "scope")?);
        notice_references(
            &mut output,
            array_field(item, "notices")?,
            &mut references,
            Reference::Material(name),
        )?;
    }

    text(&mut output, "Complete notice texts");
    text(
        &mut output,
        "Identical source texts are reproduced once and referenced by SHA-256 above.",
    );
    text(
        &mut output,
        "Alternative license texts are retained as evidence, not relicensing of other material.",
    );
    let source_metadata = object_field(manifest, "sources")?;
    for (key, data) in &loaded.sources {
        let source = source_metadata
            .get(key)
            .ok_or_else(|| format!("Missing notice source metadata: {key}"))?;
        let encoding = string_field(source, "encoding")?;
        let rendered = decode_source(data, encoding, key)?;
        let bytes = source
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_field("bytes"))?;
        output.push(b'\n');
        line!(output, "===== Notice {key} =====");
        output.extend_from_slice(b"Used by: ");
        let users = references
            .get(key.as_str())
            .ok_or_else(|| format!("Missing notice source references: {key}"))?;
        for (index, user) in users.iter().copied().enumerate() {
            if index != 0 {
                output.extend_from_slice(b", ");
            }
            user.append_to(&mut output);
        }
        output.push(b'\n');
        for source_origin in array_field(source, "origins")? {
            origin(&mut output, source_origin)?;
        }
        if encoding != "utf-8" {
            line!(
                output,
                "Original encoding: {encoding}. Transcoded to UTF-8 without changing the text."
            );
            line!(
                output,
                "UTF-8 SHA-256: {}",
                string_field(source, "utf8_sha256")?
            );
        }
        line!(
            output,
            "----- Begin complete text ({bytes} original bytes) -----"
        );
        output.extend_from_slice(rendered.as_bytes());
        output.push(b'\n');
        text(&mut output, "----- End complete text -----");
    }

    output.push(b'\n');
    text(&mut output, "Maintainer review boundaries");
    for paragraph in array_field(manifest, "maintainer_review")? {
        text(&mut output, string_value(paragraph, "maintainer_review")?);
    }
    Ok(output)
}
