use super::release::{Architecture, ReleaseInfo, Variant};
use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use reqwest::{Client, Url};
use serde_json::{Value, json};

const REPOSITORY_PATH: &str = "/repos/jitingcn/SlimeNRF-Helper";
const ABC_DIGEST: &str = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const ABC_DIGEST_BYTES: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
    0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
];

fn releases_path(page: usize) -> String {
    format!("{REPOSITORY_PATH}/releases?per_page=100&page={page}")
}

fn release_path(id: u64) -> String {
    format!("{REPOSITORY_PATH}/releases/{id}")
}

fn assets_path(id: u64, page: usize) -> String {
    format!("{REPOSITORY_PATH}/releases/{id}/assets?per_page=100&page={page}")
}

fn binary_path(id: u64) -> String {
    format!("{REPOSITORY_PATH}/releases/assets/{id}")
}

fn release(id: u64, tag: &str, prerelease: bool, draft: bool) -> Value {
    json!({
        "id": id,
        "tag_name": tag,
        "draft": draft,
        "prerelease": prerelease,
        "body": "fixture release notes"
    })
}

fn asset(id: u64, version: &str) -> Value {
    json!({
        "id": id,
        "name": format!("slimenrf-helper-{version}-windows-x64-no-firmware.exe"),
        "state": "uploaded",
        "size": 3,
        "digest": ABC_DIGEST
    })
}

fn unrelated_assets() -> Vec<Value> {
    (0..100)
        .map(|index| {
            json!({
                "id": 1000 + index,
                "name": format!("unrelated-{index}.zip"),
                "state": "uploaded",
                "size": 1,
                "digest": null
            })
        })
        .collect()
}

struct Reply {
    path: String,
    accept: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn json(path: String, value: Value) -> Self {
        Self {
            path,
            accept: "application/vnd.github+json",
            content_type: "application/json",
            body: serde_json::to_vec(&value).expect("serialize fixture metadata"),
        }
    }

    fn binary(path: String, bytes: &[u8]) -> Self {
        Self {
            path,
            accept: "application/octet-stream",
            content_type: "application/octet-stream",
            body: bytes.to_vec(),
        }
    }
}

struct Fixture {
    network: Network,
    expected: Vec<(String, &'static str)>,
    stop: Option<mpsc::Sender<()>>,
    server: Option<thread::JoinHandle<Vec<String>>>,
}

impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback fixture");
        let origin = Url::parse(&format!(
            "http://{}/",
            listener.local_addr().expect("loopback address")
        ))
        .expect("parse loopback origin");
        listener
            .set_nonblocking(true)
            .expect("make loopback listener nonblocking");
        let expected = replies
            .iter()
            .map(|reply| (reply.path.clone(), reply.accept))
            .collect();
        let (stop, stopped) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut replies = replies.into_iter();
            let mut requests = Vec::new();
            loop {
                match stopped.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                assert!(
                    Instant::now() < deadline,
                    "loopback fixture deadline expired"
                );
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Winsock can inherit nonblocking mode from the listener.
                        stream
                            .set_nonblocking(false)
                            .expect("make accepted socket blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .expect("set request deadline");
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .expect("set response deadline");
                        requests.push(read_request(&mut stream));
                        let (status, content_type, body) = match replies.next() {
                            Some(reply) => ("200 OK", reply.content_type, reply.body),
                            None => ("404 Not Found", "text/plain", Vec::new()),
                        };
                        write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .expect("write fixture response headers");
                        stream
                            .write_all(&body)
                            .expect("write fixture response body");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("loopback accept failed: {error}"),
                }
            }
            requests
        });
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build isolated loopback client");
        Self {
            network: Network::for_test(client, origin),
            expected,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn finish(mut self) {
        self.stop
            .take()
            .expect("fixture stop sender")
            .send(())
            .expect("stop loopback fixture");
        let requests = self
            .server
            .take()
            .expect("fixture server thread")
            .join()
            .expect("join loopback fixture");
        let lines: Vec<_> = requests
            .iter()
            .map(|request| request.lines().next().expect("HTTP request line"))
            .collect();
        let expected_lines: Vec<_> = self
            .expected
            .iter()
            .map(|(path, _)| format!("GET {path} HTTP/1.1"))
            .collect();
        assert_eq!(lines, expected_lines, "complete request order");
        for (request, (path, accept)) in requests.iter().zip(&self.expected) {
            let headers = request.to_ascii_lowercase();
            assert!(
                headers.contains("\r\naccept-encoding: identity\r\n"),
                "missing identity encoding for {path}"
            );
            assert!(
                headers.contains("\r\nx-github-api-version: 2022-11-28\r\n"),
                "missing GitHub API version for {path}"
            );
            assert!(
                headers.contains(&format!("\r\naccept: {accept}\r\n")),
                "incorrect Accept header for {path}"
            );
            assert!(
                !headers.contains("\r\nauthorization:"),
                "credentials must never be sent to {path}"
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).expect("read request headers");
        bytes.push(byte[0]);
        assert!(bytes.len() < 16 * 1024, "fixture request headers too large");
    }
    String::from_utf8(bytes).expect("fixture request headers are UTF-8")
}

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(20), future)
                .await
                .expect("update pipeline test deadline");
        });
}

