use super::catalog::{compatible_release, latest_with, list_artifacts_with, list_versions_with};
use super::download::{
    MAX_MEMBER, bounded_archive, download_with, extract_archive, hash, parse_digest,
};
use super::http::{Fetch, checked_url, read_bounded, safe_error_chain, status_error};
use super::{Artifact, Progress, SourceChannel, Version, variant};
use crate::features::firmware::remote_config::{GithubRepository, RemoteConfig};
use serde_json::json;
use std::{
    collections::VecDeque,
    io::{Cursor, Write},
    sync::Mutex,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

type FixtureReply = (String, Result<Vec<u8>, String>);

struct Fixture {
    replies: Mutex<VecDeque<FixtureReply>>,
    token: bool,
}

impl Fixture {
    fn new(replies: Vec<(String, serde_json::Value)>) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(url, value)| (url, Ok(serde_json::to_vec(&value).unwrap())))
                    .collect(),
            ),
            token: false,
        }
    }
}

impl Fetch for Fixture {
    fn has_token(&self) -> bool {
        self.token
    }
    async fn fetch(
        &self,
        url: &str,
        _: usize,
        cancel: &AtomicBool,
        _: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<Vec<u8>, String> {
        super::cancelled(cancel)?;
        let (expected, reply) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        assert_eq!(url, expected);
        reply
    }
}

fn version(channel: SourceChannel) -> Version {
    Version {
        channel,
        id: 42,
        name: "260910".into(),
        published_at: "2026-09-09T00:00:00Z".into(),
        commit: None,
        url: "https://github.com/jitingcn/SlimeVR-Tracker-nRF/releases/tag/260910".into(),
    }
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn artifact(channel: SourceChannel, bytes: &[u8]) -> Artifact {
    Artifact { name: "SlimeNRF_Receiver_DataCollect_CDC.hex".into(), variant: "DataCollect CDC".into(),
        size: bytes.len() as u64, version: version(channel),
        url: "https://nightly.link/jitingcn/SlimeVR-Tracker-nRF-Receiver/actions/runs/42/firmware.zip".into(),
        sha256: Some(hash(bytes)), id: 7 }
}

#[test]
fn artifact_categories_agree_with_local_names_without_exposing_mcuboot_variant() {
    run(async {
        let cases = [
            ("SlimeNRF_Holyiot_Receiver.uf2", "Standard"),
            ("SlimeNRF_Holyiot_Receiver_update.bin", "Standard"),
            ("SlimeNRF_Holyiot_Receiver_mcuboot.bin", "Standard"),
            ("update-SlimeNRF_Holyiot_Receiver.uf2", "Bootloader"),
            ("update-SlimeNRF_Holyiot_Receiver.hex", "Bootloader"),
            ("update-SlimeNRF_Holyiot_Receiver.uf2.zip", "Bootloader"),
            ("SlimeNRF_Holyiot_Receiver_bootloader.bin", "Bootloader"),
            ("SlimeNRF_Holyiot_Receiver_DataCollect.hex", "DataCollect"),
            (
                "SlimeNRF_Holyiot_Receiver_DataCollect_CDC.hex",
                "DataCollect CDC",
            ),
            (
                "SlimeNRF_Holyiot_Receiver_DataCollect-HID.hex",
                "DataCollect HID",
            ),
        ];
        let v = version(SourceChannel::Release);
        let assets: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(id, (name, _))| {
                json!({"id":id,"name":name,"size":4,
        "browser_download_url":format!("https://github.com/{name}")})
            })
            .collect();
        let source = Fixture::new(vec![(
            format!(
                "{}/releases/42/assets?per_page=100&page=1",
                v.channel.api(&RemoteConfig::default())
            ),
            json!(assets),
        )]);
        let artifacts = list_artifacts_with(
            &RemoteConfig::default(),
            &source,
            &v,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(artifacts.len(), cases.len());
        for (name, category) in cases {
            let artifact = artifacts
                .iter()
                .find(|artifact| artifact.name == name)
                .unwrap();
            assert_eq!(artifact.variant, category, "{name}");
            assert_eq!(
                variant(name),
                crate::features::firmware::board_catalog::filename_variant(name)
            );
            let hints =
                crate::features::firmware::board_catalog::Catalog::default().match_filename(name);
            if category == "Bootloader" {
                assert!(hints.is_empty(), "{name}");
            } else {
                assert_eq!(hints.len(), 1, "{name}");
                assert_eq!(hints[0].variant, artifact.variant, "{name}");
            }
            assert_eq!(
                crate::features::firmware::board_catalog::filename_variant(&format!(
                    "C:\\downloads\\{}",
                    name.to_uppercase()
                )),
                category,
                "{name}",
            );
        }
        assert_eq!(variant("Receiver DataCollect-CDC #1"), "DataCollect CDC");
        assert_eq!(variant("Receiver DataCollect_HID #1"), "DataCollect HID");
    });
}

#[test]
fn release_versions_preserve_receivers_and_variants_not_executables() {
    run(async {
        let source = Fixture::new(vec![(
            format!(
                "{}/releases?per_page=100&page=1",
                SourceChannel::Release.api(&RemoteConfig::default())
            ),
            json!([
                {"id":1,"tag_name":"260910","published_at":"2026-09-09T00:00:00Z","html_url":"https://github.com/releases/1",
                 "assets":[{"id":7,"name":"SlimeNRF_Holyiot_Receiver_DataCollect_CDC.hex","size":4,"browser_download_url":"https://github.com/image.hex"}]},
                {"id":2,"tag_name":"260526","published_at":null,"html_url":"https://github.com/releases/2","assets":[]},
                {"id":3,"tag_name":"260911","published_at":null,"html_url":"https://github.com/releases/3",
                 "assets":[{"id":8,"name":"SmolFlasher.exe","size":4,"browser_download_url":"https://github.com/tool.exe"}]}
            ]),
        )]);
        let versions = list_versions_with(
            &RemoteConfig::default(),
            &source,
            SourceChannel::Release,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, 1);
        assert_eq!(
            variant("SlimeNRF_Holyiot_Receiver_DataCollect_CDC.hex"),
            "DataCollect CDC"
        );
        assert!(!compatible_release("nightly"));
    });
}

#[test]
fn artifact_pagination_keeps_all_variants_and_digests() {
    run(async {
        let v = version(SourceChannel::ReceiverCi);
        let first = (0..100)
    .map(
        |id| json!({"id":id,"name":format!("receiver_{id}"),"size_in_bytes":12,"expired":true}),
    )
    .collect::<Vec<_>>();
        let prefix = format!(
            "{}/actions/runs/42/artifacts?per_page=100&page=",
            v.channel.api(&RemoteConfig::default())
        );
        let source = Fixture::new(vec![
            (format!("{prefix}1"), json!({"artifacts":first})),
            (
                format!("{prefix}2"),
                json!({"artifacts":[
                    {"id":101,"name":"Receiver DataCollect_CDC #1","size_in_bytes":42,"expired":false,"digest":format!("sha256:{}", "a".repeat(64))},
                    {"id":102,"name":"Receiver","size_in_bytes":42,"expired":false}
                ]}),
            ),
        ]);
        let artifacts = list_artifacts_with(
            &RemoteConfig::default(),
            &source,
            &v,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(artifacts.len(), 2);
        let cdc = artifacts.iter().find(|a| a.id == 101).unwrap();
        assert_eq!(cdc.variant, "DataCollect CDC");
        assert!(cdc.url.contains("%20") && cdc.url.contains("%23"));
        assert_eq!(cdc.sha256.as_deref(), Some("a".repeat(64).as_str()));
    });
}

#[test]
fn latest_skips_expired_ci_and_ignores_other_workflows() {
    run(async {
        let channel = SourceChannel::ReceiverCi;
        let run = |id, number, path: &str, date: &str| {
            json!({"id":id,"run_number":number,"display_title":"Build",
    "created_at":date,"head_sha":"abcdef","head_branch":"dev","event":"push","status":"completed","conclusion":"success","path":path,"html_url":"https://github.com/run"})
        };
        let api = channel.api(&RemoteConfig::default());
        let source = Fixture::new(vec![
            (
                format!(
                    "{api}/actions/runs?branch=dev&event=push&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":[
        run(44,90,".github/workflows/docs.yml","2026-10-10"),
        run(43,89,".github/workflows/workflow.yml","2026-10-09"),
        run(42,88,".github/workflows/workflow.yml","2026-10-08")]}),
            ),
            (
                format!(
                    "{api}/actions/runs?branch=dev&event=workflow_dispatch&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":[]}),
            ),
            (
                format!("{api}/actions/runs/43/artifacts?per_page=100&page=1"),
                json!({"artifacts":[{"id":1,"name":"Receiver","size_in_bytes":1,"expired":true}]}),
            ),
            (
                format!("{api}/actions/runs/42/artifacts?per_page=100&page=1"),
                json!({"artifacts":[{"id":2,"name":"Receiver_DataCollect_CDC","size_in_bytes":1,"expired":false}]}),
            ),
        ]);
        assert_eq!(
            latest_with(
                &RemoteConfig::default(),
                &source,
                channel,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .unwrap()
            .id,
            42
        );
    });
}

#[test]
fn latest_skips_oversized_artifacts_but_accepts_exact_download_limit() {
    run(async {
        let limit = super::MAX_DOWNLOAD as u64;
        for channel in [
            SourceChannel::Release,
            SourceChannel::TrackerCi,
            SourceChannel::ReceiverCi,
        ] {
            for newest_size in [limit, limit + 1] {
                let api = channel.api(&RemoteConfig::default());
                let metadata = |id, size| {
                    if channel == SourceChannel::Release {
                        json!({"id":id,"name":"Receiver.hex","size":size,"browser_download_url":"https://github.com/image.hex"})
                    } else {
                        json!({"id":id,"name":"Firmware","size_in_bytes":size,"expired":false})
                    }
                };
                let mut replies = if channel == SourceChannel::Release {
                    vec![(
                        format!("{api}/releases?per_page=100&page=1"),
                        json!([
                            {"id":43,"tag_name":"261010","published_at":"2026-10-10","html_url":"https://github.com/release/43","assets":[metadata(43, newest_size)]},
                            {"id":42,"tag_name":"261009","published_at":"2026-10-09","html_url":"https://github.com/release/42","assets":[metadata(42, limit)]}
                        ]),
                    )]
                } else {
                    let run = |id, date| {
                        json!({"id":id,"run_number":200 + id,"display_title":"Build",
                "created_at":date,"head_sha":"abcdef","head_branch":"dev","event":"push",
                "status":"completed","conclusion":"success","path":".github/workflows/workflow.yml","html_url":"https://github.com/run"})
                    };
                    vec![
                        (
                            format!(
                                "{api}/actions/runs?branch=dev&event=push&status=success&per_page=100&page=1"
                            ),
                            json!({"workflow_runs":[run(43, "2026-10-10"), run(42, "2026-10-09")]}),
                        ),
                        (
                            format!(
                                "{api}/actions/runs?branch=dev&event=workflow_dispatch&status=success&per_page=100&page=1"
                            ),
                            json!({"workflow_runs":[]}),
                        ),
                    ]
                };
                let listed_ids = if newest_size > limit {
                    vec![43, 42]
                } else {
                    vec![43]
                };
                for id in listed_ids {
                    let asset = metadata(id, if id == 43 { newest_size } else { limit });
                    replies.push(if channel == SourceChannel::Release {
                        (
                            format!("{api}/releases/{id}/assets?per_page=100&page=1"),
                            json!([asset]),
                        )
                    } else {
                        (
                            format!("{api}/actions/runs/{id}/artifacts?per_page=100&page=1"),
                            json!({"artifacts":[asset]}),
                        )
                    });
                }
                let source = Fixture::new(replies);
                let chosen = latest_with(
                    &RemoteConfig::default(),
                    &source,
                    channel,
                    &AtomicBool::new(false),
                    &mut |_| {},
                )
                .await
                .unwrap();
                assert_eq!(chosen.id, if newest_size > limit { 42 } else { 43 });
                assert!(source.replies.lock().unwrap().is_empty());
            }
            // Deliberate selection remains explicit and performs no network request.
            let mut oversized = artifact(channel, b"firmware");
            oversized.size = limit + 1;
            let source = Fixture::new(Vec::new());
            assert!(
                download_with(
                    &RemoteConfig::default(),
                    &source,
                    &oversized,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .await
                .unwrap_err()
                .contains("64 MiB")
            );
        }
    });
}

#[test]
fn download_verifies_archive_then_preserves_uf2_hex_and_nested_package() {
    run(async {
        let nested = zip(&[
            ("manifest.json", b"{}"),
            ("app.bin", b"image"),
            ("app.dat", b"init"),
        ]);
        let bytes = zip(&[
            ("out/receiver.uf2", b"uf2"),
            ("out/receiver.hex", b"hex"),
            ("legacy.zip", &nested),
            ("notes.txt", b"ignore"),
        ]);
        let artifact = artifact(SourceChannel::ReceiverCi, &bytes);
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(artifact.url.clone(), Ok(bytes))])),
            token: false,
        };
        let files = download_with(
            &RemoteConfig::default(),
            &source,
            &artifact,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].bytes, nested);
        assert!(files.iter().all(
            |f| f.sha256 == hash(&f.bytes) && f.provenance.contains("matches upstream digest")
        ));
    });
}

