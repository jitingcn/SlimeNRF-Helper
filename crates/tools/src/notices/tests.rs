use super::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

mod behavior;
mod failures;
mod maintained;
mod rendering;

const OUTPUT: &str = "THIRD_PARTY_NOTICES.txt";
const REVIEW_GUIDE: &str = "docs/DEVELOPMENT.md#license-notices";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        loop {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "slimenrf notice tests {}-{serial}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fixture directory {}: {error}", path.display()),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    directory: TempDir,
    document: Value,
    inputs: BTreeMap<String, String>,
    upstream: Vec<u8>,
    upstream_key: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new();
        let mut inputs = BTreeMap::new();
        let mut fingerprints = BTreeMap::new();
        for &name in MANIFESTS {
            let bytes = if name == "Cargo.toml" {
                b"[workspace.package]\nlicense = \"MIT OR Apache-2.0\"\n".to_vec()
            } else {
                let package = name.split('/').nth(1).expect("member manifest path");
                format!("[package]\nname = \"fixture-{package}\"\nlicense.workspace = true\n")
                    .into_bytes()
            };
            put(directory.path(), name, &bytes);
            inputs.insert(name.to_owned(), digest(&bytes));
            fingerprints.insert(
                name.to_owned(),
                semantic_manifest_hash(&bytes).expect("fixture manifest is valid TOML"),
            );
        }
        for (name, data) in [
            (
                "Cargo.lock",
                b"[[package]]\nname = \"example\"\nversion = \"1.2.3\"\nchecksum = \"abc123\"\n"
                    .as_slice(),
            ),
            ("LICENSE-MIT", b"Application MIT terms\n".as_slice()),
            ("LICENSE-APACHE", b"Application Apache terms\n".as_slice()),
        ] {
            put(directory.path(), name, data);
            inputs.insert(name.to_owned(), digest(data));
        }
        for (index, &name) in VENDOR_LICENSES.iter().enumerate() {
            let data = format!("Vendor grant {index}\n");
            put(directory.path(), name, data.as_bytes());
            inputs.insert(name.to_owned(), digest(data.as_bytes()));
        }
        let document = json!({
            "schema": 1,
            "input_sha256": inputs,
            "manifest_semantic_sha256": fingerprints,
            "sources": {},
            "graphs": [
                {"target": "windows", "packages": ["example@1.2.3"]},
                {"target": "linux", "packages": ["example@1.2.3"]}
            ],
            "packages": [{
                "name": "example", "version": "1.2.3", "license": "MIT AND BSD-3-Clause",
                "checksum": "abc123", "source_archive": "https://example.invalid/example-1.2.3.crate",
                "target_roles": {"windows": ["code_candidate"], "linux": ["compile_tool"]},
                "notices": []
            }],
            "materials": [{"name": "Native kernels", "scope": "Both variants", "notices": []}],
            "scope": ["Fixture scope"],
            "mpl_source": [],
            "source_instructions": [],
            "maintainer_review": ["Fixture review boundary"]
        });
        let upstream =
            b"Mixed upstream notice\r\nCopyright (c) Example Owner\r\nMIT and BSD terms\r\n"
                .to_vec();
        let mut fixture = Self {
            directory,
            document,
            inputs,
            upstream,
            upstream_key: String::new(),
        };
        let upstream = fixture.upstream.clone();
        fixture.upstream_key = fixture.source(
            &upstream,
            &upstream,
            "utf-8",
            json!({"kind": "published_crate", "url": "https://example.invalid/LICENSE"}),
        );
        fixture.document["packages"][0]["notices"] = json!([fixture.upstream_key]);
        for &name in VENDOR_LICENSES {
            let data = fs::read(fixture.path(name)).expect("read fixture vendor license");
            let key = fixture.source(
                &data,
                &data,
                "utf-8",
                json!({"kind": "repository", "path": name, "file_sha256": digest(&data)}),
            );
            fixture.material_notices().push(json!(key));
        }
        fixture.save();
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    fn put(&self, name: &str, data: &[u8]) {
        put(self.root(), name, data);
    }

    fn append(&self, name: &str, addition: &[u8]) {
        let mut bytes = fs::read(self.path(name)).expect("read fixture input");
        bytes.extend_from_slice(addition);
        self.put(name, &bytes);
    }

    fn save(&self) {
        let mut bytes = serde_json::to_vec(&self.document).expect("serialize fixture input");
        bytes.push(b'\n');
        self.put(INPUT, &bytes);
    }

    fn source(&mut self, bytes: &[u8], utf8: &[u8], encoding: &str, origin: Value) -> String {
        let key = digest(bytes);
        self.document["sources"][key.as_str()] = json!({
            "sha256": key,
            "bytes": bytes.len(),
            "encoding": encoding,
            "utf8_sha256": digest(utf8),
            "base64": base64(bytes),
            "origins": [origin]
        });
        key
    }

    fn package_notices(&mut self) -> &mut Vec<Value> {
        self.document["packages"][0]["notices"]
            .as_array_mut()
            .expect("fixture package notices")
    }

    fn material_notices(&mut self) -> &mut Vec<Value> {
        self.document["materials"][0]["notices"]
            .as_array_mut()
            .expect("fixture material notices")
    }

    fn options(&self, output: Option<PathBuf>) -> Options {
        Options {
            root: self.root().to_path_buf(),
            output,
            check: false,
            refresh_inputs: false,
            development: false,
            cargo_rerun: false,
        }
    }

    fn check(&self, output: Option<PathBuf>) -> Options {
        let mut options = self.options(output);
        options.check = true;
        options
    }

    fn load(&self) -> Loaded {
        load_inputs(self.root(), false).expect("load valid reviewed fixture")
    }
}