fn target() -> Target {
    Target {
        arch: Architecture::X64,
        variant: Variant::NoFirmware,
    }
}

async fn check_fixture(fixture: &Fixture, channel: Channel) -> Result<CheckResult, String> {
    check_with(
        &fixture.network,
        channel,
        CheckKind::Portable(target()),
        &Version::new(1, 0, 0),
        Arc::new(AtomicBool::new(false)),
    )
    .await
}

async fn check_page_fixture(fixture: &Fixture, channel: Channel) -> Result<CheckResult, String> {
    check_with(
        &fixture.network,
        channel,
        CheckKind::ReleasePage,
        &Version::new(1, 0, 0),
        Arc::new(AtomicBool::new(false)),
    )
    .await
}

fn available(result: Result<CheckResult, String>) -> Release {
    match result.expect("check release fixture") {
        CheckResult::Available(release) => *release,
        other => panic!("expected an available release, got {other:?}"),
    }
}

fn release_page(result: Result<CheckResult, String>) -> ReleaseInfo {
    match result.expect("check release page fixture") {
        CheckResult::ReleasePage(release) => *release,
        other => panic!("expected release page metadata, got {other:?}"),
    }
}

#[test]
fn test_builds_check_the_release_page_without_portable_installation() {
    assert!(
        matches!(current_check_kind(), CheckKind::ReleasePage),
        "test builds cannot install portable updates"
    );
    assert!(
        !installable_now(),
        "test builds must not offer portable installation"
    );
}

#[test]
fn release_page_selects_highest_valid_version_and_freezes_trusted_metadata() {
    run(async {
        let mut newest = release(3, "1.10.0+build.7", false, false);
        newest["body"] = json!("Selected release notes\nWith details.");
        newest["html_url"] = json!("https://untrusted.invalid/not-the-release");
        let fixture = Fixture::new(vec![Reply::json(
            releases_path(1),
            json!([
                release(2, "v1.9.99", false, false),
                release(4, "v42.0", false, false),
                release(1, "v1.0.0", false, false),
                newest,
                release(5, "v99.0.0", false, true)
            ]),
        )]);
        let selected = release_page(check_page_fixture(&fixture, Channel::Stable).await);
        fixture.finish();
        assert_eq!(
            selected,
            ReleaseInfo {
                version: "1.10.0+build.7".into(),
                tag: "1.10.0+build.7".into(),
                prerelease: false,
                notes: "Selected release notes\nWith details.".into(),
                html_url: "https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/1.10.0+build.7"
                    .into(),
            }
        );
    });
}