#[test]
fn digest_mismatch_and_truncated_asset_fail_closed() {
    run(async {
        let bytes = b"firmware";
        let mut asset = artifact(SourceChannel::Release, bytes);
        asset.sha256 = Some("0".repeat(64));
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes.to_vec()))])),
            token: false,
        };
        assert!(
            download_with(
                &RemoteConfig::default(),
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .unwrap_err()
            .contains("SHA-256 mismatch")
        );
        asset.sha256 = None;
        asset.size += 1;
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes.to_vec()))])),
            token: false,
        };
        assert!(
            download_with(
                &RemoteConfig::default(),
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .unwrap_err()
            .contains("size differs")
        );
        assert!(parse_digest(Some("sha512:abc")).is_err());
    });
}

#[test]
fn public_ci_failure_can_use_authenticated_api_fallback() {
    run(async {
        let bytes = zip(&[("receiver.hex", b"image")]);
        let asset = artifact(SourceChannel::ReceiverCi, &bytes);
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([
                (asset.url.clone(), Err("Public link expired".into())),
                (
                    format!(
                        "{}/actions/artifacts/7/zip",
                        asset.version.channel.api(&RemoteConfig::default())
                    ),
                    Ok(bytes),
                ),
            ])),
            token: true,
        };
        assert_eq!(
            download_with(
                &RemoteConfig::default(),
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .unwrap()[0]
                .bytes,
            b"image"
        );
    });
}