fn put(root: &Path, name: &str, data: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().expect("fixture path has a parent"))
        .expect("create fixture parent");
    fs::write(&path, data).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        result.push(char::from(ALPHABET[usize::from(a >> 2)]));
        result.push(char::from(ALPHABET[usize::from(((a & 3) << 4) | (b >> 4))]));
        result.push(if chunk.len() > 1 {
            char::from(ALPHABET[usize::from(((b & 15) << 2) | (c >> 6))])
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            char::from(ALPHABET[usize::from(c & 63)])
        } else {
            '='
        });
    }
    result
}

fn file_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read fixture directory") {
            let entry = entry.expect("read fixture directory entry");
            let path = entry.path();
            if entry.file_type().expect("read fixture file type").is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("fixture file is inside root")
                        .to_path_buf(),
                    fs::read(&path).expect("read fixture file"),
                );
            }
        }
    }
    files
}

fn assert_execute_error_unchanged(fixture: &Fixture, options: &Options, message: &str) {
    let before = file_snapshot(fixture.root());
    let error = execute(options, &write_atomic).expect_err("execution should reject the fixture");
    assert!(error.contains(message), "expected {message:?} in {error:?}");
    assert_eq!(
        file_snapshot(fixture.root()),
        before,
        "failed execution changed fixture files"
    );
}

fn assert_load_error_unchanged(fixture: &Fixture, refresh: bool, message: &str) {
    let before = file_snapshot(fixture.root());
    let error = load_inputs(fixture.root(), refresh)
        .err()
        .expect("loading should reject the fixture");
    assert!(error.contains(message), "expected {message:?} in {error:?}");
    assert_eq!(
        file_snapshot(fixture.root()),
        before,
        "failed load changed fixture files"
    );
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|part| part == needle)
}

fn replace_first(bytes: &[u8], old: &str, new: &str) -> Vec<u8> {
    let start = bytes
        .windows(old.len())
        .position(|part| part == old.as_bytes())
        .expect("expected bytes to replace");
    let mut result = Vec::with_capacity(bytes.len() - old.len() + new.len());
    result.extend_from_slice(&bytes[..start]);
    result.extend_from_slice(new.as_bytes());
    result.extend_from_slice(&bytes[start + old.len()..]);
    result
}