#[test]
fn release_page_channels_honor_github_and_semver_prerelease_signals() {
    run(async {
        let flagged = json!([
            release(2, "v2.0.0", false, false),
            release(4, "v4.0.0-rc.1", false, false),
            release(5, "v5.0.0", true, false)
        ]);
        let suffixes = json!([
            release(2, "v2.0.0", false, false),
            release(3, "v3.0.0-rc.2", false, false),
            release(4, "v3.0.0-rc.10", false, false)
        ]);
        let stable_after_prerelease = json!([
            release(3, "v3.0.0-rc.10", false, false),
            release(4, "v3.0.0", false, false)
        ]);
        for (channel, releases, version, prerelease) in [
            (Channel::Stable, flagged.clone(), "2.0.0", false),
            (Channel::Preview, flagged, "5.0.0", true),
            (Channel::Stable, suffixes.clone(), "2.0.0", false),
            (Channel::Preview, suffixes, "3.0.0-rc.10", true),
            (Channel::Preview, stable_after_prerelease, "3.0.0", false),
        ] {
            let fixture = Fixture::new(vec![Reply::json(releases_path(1), releases)]);
            let selected = release_page(check_page_fixture(&fixture, channel).await);
            fixture.finish();
            assert_eq!(selected.version, version, "channel {channel:?}");
            assert_eq!(selected.tag, format!("v{version}"), "channel {channel:?}");
            assert_eq!(selected.prerelease, prerelease, "channel {channel:?}");
            assert_eq!(
                selected.html_url,
                format!("https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v{version}"),
                "channel {channel:?}"
            );
        }
    });
}

#[test]
fn release_page_reports_equal_and_older_versions_as_up_to_date() {
    run(async {
        for channel in [Channel::Stable, Channel::Preview] {
            for tag in ["v1.0.0", "v1.0.0+build.7", "v0.99.0"] {
                let fixture = Fixture::new(vec![Reply::json(
                    releases_path(1),
                    json!([release(1, tag, false, false)]),
                )]);
                let result = check_page_fixture(&fixture, channel).await;
                fixture.finish();
                assert_eq!(
                    result.expect("release page current-version check"),
                    CheckResult::UpToDate,
                    "channel {channel:?}, tag {tag}"
                );
            }
        }

        let fixture = Fixture::new(vec![Reply::json(
            releases_path(1),
            json!([release(2, "v1.0.0-rc.1", false, false)]),
        )]);
        let result = check_page_fixture(&fixture, Channel::Preview).await;
        fixture.finish();
        assert_eq!(
            result.expect("older prerelease check"),
            CheckResult::UpToDate
        );
    });
}

#[test]
fn release_page_reports_empty_and_ineligible_histories() {
    run(async {
        for channel in [Channel::Stable, Channel::Preview] {
            for releases in [
                json!([]),
                json!([
                    release(1, "v99.0.0", false, true),
                    release(2, "v100.0", false, false),
                    release(0, "v200.0.0", false, false)
                ]),
            ] {
                let fixture = Fixture::new(vec![Reply::json(releases_path(1), releases)]);
                let result = check_page_fixture(&fixture, channel).await;
                fixture.finish();
                assert_eq!(
                    result.expect("release page ineligible-history check"),
                    CheckResult::NoEligibleRelease,
                    "channel {channel:?}"
                );
            }
        }

        let fixture = Fixture::new(vec![Reply::json(
            releases_path(1),
            json!([
                release(1, "v2.0.0", true, false),
                release(2, "v3.0.0-rc.1", false, false)
            ]),
        )]);
        let result = check_page_fixture(&fixture, Channel::Stable).await;
        fixture.finish();
        assert_eq!(
            result.expect("stable prerelease-only check"),
            CheckResult::NoEligibleRelease
        );
    });
}

#[test]
fn release_page_searches_all_pages_and_excludes_drafts() {
    run(async {
        let mut first_page: Vec<_> = (0..98)
            .map(|index| release(1000 + index, "v1.0.0", false, false))
            .collect();
        first_page.push(release(2, "v2.0.0", false, false));
        first_page.push(release(30, "v30.0.0", false, true));
        let fixture = Fixture::new(vec![
            Reply::json(releases_path(1), json!(first_page)),
            Reply::json(
                releases_path(2),
                json!([
                    release(3, "v3.0.0", false, false),
                    release(99, "v99.0.0", false, true)
                ]),
            ),
        ]);
        let selected = release_page(check_page_fixture(&fixture, Channel::Stable).await);
        fixture.finish();
        assert_eq!(
            selected,
            ReleaseInfo {
                version: "3.0.0".into(),
                tag: "v3.0.0".into(),
                prerelease: false,
                notes: "fixture release notes".into(),
                html_url: "https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v3.0.0".into(),
            }
        );
    });
}