#[test]
fn archive_rejects_traversal_duplicate_names_and_oversize() {
    run(async {
        for entries in [
            vec![("../bad.hex", b"bad".as_slice())],
            vec![("C:/bad.hex", b"bad".as_slice())],
            vec![("a.hex", b"a".as_slice()), ("A.hex", b"b".as_slice())],
        ] {
            assert!(
                extract_archive(
                    &zip(&entries),
                    "fixture",
                    SourceChannel::ReceiverCi,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .is_err()
            );
        }
        let oversized = vec![0u8; MAX_MEMBER + 1];
        assert!(
            extract_archive(
                &zip(&[("huge.hex", &oversized)]),
                "fixture",
                SourceChannel::ReceiverCi,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .unwrap_err()
            .contains("size limits")
        );
    });
}

#[test]
fn zip_envelope_and_link_metadata_fail_before_member_allocation() {
    run(async {
        let cancel = AtomicBool::new(false);
        let bytes = zip(&[("app.hex", b"image")]);
        let eocd = bytes.len() - 22;
        let mut excessive = bytes.clone();
        excessive[eocd + 8..eocd + 12].copy_from_slice(&[0x01, 0x04, 0x01, 0x04]);
        assert!(bounded_archive(&excessive, &cancel).is_err());
        let mut inconsistent = bytes.clone();
        inconsistent[eocd + 8..eocd + 12].copy_from_slice(&[0, 0, 0, 0]);
        assert!(bounded_archive(&inconsistent, &cancel).is_err());
        let mut split = bytes.clone();
        split[eocd + 4] = 1;
        assert!(bounded_archive(&split, &cancel).is_err());
        let central = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let mut linked = bytes;
        linked[central + 5] = 3; // Unix creator.
        linked[central + 38..central + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
        assert!(
            extract_archive(
                &linked,
                "fixture",
                SourceChannel::ReceiverCi,
                &cancel,
                &mut |_| {}
            )
            .unwrap_err()
            .contains("link")
        );
    });
}

#[test]
fn release_dfu_zip_is_retained_but_unsafe_package_is_rejected() {
    run(async {
        for (bytes, valid) in [
            (
                zip(&[("manifest.json", b"{}"), ("app.bin", b"image")]),
                true,
            ),
            (
                zip(&[("../manifest.json", b"{}"), ("app.bin", b"image")]),
                false,
            ),
        ] {
            let mut asset = artifact(SourceChannel::Release, &bytes);
            asset.name = "Legacy_DFU.zip".into();
            let source = Fixture {
                replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes.clone()))])),
                token: false,
            };
            let result = download_with(
                &RemoteConfig::default(),
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .await;
            if valid {
                assert_eq!(result.unwrap()[0].bytes, bytes);
            } else {
                assert!(result.is_err());
            }
        }
    });
}