#[test]
fn release_page_is_not_limited_by_portable_asset_lookups() {
    run(async {
        let releases: Vec<_> = (2..=71)
            .map(|version| release(version, &format!("v{version}.0.0"), false, false))
            .collect();
        let fixture = Fixture::new(vec![Reply::json(releases_path(1), json!(releases))]);
        let selected = release_page(check_page_fixture(&fixture, Channel::Stable).await);
        fixture.finish();
        assert_eq!(selected.version, "71.0.0");
        assert_eq!(selected.tag, "v71.0.0");
        assert!(!selected.prerelease);
    });
}

#[test]
fn stable_and_preview_channels_distinguish_eligible_releases() {
    run(async {
        let releases = json!([
            release(12, "v2.0.0", true, false),
            release(13, "v3.0.0-rc.1", false, false),
            release(14, "v9.0.0", false, true)
        ]);
        let stable = Fixture::new(vec![Reply::json(releases_path(1), releases.clone())]);
        let result = check_fixture(&stable, Channel::Stable).await;
        stable.finish();
        assert_eq!(
            result.expect("stable check"),
            CheckResult::NoEligibleRelease
        );

        let preview = Fixture::new(vec![
            Reply::json(releases_path(1), releases),
            Reply::json(assets_path(13, 1), json!([asset(130, "3.0.0-rc.1")])),
        ]);
        let selected = available(check_fixture(&preview, Channel::Preview).await);
        preview.finish();
        assert_eq!(selected.id, 13);
        assert_eq!(selected.version, "3.0.0-rc.1");
        assert!(selected.prerelease);
        assert_eq!(selected.asset.id, 130);
    });
}

#[test]
fn current_version_and_missing_compatible_asset_have_distinct_results() {
    run(async {
        let current = Fixture::new(vec![Reply::json(
            releases_path(1),
            json!([
                release(1, "v1.0.0", false, false),
                release(2, "v0.9.9", false, false),
                release(3, "v5.0.0-rc.1", true, false)
            ]),
        )]);
        let result = check_fixture(&current, Channel::Stable).await;
        current.finish();
        assert_eq!(
            result.expect("current-version check"),
            CheckResult::UpToDate
        );

        let mut wrong_variant = asset(20, "2.0.0");
        wrong_variant["name"] = json!("slimenrf-helper-2.0.0-windows-x64.exe");
        let incompatible = Fixture::new(vec![
            Reply::json(
                releases_path(1),
                json!([release(2, "v2.0.0", false, false)]),
            ),
            Reply::json(assets_path(2, 1), json!([wrong_variant])),
        ]);
        let result = check_fixture(&incompatible, Channel::Stable).await;
        incompatible.finish();
        assert_eq!(
            result.expect("incompatible-asset check"),
            CheckResult::NoCompatibleRelease
        );
    });
}

#[test]
fn newest_compatible_release_wins_when_newer_variant_does_not_match() {
    run(async {
        let mut wrong_variant = asset(30, "3.0.0");
        wrong_variant["name"] = json!("slimenrf-helper-3.0.0-windows-x64.exe");
        let fixture = Fixture::new(vec![
            Reply::json(
                releases_path(1),
                json!([
                    release(2, "v2.0.0", false, false),
                    release(1, "v1.5.0", false, false),
                    release(3, "v3.0.0", false, false)
                ]),
            ),
            Reply::json(assets_path(3, 1), json!([wrong_variant])),
            Reply::json(assets_path(2, 1), json!([asset(20, "2.0.0")])),
        ]);
        let selected = available(check_fixture(&fixture, Channel::Stable).await);
        fixture.finish();
        assert_eq!(selected.id, 2);
        assert_eq!(selected.version, "2.0.0");
        assert_eq!(selected.tag, "v2.0.0");
        assert!(!selected.prerelease);
        assert_eq!(selected.notes, "fixture release notes");
        assert_eq!(selected.asset.id, 20);
        assert_eq!(
            selected.asset.name,
            "slimenrf-helper-2.0.0-windows-x64-no-firmware.exe"
        );
        assert_eq!(selected.asset.size, 3);
        assert_eq!(selected.asset.sha256, ABC_DIGEST_BYTES);
        assert_eq!(
            selected.asset.url,
            format!("{REPOSITORY_API}/releases/assets/20")
        );
    });
}