#[test]
fn zip_member_sizes_are_exact_including_zero_and_crc_is_still_checked() {
    let cancel = AtomicBool::new(false);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "application.hex",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(b"firmware").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let local = u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
    for declared in [0u32, 7, 9] {
        let mut malformed = bytes.clone();
        malformed[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
        malformed[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
        assert!(
            extract_archive(
                &malformed,
                "fixture",
                SourceChannel::ReceiverCi,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
    }
    assert!(
        extract_archive(
            &zip(&[("application.hex", b"")]),
            "fixture",
            SourceChannel::ReceiverCi,
            &cancel,
            &mut |_| {}
        )
        .is_err()
    );
    let extracted = extract_archive(
        &bytes,
        "fixture",
        SourceChannel::ReceiverCi,
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(extracted[0].bytes, b"firmware");
    assert_eq!(
        extracted[0].sha256,
        "c3bf47ea1f4a4a605470313cacb3a44f4a461f68c6faeab07e737610cb5ac835"
    );
    let mut corrupt = bytes;
    corrupt[local + 14] ^= 1;
    corrupt[central + 16] ^= 1;
    assert!(
        extract_archive(
            &corrupt,
            "fixture",
            SourceChannel::ReceiverCi,
            &cancel,
            &mut |_| {}
        )
        .is_err()
    );
}

#[test]
fn bounded_member_reads_consume_at_most_declared_size_plus_one() {
    let cancel = AtomicBool::new(false);
    for declared in [0u64, 1, 2] {
        let mut input = Cursor::new(b"firmware");
        assert!(read_bounded(&mut input, 8, declared, &cancel, &mut |_| {}).is_err());
        assert_eq!(input.position(), declared + 1);
    }
    assert_eq!(
        read_bounded(&mut Cursor::new(b"abc"), 3, 3, &cancel, &mut |_| {}).unwrap(),
        b"abc"
    );
    assert_eq!(
        read_bounded(&mut Cursor::new(b""), 3, 0, &cancel, &mut |_| {}).unwrap(),
        b""
    );
    assert!(read_bounded(&mut Cursor::new(b"ab"), 3, 3, &cancel, &mut |_| {}).is_err());
    let mut input = Cursor::new(b"abc");
    assert!(read_bounded(&mut input, 2, 3, &cancel, &mut |_| {}).is_err());
    assert_eq!(input.position(), 0);
}

#[test]
fn cancel_and_stream_bounds_stop_before_publishing_bytes() {
    run(async {
        let cancel = AtomicBool::new(false);
        let data = vec![1u8; 128 * 1024];
        let result = read_bounded(
            &mut Cursor::new(&data),
            data.len(),
            data.len() as u64,
            &cancel,
            &mut |_| cancel.store(true, Ordering::Relaxed),
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(
            read_bounded(
                &mut Cursor::new(b"123"),
                2,
                0,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .is_err()
        );
        assert!(
            read_bounded(
                &mut Cursor::new(b"123"),
                4,
                4,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .is_err()
        );
    });
}

#[test]
fn transport_causes_preserve_diagnostics_without_credentials_or_urls() {
    run(async {
        #[derive(Debug)]
        struct Cause {
            message: &'static str,
            source: Option<Box<Cause>>,
        }
        impl std::fmt::Display for Cause {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.message)
            }
        }
        impl std::error::Error for Cause {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.source
                    .as_deref()
                    .map(|cause| cause as &dyn std::error::Error)
            }
        }
        let error = Cause {
            message: "error sending request (https://user:password@nightly.link/private?sig=private-signature)",
            source: Some(Box::new(Cause {
                message: "client error (Connect), Bearer ghp_private_token sig=detached-signature ?se=private-expiry",
                source: Some(Box::new(Cause {
                    message: "dns error: failed to lookup address information (os error 11001)",
                    source: None,
                })),
            })),
        };
        let message = safe_error_chain(&error, Some("ghp_private_token"));
        assert!(message.contains("client error (Connect)"));
        assert!(
            message.contains("dns error: failed to lookup address information (os error 11001)")
        );
        for secret in [
            "https://",
            "user",
            "password",
            "private",
            "signature",
            "?se",
            "sig=",
        ] {
            assert!(
                !message.contains(secret),
                "Sensitive diagnostic fragment {secret}"
            );
        }
        let tls = Cause {
            message: "invalid peer certificate: UnknownIssuer",
            source: None,
        };
        assert_eq!(
            safe_error_chain(&tls, None),
            "invalid peer certificate: UnknownIssuer"
        );
    });
}

#[test]
fn redirects_and_error_messages_are_restricted_and_actionable() {
    run(async {
        for url in [
            "http://github.com/a",
            "https://github.com.evil.test/a",
            "https://user:secret@api.github.com/a",
            "https://github.com:444/a",
        ] {
            assert!(checked_url(url).is_err());
        }
        assert!(checked_url("https://productionresultssa.blob.core.windows.net/artifact").is_ok());
        assert!(status_error(403, Some("0"), Some("123")).contains("rate limit"));
        assert!(status_error(401, None, None).contains("GH_TOKEN"));
        assert!(status_error(410, None, None).contains("expired"));
    });
}

// Recorded upstream metadata: Adafruit_nRF52_Bootloader release 0.11.1-slimenrf.
const BOOTLOADER_NAME: &str =
    "update-slimenrf_receiver_aerorx_r1_bootloader-0.11.1-slimenrf_nosd.uf2";
const BOOTLOADER_DIGEST: &str = "bf1a73ec739f74ac3d5223188eec697b97abcc839a3e0e439e3320c1a258a810";

fn bootloader_asset_metadata() -> serde_json::Value {
    json!({
        "id":622821323,
        "name":BOOTLOADER_NAME,
        "size":74752,
        "digest":format!("sha256:{BOOTLOADER_DIGEST}"),
        "browser_download_url":format!(
            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/download/0.11.1-slimenrf/{BOOTLOADER_NAME}"
        )
    })
}

fn bootloader_release(id: u64, tag: &str, date: &str) -> serde_json::Value {
    json!({
        "id":id, "tag_name":tag, "published_at":date,
        "html_url":format!("https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/{tag}"),
        "assets":[bootloader_asset_metadata()]
    })
}

fn bootloader_version() -> Version {
    Version {
        channel: SourceChannel::BootloaderRelease,
        id: 407199605,
        name: "0.11.1-slimenrf".into(),
        published_at: "2026-10-08T19:40:32Z".into(),
        commit: None,
        url: "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.1-slimenrf"
            .into(),
    }
}

fn bootloader_artifact(bytes: &[u8]) -> Artifact {
    let version = bootloader_version();
    Artifact {
        name: BOOTLOADER_NAME.into(),
        variant: "Bootloader".into(),
        size: bytes.len() as u64,
        url: format!(
            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/download/{}/{BOOTLOADER_NAME}",
            version.name
        ),
        version,
        sha256: Some(hash(bytes)),
        id: 622821323,
    }
}

#[test]
fn configured_repositories_route_every_channel_independently() {
    run(async {
        let mut config = RemoteConfig::default();
        let repository = |name: &str| GithubRepository {
            owner: "configured-owner".into(),
            name: name.into(),
        };
        config.application.releases = repository("application-releases");
        config.application.tracker_ci = repository("tracker-builds");
        config.application.receiver_ci = repository("receiver-builds");
        config.bootloader.releases = repository("bootloader-releases");
        config.bootloader.ci = repository("bootloader-builds");
        for (channel, name) in [
            (SourceChannel::Release, "application-releases"),
            (SourceChannel::TrackerCi, "tracker-builds"),
            (SourceChannel::ReceiverCi, "receiver-builds"),
            (SourceChannel::BootloaderRelease, "bootloader-releases"),
            (SourceChannel::BootloaderCi, "bootloader-builds"),
        ] {
            let api = format!("https://api.github.com/repos/configured-owner/{name}");
            let replies = if channel.is_release() {
                vec![(format!("{api}/releases?per_page=100&page=1"), json!([]))]
            } else {
                let branch = if channel == SourceChannel::BootloaderCi {
                    "master"
                } else {
                    "dev"
                };
                ["push", "workflow_dispatch"]
                    .into_iter()
                    .map(|event| {
                        (
                            format!("{api}/actions/runs?branch={branch}&event={event}&status=success&per_page=100&page=1"),
                            json!({"workflow_runs":[]}),
                        )
                    })
                    .collect()
            };
            let source = Fixture::new(replies);
            assert!(
                list_versions_with(
                    &config,
                    &source,
                    channel,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .await
                .unwrap()
                .is_empty()
            );
            assert!(source.replies.lock().unwrap().is_empty());

            let v = version(channel);
            let reply = if channel.is_release() {
                (
                    format!("{api}/releases/42/assets?per_page=100&page=1"),
                    json!([]),
                )
            } else {
                (
                    format!("{api}/actions/runs/42/artifacts?per_page=100&page=1"),
                    json!({"artifacts":[{"id":7,"name":"Firmware #1","size_in_bytes":1,"expired":false}]}),
                )
            };
            let source = Fixture::new(vec![reply]);
            let artifacts =
                list_artifacts_with(&config, &source, &v, &AtomicBool::new(false), &mut |_| {})
                    .await
                    .unwrap();
            if !channel.is_release() {
                assert_eq!(
                    artifacts[0].url,
                    format!(
                        "https://nightly.link/configured-owner/{name}/actions/runs/42/Firmware%20%231.zip"
                    )
                );
            }
            assert!(source.replies.lock().unwrap().is_empty());
        }
    });
}

#[test]
fn configured_ci_download_fallback_uses_selected_repository() {
    run(async {
        let mut config = RemoteConfig::default();
        config.application.tracker_ci = GithubRepository {
            owner: "build-owner".into(),
            name: "tracker-fork".into(),
        };
        let bytes = zip(&[("tracker.uf2", b"image")]);
        let asset = artifact(SourceChannel::TrackerCi, &bytes);
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([
                (asset.url.clone(), Err("Public link expired".into())),
                (
                    "https://api.github.com/repos/build-owner/tracker-fork/actions/artifacts/7/zip"
                        .into(),
                    Ok(bytes),
                ),
            ])),
            token: true,
        };
        let files = download_with(
            &config,
            &source,
            &asset,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(files[0].bytes, b"image");
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_semver_listing_has_no_application_date_gate_or_ci_requests() {
    run(async {
        let config = RemoteConfig::default();
        let api = "https://api.github.com/repos/jitingcn/Adafruit_nRF52_Bootloader";
        let mut draft = bootloader_release(4, "0.12.0-slimenrf", "2026-10-10");
        draft["draft"] = json!(true);
        let mut prerelease = bootloader_release(3, "0.12.0-rc1", "2026-10-09");
        prerelease["prerelease"] = json!(true);
        let mut unsupported = bootloader_release(5, "0.13.0", "2026-10-11");
        unsupported["assets"][0]["name"] = json!("slimenrf_bootloader_nosd.hex");
        let source = Fixture::new(vec![(
            format!("{api}/releases?per_page=100&page=1"),
            json!([
                bootloader_release(1, "0.11.0-slimenrf", "2025-01-01"),
                bootloader_release(407199605, "0.11.1-slimenrf", "2026-10-08T19:40:32Z"),
                prerelease,
                draft,
                unsupported,
            ]),
        )]);
        let versions = list_versions_with(
            &config,
            &source,
            SourceChannel::BootloaderRelease,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            versions.iter().map(|v| v.id).collect::<Vec<_>>(),
            [3, 407199605, 1]
        );
        assert_eq!(versions[0].name, "0.12.0-rc1 (prerelease)");
        assert!(
            versions
                .iter()
                .all(|v| v.channel.is_release() && v.commit.is_none())
        );
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_assets_keep_actual_update_metadata_and_filter_full_images() {
    run(async {
        let config = RemoteConfig::default();
        let v = bootloader_version();
        let mut assets = vec![bootloader_asset_metadata()];
        let mut newer = bootloader_asset_metadata();
        newer["id"] = json!(622821324_u64);
        newer["name"] =
            json!("update-slimenrf_receiver_aerorx_r1_bootloader-0.12.0-slimenrf_nosd.uf2");
        newer["browser_download_url"] = json!(
            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/download/0.12.0/update-slimenrf_receiver_aerorx_r1_bootloader-0.12.0-slimenrf_nosd.uf2"
        );
        assets.push(newer);
        for name in [
            "slimenrf_receiver_aerorx_r1_bootloader-0.11.1-slimenrf_nosd.hex",
            "slimenrf_receiver_aerorx_r1_bootloader-0.11.1-slimenrf_s140_7.3.0.zip",
            "slimenrf_receiver_aerorx_r1_bootloader-0.11.1-slimenrf_nosd.uf2",
            "update-SlimeNRF_Receiver.uf2",
            "update-board_bootloader.hex",
            "update-board_bootloader.uf2.zip",
            "update-board_bootloader_nosd.uf2",
            "update-board_bootloader-0.11.1_s140_7.3.0.uf2",
            "update-board_bootloader-0.11.1_nosd.uf2.zip",
            "path/update-board_bootloader-0.11.1_nosd.uf2",
            "update-board_bootloader-0.11.1_nosd.uf2.exe",
            "SmolFlasher.exe",
        ] {
            let mut asset = bootloader_asset_metadata();
            asset["name"] = json!(name);
            asset["digest"] = json!("invalid digest on ineligible asset");
            assets.push(asset);
            assert!(!super::is_bootloader_update(name));
        }
        assert!(!super::is_bootloader_update(
            &BOOTLOADER_NAME.to_uppercase()
        ));
        let source = Fixture::new(vec![(
    "https://api.github.com/repos/jitingcn/Adafruit_nRF52_Bootloader/releases/407199605/assets?per_page=100&page=1".into(),
    json!(assets),
)]);
        let artifacts =
            list_artifacts_with(&config, &source, &v, &AtomicBool::new(false), &mut |_| {})
                .await
                .unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].name, BOOTLOADER_NAME);
        assert_eq!(artifacts[0].id, 622821323);
        assert_eq!(artifacts[0].size, 74752);
        assert_eq!(artifacts[0].sha256.as_deref(), Some(BOOTLOADER_DIGEST));
        assert_eq!(artifacts[0].variant, "Bootloader");
        assert!(artifacts[0].url.ends_with(BOOTLOADER_NAME));
        assert_eq!(artifacts[1].id, 622821324);
        assert_eq!(
            artifacts[1].name,
            "update-slimenrf_receiver_aerorx_r1_bootloader-0.12.0-slimenrf_nosd.uf2"
        );
        assert!(artifacts[1].url.ends_with(&artifacts[1].name));
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_latest_uses_configured_release_assets_and_skips_unavailable_versions() {
    run(async {
        let mut config = RemoteConfig::default();
        config.bootloader.releases = GithubRepository {
            owner: "boot-owner".into(),
            name: "boot-fork".into(),
        };
        let api = "https://api.github.com/repos/boot-owner/boot-fork";
        let mut oversized = bootloader_asset_metadata();
        oversized["size"] = json!(super::MAX_DOWNLOAD as u64 + 1);
        let source = Fixture::new(vec![
            (
                format!("{api}/releases?per_page=100&page=1"),
                json!([
                    bootloader_release(44, "0.13.0", "2026-10-10"),
                    bootloader_release(43, "0.12.0", "2026-10-09"),
                    bootloader_release(407199605, "0.11.1-slimenrf", "2026-10-08"),
                ]),
            ),
            (
                format!("{api}/releases/44/assets?per_page=100&page=1"),
                json!([]),
            ),
            (
                format!("{api}/releases/43/assets?per_page=100&page=1"),
                json!([oversized]),
            ),
            (
                format!("{api}/releases/407199605/assets?per_page=100&page=1"),
                json!([bootloader_asset_metadata()]),
            ),
        ]);
        let latest = latest_with(
            &config,
            &source,
            SourceChannel::BootloaderRelease,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(latest.id, 407199605);
        assert_eq!(latest.name, "0.11.1-slimenrf");
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_latest_empty_releases_never_fall_back_to_ci() {
    run(async {
        let config = RemoteConfig::default();
        let source = Fixture::new(vec![(
    "https://api.github.com/repos/jitingcn/Adafruit_nRF52_Bootloader/releases?per_page=100&page=1".into(),
    json!([]),
)]);
        assert!(
            latest_with(
                &config,
                &source,
                SourceChannel::BootloaderRelease,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .is_err()
        );
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_release_download_retains_uf2_and_checks_digest_and_size() {
    run(async {
        let config = RemoteConfig::default();
        let bytes = b"abc";
        let mut asset = bootloader_artifact(bytes);
        // Known SHA-256 vector is independent of the acquisition hashing implementation.
        asset.sha256 =
            Some("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into());
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes.to_vec()))])),
            token: true,
        };
        let files = download_with(
            &config,
            &source,
            &asset,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, BOOTLOADER_NAME);
        assert_eq!(files[0].bytes, bytes);
        assert_eq!(
            files[0].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(files[0].provenance.contains("matches upstream digest"));
        assert!(source.replies.lock().unwrap().is_empty());

        for (digest, size, error) in [
            (Some("0".repeat(64)), bytes.len() as u64, "SHA-256 mismatch"),
            (None, bytes.len() as u64 + 1, "size differs"),
        ] {
            asset.sha256 = digest;
            asset.size = size;
            let source = Fixture {
                replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes.to_vec()))])),
                token: true,
            };
            assert!(
                download_with(
                    &config,
                    &source,
                    &asset,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .await
                .unwrap_err()
                .contains(error)
            );
            assert!(source.replies.lock().unwrap().is_empty());
        }
    });
}

#[test]
fn bootloader_download_failure_never_uses_authenticated_ci_fallback() {
    run(async {
        let config = RemoteConfig::default();
        let asset = bootloader_artifact(b"image");
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(
                asset.url.clone(),
                Err("Release asset unavailable".into()),
            )])),
            token: true,
        };
        let error = download_with(
            &config,
            &source,
            &asset,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Release asset unavailable");
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_download_rejects_ineligible_direct_selection_before_network() {
    run(async {
        let config = RemoteConfig::default();
        let mut asset = bootloader_artifact(b"image");
        asset.name = "slimenrf_board_bootloader.hex".into();
        let source = Fixture::new(Vec::new());
        assert!(
            download_with(
                &config,
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .await
            .unwrap_err()
            .contains("update-*bootloader*.uf2")
        );
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

fn bootloader_ci_run(id: u64, number: u64, date: &str, event: &str) -> serde_json::Value {
    json!({
        "id":id, "run_number":number, "display_title":"Build bootloader",
        "created_at":date, "head_sha":"7f0654b898e92dd13f89c4fef108559a3d1c4b0e",
        "head_branch":"master", "event":event, "status":"completed", "conclusion":"success",
        "path":".github/workflows/githubci.yml", "html_url":format!("https://github.com/build/runs/{id}")
    })
}

#[test]
fn bootloader_ci_lists_low_successful_runs_with_pagination_dedup_and_exact_policy() {
    run(async {
        let channel = SourceChannel::BootloaderCi;
        let config = RemoteConfig::default();
        let api = channel.api(&config);
        let accepted = bootloader_ci_run(47, 47, "2026-10-24", "push");
        let mut first_page = vec![accepted.clone()];
        for (field, value) in [
            ("status", json!("in_progress")),
            ("conclusion", json!("failure")),
            ("conclusion", serde_json::Value::Null),
            ("head_branch", json!("dev")),
            ("head_branch", serde_json::Value::Null),
            ("event", json!("pull_request")),
            ("event", json!("release")),
            ("path", json!(".github/workflows/workflow.yml")),
            ("path", json!(".github/workflows/docs.yml")),
        ] {
            let mut rejected = accepted.clone();
            rejected["id"] = json!(100 + first_page.len());
            rejected[field] = value;
            first_page.push(rejected);
        }
        while first_page.len() < 100 {
            let mut rejected = accepted.clone();
            rejected["id"] = json!(100 + first_page.len());
            rejected["path"] = json!(".github/workflows/docs.yml");
            first_page.push(rejected);
        }
        let source = Fixture::new(vec![
            (
                format!(
                    "{api}/actions/runs?branch=master&event=push&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":first_page}),
            ),
            (
                format!(
                    "{api}/actions/runs?branch=master&event=push&status=success&per_page=100&page=2"
                ),
                json!({"workflow_runs":[accepted, bootloader_ci_run(45, 45, "2026-10-23", "push")]}),
            ),
            (
                format!(
                    "{api}/actions/runs?branch=master&event=workflow_dispatch&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":[
                    bootloader_ci_run(47, 47, "2026-10-24", "workflow_dispatch"),
                    bootloader_ci_run(1, 1, "2026-10-25", "workflow_dispatch"),
                    bootloader_ci_run(2, 2, "2026-10-25", "workflow_dispatch"),
                    bootloader_ci_run(3, 3, "2026-10-26", "push"),
                ]}),
            ),
        ]);
        let versions = list_versions_with(
            &config,
            &source,
            channel,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            versions.iter().map(|v| v.id).collect::<Vec<_>>(),
            [2, 1, 47, 45]
        );
        assert!(versions.iter().all(|v| {
            v.channel == channel
                && v.commit.as_deref() == Some("7f0654b898e92dd13f89c4fef108559a3d1c4b0e")
        }));
        assert!(versions[1].name.starts_with("#1 "));
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn application_ci_keeps_existing_run_thresholds_and_workflow_policy() {
    run(async {
        for (channel, minimum) in [
            (SourceChannel::TrackerCi, 191),
            (SourceChannel::ReceiverCi, 68),
        ] {
            let config = RemoteConfig::default();
            let api = channel.api(&config);
            let mut accepted = bootloader_ci_run(42, minimum, "2026-10-24", "push");
            accepted["head_branch"] = json!("dev");
            accepted["path"] = json!(".github/workflows/workflow.yml");
            let mut too_old = accepted.clone();
            too_old["id"] = json!(43);
            too_old["run_number"] = json!(minimum - 1);
            let source = Fixture::new(vec![
                (
                    format!(
                        "{api}/actions/runs?branch=dev&event=push&status=success&per_page=100&page=1"
                    ),
                    json!({"workflow_runs":[
                        accepted,
                        too_old,
                        bootloader_ci_run(47, 47, "2026-10-25", "push"),
                    ]}),
                ),
                (
                    format!(
                        "{api}/actions/runs?branch=dev&event=workflow_dispatch&status=success&per_page=100&page=1"
                    ),
                    json!({"workflow_runs":[]}),
                ),
            ]);
            let versions = list_versions_with(
                &config,
                &source,
                channel,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .await
            .unwrap();
            assert_eq!(versions.iter().map(|v| v.id).collect::<Vec<_>>(), [42]);
            assert!(source.replies.lock().unwrap().is_empty());
        }
    });
}

#[test]
fn bootloader_ci_latest_skips_expired_newest_and_keeps_container_metadata() {
    run(async {
        let config = RemoteConfig::default();
        let channel = SourceChannel::BootloaderCi;
        let api = channel.api(&config);
        let active_artifacts = json!({"artifacts":[
            {"id":8,"name":"slimenrf_receiver_aerorx_r1","size_in_bytes":400,"expired":false},
            {"id":7,"name":"slimenrf_promicro","size_in_bytes":422724,"expired":false,
             "digest":format!("sha256:{}", "a".repeat(64))},
        ]});
        let source = Fixture::new(vec![
            (
                format!(
                    "{api}/actions/runs?branch=master&event=push&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":[
                    bootloader_ci_run(47, 47, "2026-10-25", "push"),
                    bootloader_ci_run(45, 45, "2026-10-24", "push"),
                ]}),
            ),
            (
                format!(
                    "{api}/actions/runs?branch=master&event=workflow_dispatch&status=success&per_page=100&page=1"
                ),
                json!({"workflow_runs":[]}),
            ),
            (
                format!("{api}/actions/runs/47/artifacts?per_page=100&page=1"),
                json!({"artifacts":[
                    {"id":9,"name":"slimenrf_promicro","size_in_bytes":1,"expired":true,"digest":"unavailable"},
                ]}),
            ),
            (
                format!("{api}/actions/runs/45/artifacts?per_page=100&page=1"),
                active_artifacts.clone(),
            ),
            (
                format!("{api}/actions/runs/45/artifacts?per_page=100&page=1"),
                active_artifacts,
            ),
        ]);
        let latest = latest_with(
            &config,
            &source,
            channel,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(latest.id, 45);
        let artifacts = list_artifacts_with(
            &config,
            &source,
            &latest,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].name, "slimenrf_promicro");
        assert_eq!(artifacts[0].variant, "Bootloader");
        assert_eq!(artifacts[0].id, 7);
        assert_eq!(artifacts[0].size, 422724);
        assert_eq!(
            artifacts[0].sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            artifacts[0].url,
            "https://nightly.link/jitingcn/Adafruit_nRF52_Bootloader/actions/runs/45/slimenrf_promicro.zip"
        );
        assert!(
            artifacts
                .iter()
                .all(|a| a.variant == "Bootloader" && a.version.id == latest.id)
        );
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_ci_download_returns_only_strict_updaters_with_real_paths_and_provenance() {
    run(async {
        let nested = zip(&[("manifest.json", b"{}"), ("bootloader.bin", b"full image")]);
        let updater = "out/update-slimenrf_promicro_bootloader-0.11.1-slimenrf-3-g7f0654b_nosd.uf2";
        let other_updater = format!("out/{BOOTLOADER_NAME}");
        let bytes = zip(&[
            (updater, b"updater"),
            (&other_updater, b"independent updater"),
            ("out/slimenrf_promicro_bootloader-0.11.1_nosd.hex", b"hex"),
            (
                "out/slimenrf_promicro_bootloader-0.11.1_nosd.uf2",
                b"full uf2",
            ),
            ("out/slimenrf_promicro_bootloader-0.11.1_s140.zip", &nested),
            (
                "out/update-board_bootloader-0.11.1_s140.uf2",
                b"wrong softdevice",
            ),
            ("out/update-board_bootloader_nosd.uf2", b"no version"),
            ("out/UPDATE-board_bootloader-0.11.1_nosd.uf2", b"wrong case"),
            (
                "out/update-board_bootloader-0.11.1_nosd.uf2.bin",
                b"not uf2",
            ),
        ]);
        let mut asset = artifact(SourceChannel::BootloaderCi, &bytes);
        asset.name = "slimenrf_promicro".into();
        asset.variant = "Bootloader".into();
        asset.version.commit = Some("7f0654b".into());
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes))])),
            token: false,
        };
        let files = download_with(
            &RemoteConfig::default(),
            &source,
            &asset,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(files.len(), 2);
        let promicro = files.iter().find(|file| file.name == updater).unwrap();
        assert_eq!(promicro.bytes, b"updater");
        let other = files
            .iter()
            .find(|file| file.name == other_updater)
            .unwrap();
        assert_eq!(other.bytes, b"independent updater");
        assert!(files.iter().all(|file| {
            file.sha256 == hash(&file.bytes)
                && file.provenance.contains(&format!("member {}", file.name))
                && file.provenance.contains("7f0654b")
                && file.provenance.contains("matches upstream digest")
        }));
        assert!(source.replies.lock().unwrap().is_empty());
    });
}

#[test]
fn bootloader_ci_archive_without_updater_is_unavailable() {
    let nested = zip(&[("manifest.json", b"{}"), ("bootloader.bin", b"full")]);
    let bytes = zip(&[
        ("slimenrf_promicro_bootloader-0.11.1_nosd.hex", b"hex"),
        ("slimenrf_promicro_bootloader-0.11.1_nosd.uf2", b"full"),
        ("slimenrf_promicro_bootloader-0.11.1_s140.zip", &nested),
    ]);
    let error = extract_archive(
        &bytes,
        "fixture",
        SourceChannel::BootloaderCi,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.contains("no bootloader self-update UF2"), "{error}");
}

#[test]
fn bootloader_ci_rejects_unsafe_excluded_members_and_nested_packages() {
    let cancel = AtomicBool::new(false);
    let unsafe_nested = zip(&[("../manifest.json", b"{}")]);
    for entries in [
        vec![
            (BOOTLOADER_NAME, b"updater".as_slice()),
            ("../excluded.txt", b"unsafe".as_slice()),
        ],
        vec![
            (BOOTLOADER_NAME, b"updater".as_slice()),
            ("excluded.hex", b"a".as_slice()),
            ("EXCLUDED.hex", b"b".as_slice()),
        ],
        vec![
            (BOOTLOADER_NAME, b"updater".as_slice()),
            ("excluded.zip", unsafe_nested.as_slice()),
        ],
        vec![
            (BOOTLOADER_NAME, b"updater".as_slice()),
            ("excluded.hex", b"".as_slice()),
        ],
    ] {
        assert!(
            extract_archive(
                &zip(&entries),
                "fixture",
                SourceChannel::BootloaderCi,
                &cancel,
                &mut |_| {},
            )
            .is_err()
        );
    }
    let bytes = zip(&[
        ("excluded.hex", b"full image"),
        (BOOTLOADER_NAME, b"updater"),
    ]);
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let local = u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
    let mut linked = bytes.clone();
    linked[central + 5] = 3;
    linked[central + 38..central + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
    assert!(
        extract_archive(
            &linked,
            "fixture",
            SourceChannel::BootloaderCi,
            &cancel,
            &mut |_| {},
        )
        .unwrap_err()
        .contains("link")
    );
    let mut oversized = bytes.clone();
    oversized[local + 22..local + 26].copy_from_slice(&((MAX_MEMBER + 1) as u32).to_le_bytes());
    oversized[central + 24..central + 28].copy_from_slice(&((MAX_MEMBER + 1) as u32).to_le_bytes());
    assert!(
        extract_archive(
            &oversized,
            "fixture",
            SourceChannel::BootloaderCi,
            &cancel,
            &mut |_| {},
        )
        .unwrap_err()
        .contains("size limits")
    );
    let mut corrupt = bytes;
    corrupt[local + 14] ^= 1;
    corrupt[central + 16] ^= 1;
    assert!(
        extract_archive(
            &corrupt,
            "fixture",
            SourceChannel::BootloaderCi,
            &cancel,
            &mut |_| {},
        )
        .is_err()
    );
}

#[test]
fn bootloader_ci_configured_public_download_and_token_fallback_preserve_digest_checks() {
    run(async {
        let mut config = RemoteConfig::default();
        config.bootloader.ci = GithubRepository {
            owner: "boot-owner".into(),
            name: "boot-builds".into(),
        };
        let bytes = zip(&[(BOOTLOADER_NAME, b"updater")]);
        let mut asset = artifact(SourceChannel::BootloaderCi, &bytes);
        asset.name = "slimenrf_promicro".into();
        asset.url =
            "https://nightly.link/boot-owner/boot-builds/actions/runs/42/slimenrf_promicro.zip"
                .into();
        for token in [false, true] {
            let mut replies =
                VecDeque::from([(asset.url.clone(), Err("Public link expired".into()))]);
            if token {
                replies.push_back((
                    "https://api.github.com/repos/boot-owner/boot-builds/actions/artifacts/7/zip"
                        .into(),
                    Ok(bytes.clone()),
                ));
            }
            let source = Fixture {
                replies: Mutex::new(replies),
                token,
            };
            let result = download_with(
                &config,
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .await;
            if token {
                let files = result.unwrap();
                assert_eq!(files[0].bytes, b"updater");
                assert!(files[0].provenance.contains(
                    "https://api.github.com/repos/boot-owner/boot-builds/actions/artifacts/7/zip"
                ));
            } else {
                assert!(result.unwrap_err().contains("Public link expired"));
            }
            assert!(source.replies.lock().unwrap().is_empty());
        }
        asset.sha256 = Some("0".repeat(64));
        let source = Fixture {
            replies: Mutex::new(VecDeque::from([(asset.url.clone(), Ok(bytes))])),
            token: true,
        };
        assert!(
            download_with(
                &config,
                &source,
                &asset,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .await
            .unwrap_err()
            .contains("SHA-256 mismatch")
        );
        assert!(source.replies.lock().unwrap().is_empty());
    });
}