#[test]
fn a_full_release_page_is_followed_before_choosing_the_newest_release() {
    run(async {
        let mut first_page: Vec<_> = (0..99)
            .map(|index| release(1000 + index, "v1.0.0", false, false))
            .collect();
        first_page.push(release(2, "v2.0.0", false, false));
        let fixture = Fixture::new(vec![
            Reply::json(releases_path(1), json!(first_page)),
            Reply::json(
                releases_path(2),
                json!([release(3, "v3.0.0", false, false)]),
            ),
            Reply::json(assets_path(3, 1), json!([asset(30, "3.0.0")])),
        ]);
        let selected = available(check_fixture(&fixture, Channel::Stable).await);
        fixture.finish();
        assert_eq!(selected.id, 3);
        assert_eq!(selected.version, "3.0.0");
        assert_eq!(selected.asset.id, 30);
    });
}

#[test]
fn full_release_and_asset_pages_stop_at_the_page_limit() {
    run(async {
        let release_page = json!(vec![release(1, "v1.0.0", false, false); 100]);
        let fixture = Fixture::new(
            (1..=10)
                .map(|page| Reply::json(releases_path(page), release_page.clone()))
                .collect(),
        );
        let result = check_fixture(&fixture, Channel::Stable).await;
        fixture.finish();
        assert!(
            result.is_err(),
            "a full tenth release page must not produce a partial result"
        );

        let mut replies = vec![Reply::json(
            releases_path(1),
            json!([release(2, "v2.0.0", false, false)]),
        )];
        let asset_page = json!(unrelated_assets());
        replies.extend((1..=10).map(|page| Reply::json(assets_path(2, page), asset_page.clone())));
        let fixture = Fixture::new(replies);
        let result = check_fixture(&fixture, Channel::Stable).await;
        fixture.finish();
        assert!(
            result.is_err(),
            "a full tenth asset page must not produce a partial result"
        );
    });
}

#[test]
fn candidate_lookup_limit_fails_instead_of_reporting_an_incomplete_search() {
    run(async {
        let releases: Vec<_> = (2..=66)
            .map(|version| release(version, &format!("v{version}.0.0"), false, false))
            .collect();
        let mut replies = vec![Reply::json(releases_path(1), json!(releases))];
        replies.extend(
            (3..=66)
                .rev()
                .map(|id| Reply::json(assets_path(id, 1), json!([]))),
        );
        let fixture = Fixture::new(replies);
        let result = check_fixture(&fixture, Channel::Stable).await;
        fixture.finish();
        assert!(
            result
                .expect_err("an incomplete candidate search must fail")
                .contains("Too many"),
            "candidate limit must be reported rather than claiming incompatibility"
        );
    });
}

#[test]
fn missing_digest_is_rejected_during_check() {
    run(async {
        for omit in [false, true] {
            let mut missing = asset(20, "2.0.0");
            if omit {
                missing
                    .as_object_mut()
                    .expect("asset fixture is an object")
                    .remove("digest");
            } else {
                missing["digest"] = Value::Null;
            }
            let fixture = Fixture::new(vec![
                Reply::json(
                    releases_path(1),
                    json!([release(2, "v2.0.0", false, false)]),
                ),
                Reply::json(assets_path(2, 1), json!([missing])),
            ]);
            let result = check_fixture(&fixture, Channel::Stable).await;
            fixture.finish();
            let error = result.expect_err("matching asset without a digest must be rejected");
            assert!(error.to_ascii_lowercase().contains("digest"), "{error}");
        }
    });
}

#[test]
fn revalidation_rejects_changed_release_and_asset_before_binary_request() {
    run(async {
        let cases = [
            ("release id", true, "id", json!(8), "release changed"),
            (
                "release tag",
                true,
                "tag_name",
                json!("v2.1.0"),
                "release changed",
            ),
            (
                "release prerelease",
                true,
                "prerelease",
                json!(true),
                "release changed",
            ),
            (
                "release draft",
                true,
                "draft",
                json!(true),
                "no longer published",
            ),
            ("asset id", false, "id", json!(72), "asset changed"),
            (
                "asset name",
                false,
                "name",
                json!("slimenrf-helper-2.0.0-windows-x64.exe"),
                "asset is no longer available",
            ),
            ("asset size", false, "size", json!(4), "asset changed"),
            (
                "asset digest",
                false,
                "digest",
                json!(format!("sha256:{}", "0".repeat(64))),
                "asset changed",
            ),
            (
                "asset state",
                false,
                "state",
                json!("pending"),
                "not fully uploaded",
            ),
            (
                "missing asset digest",
                false,
                "digest",
                Value::Null,
                "digest",
            ),
        ];
        for (case, is_release, field, value, expected_error) in cases {
            let original_release = release(7, "v2.0.0", false, false);
            let original_asset = asset(71, "2.0.0");
            let mut refreshed_release = original_release.clone();
            let mut refreshed_asset = original_asset.clone();
            if is_release {
                refreshed_release[field] = value;
            } else {
                refreshed_asset[field] = value;
            }
            let fixture = Fixture::new(vec![
                Reply::json(releases_path(1), json!([original_release])),
                Reply::json(assets_path(7, 1), json!([original_asset])),
                Reply::json(release_path(7), refreshed_release),
                Reply::json(assets_path(7, 1), json!([refreshed_asset])),
            ]);
            let selected = available(check_fixture(&fixture, Channel::Stable).await);
            let result = download_with(
                &fixture.network,
                selected,
                target(),
                Arc::new(AtomicBool::new(false)),
                |_, _| panic!("{case} must fail before any binary bytes"),
            )
            .await;
            fixture.finish();
            let error = match result {
                Ok(_) => panic!("{case} was accepted after revalidation"),
                Err(error) => error,
            };
            assert!(
                error.to_ascii_lowercase().contains(expected_error),
                "unexpected {case} error: {error}"
            );
        }
    });
}

#[test]
fn matching_paginated_assets_are_rechecked_before_binary_transfer() {
    run(async {
        let release = release(7, "v2.0.0", false, false);
        let matching_asset = asset(71, "2.0.0");
        let first_page = json!(unrelated_assets());
        let fixture = Fixture::new(vec![
            Reply::json(releases_path(1), json!([release.clone()])),
            Reply::json(assets_path(7, 1), first_page.clone()),
            Reply::json(assets_path(7, 2), json!([matching_asset.clone()])),
            Reply::json(release_path(7), release),
            Reply::json(assets_path(7, 1), first_page),
            Reply::json(assets_path(7, 2), json!([matching_asset])),
            Reply::binary(binary_path(71), b"abc"),
        ]);
        let selected = available(check_fixture(&fixture, Channel::Stable).await);
        let mut progress = Vec::new();
        let result = download_with(
            &fixture.network,
            selected.clone(),
            target(),
            Arc::new(AtomicBool::new(false)),
            |completed, total| progress.push((completed, total)),
        )
        .await;
        fixture.finish();
        assert_eq!(progress.last(), Some(&(3, 3)));

        #[cfg(not(windows))]
        {
            let downloaded = match result {
                Ok(downloaded) => downloaded,
                Err(error) => panic!("verified fixture download failed: {error}"),
            };
            assert_eq!(downloaded.release, selected);
            assert_eq!(
                std::fs::read(downloaded.file.path()).expect("read verified temporary update"),
                b"abc"
            );
        }
        #[cfg(windows)]
        {
            let error = match result {
                Ok(_) => panic!("synthetic bytes must not pass Windows PE validation"),
                Err(error) => error,
            };
            assert!(
                error.contains("DOS header") || error.contains("unsupported update architecture"),
                "unexpected synthetic PE validation error: {error}"
            );
        }
    });
}
