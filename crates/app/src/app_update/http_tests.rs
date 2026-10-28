use super::*;
use crate::app_update::release::Asset;
use std::{
    fs,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const CHILD_MARKER: &str = "SLIMENRF_APP_UPDATE_HTTP_TEST_CHILD";
const ISOLATED_TEMP: &str = "SLIMENRF_APP_UPDATE_HTTP_TEST_TEMP";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .user_agent("SmolHelper-update-test/1")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap()
}

fn server<T: Send + 'static>(
    serve: impl FnOnce(TcpListener) -> T + Send + 'static,
) -> (reqwest::Url, thread::JoinHandle<T>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin =
        reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    listener.set_nonblocking(true).unwrap();
    (origin, thread::spawn(move || serve(listener)))
}

fn blocking_stream(stream: TcpStream) -> TcpStream {
    // Winsock can inherit nonblocking mode from the listener on accept.
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(SOCKET_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(SOCKET_TIMEOUT)).unwrap();
    stream
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return blocking_stream(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "loopback request never arrived");
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("loopback accept failed: {error}"),
        }
    }
}

fn request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16 * 1024, "oversized fixture request");
    }
    String::from_utf8(bytes).unwrap()
}

fn assert_no_authorization(request: &str) {
    assert!(
        !request
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("authorization:")),
        "update requests must not send authorization headers"
    );
}

fn json_response(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
        )
        .unwrap();
}

async fn joined<T: Send + 'static>(join: thread::JoinHandle<T>) -> T {
    tokio::task::spawn_blocking(move || join.join().unwrap())
        .await
        .unwrap()
}

fn asset(data: &[u8]) -> Asset {
    Asset {
        id: 7,
        name: "slimenrf-helper-fixture.exe".into(),
        url: "https://api.github.com/repos/example/releases/assets/7".into(),
        size: data.len() as u64,
        sha256: Sha256::digest(data).into(),
    }
}

fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn assert_redacted(error: &str) {
    assert!(!error.contains("://"), "error exposed a URL: {error}");
    assert!(
        !error.contains("fixture-secret") && !error.contains("private-fixture-path"),
        "error exposed request details: {error}"
    );
}

// Serve an accidental request as valid JSON so a missing URL check cannot pass merely
// because the local listener closed or a connection attempt timed out.
fn unexpected_requests(listener: &TcpListener, stop: mpsc::Receiver<()>) -> usize {
    let mut count = 0;
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let mut stream = blocking_stream(stream);
                request(&mut stream);
                json_response(&mut stream);
                count += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                match stop.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return count,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                assert!(Instant::now() < deadline, "fixture was not stopped");
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("loopback accept failed: {error}"),
        }
    }
}

fn in_child(test: &str) -> bool {
    std::env::var(CHILD_MARKER).ok().as_deref() == Some(test)
}

fn run_child(test: &str, configure: impl FnOnce(&mut Command)) {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, module)| module);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(format!("{module}::{test}"))
        .arg("--nocapture")
        .env(CHILD_MARKER, test);
    configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("running 1 test"),
        "isolated test {test} failed or did not run:\n{stdout}\n{stderr}"
    );
}

#[test]
fn approved_redirects_preserve_queries_and_relative_locations() {
    run(async {
        let (origin, join) = server(|listener| {
            let mut api = accept(&listener);
            let headers = request(&mut api);
            assert!(headers.starts_with("GET /start?first=one HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            api.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: https://release-assets.githubusercontent.com/blob?sig=FiXture%2Fpart&n=1\r\n\r\n").unwrap();
            drop(api);

            let mut blob = accept(&listener);
            let headers = request(&mut blob);
            assert!(headers.starts_with("GET /blob?sig=FiXture%2Fpart&n=1 HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            blob.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: /relative?next=two%2Fthree\r\n\r\n").unwrap();
            drop(blob);

            let mut final_request = accept(&listener);
            let headers = request(&mut final_request);
            assert!(headers.starts_with("GET /relative?next=two%2Fthree HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            json_response(&mut final_request);
        });
        let network = Network::for_test(client(), origin);
        let value: serde_json::Value = network
            .json("https://api.github.com/start?first=one", flag())
            .await
            .unwrap();
        assert_eq!(value, serde_json::json!({"ok": true}));
        joined(join).await;
    });
}

#[test]
fn every_approved_host_routes_only_to_the_loopback_fixture() {
    run(async {
        let hosts = [
            "api.github.com",
            "github.com",
            "release-assets.githubusercontent.com",
            "objects.githubusercontent.com",
            "github-releases.githubusercontent.com",
        ];
        let (origin, join) = server(move |listener| {
            for index in 0..hosts.len() {
                let mut stream = accept(&listener);
                assert!(request(&mut stream).starts_with(&format!("GET /approved/{index} ")));
                json_response(&mut stream);
            }
        });
        let network = Network::for_test(client(), origin);
        for (index, host) in hosts.iter().enumerate() {
            let url = format!("https://{host}/approved/{index}");
            let result: serde_json::Value = network.json(&url, flag()).await.unwrap();
            assert_eq!(result, serde_json::json!({"ok": true}), "host {host}");
        }
        joined(join).await;
    });
}

#[test]
fn hostile_initial_urls_and_redirects_never_reach_the_listener() {
    let targets = [
        "http://github.com/private-fixture-path?sig=fixture-secret",
        "https://github.com.evil.test/private-fixture-path?sig=fixture-secret",
        "https://api.github.com.evil.test/private-fixture-path?sig=fixture-secret",
        "https://evil.github.com/private-fixture-path?sig=fixture-secret",
        "https://user:fixture-secret@api.github.com/private-fixture-path",
        "https://github.com:444/private-fixture-path?sig=fixture-secret",
        "http://127.0.0.1/private-fixture-path?sig=fixture-secret",
        "https://nightly.link/private-fixture-path?sig=fixture-secret",
        "https://example.blob.core.windows.net/private-fixture-path?sig=fixture-secret",
    ];
    run(async {
        let (stop, stopped) = mpsc::channel();
        let (origin, join) = server(move |listener| unexpected_requests(&listener, stopped));
        let network = Network::for_test(client(), origin);
        for target in targets {
            let error = network
                .json::<serde_json::Value>(target, flag())
                .await
                .unwrap_err();
            assert_redacted(&error);
        }
        stop.send(()).unwrap();
        assert_eq!(
            joined(join).await,
            0,
            "an invalid initial URL reached the listener"
        );

        for target in targets {
            let (stop, stopped) = mpsc::channel();
            let response = format!(
                "HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: {target}\r\n\r\n"
            );
            let (origin, join) = server(move |listener| {
                let mut stream = accept(&listener);
                request(&mut stream);
                stream.write_all(response.as_bytes()).unwrap();
                drop(stream);
                unexpected_requests(&listener, stopped)
            });
            let network = Network::for_test(client(), origin);
            let error = network
                .json::<serde_json::Value>("https://api.github.com/redirect", flag())
                .await
                .unwrap_err();
            stop.send(()).unwrap();
            assert_eq!(
                joined(join).await,
                0,
                "redirect reached forbidden target {target}"
            );
            assert_redacted(&error);
        }
    });
}

#[test]
fn five_redirects_are_allowed_but_six_are_rejected() {
    run(async {
        for accepted in [true, false] {
            let (origin, join) = server(move |listener| {
                for hop in 0..=5 {
                    let mut stream = accept(&listener);
                    assert!(
                        request(&mut stream)
                            .starts_with(&format!("GET /hop?step={hop} HTTP/1.1\r\n"))
                    );
                    if accepted && hop == 5 {
                        json_response(&mut stream);
                    } else {
                        write!(stream, "HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: /hop?step={}\r\n\r\n", hop + 1).unwrap();
                    }
                }
            });
            let network = Network::for_test(client(), origin);
            let result = network
                .json::<serde_json::Value>("https://github.com/hop?step=0", flag())
                .await;
            joined(join).await;
            if accepted {
                assert_eq!(result.unwrap(), serde_json::json!({"ok": true}));
            } else {
                let error = result.unwrap_err();
                assert!(error.to_ascii_lowercase().contains("redirect"));
                assert_redacted(&error);
            }
        }
    });
}

#[test]
fn github_environment_tokens_are_never_authorization_headers() {
    const TEST: &str = "github_environment_tokens_are_never_authorization_headers";
    const GH_SECRET: &str = "fixture-secret-gh-token";
    const GITHUB_SECRET: &str = "fixture-secret-github-token";
    if !in_child(TEST) {
        run_child(TEST, |command| {
            command
                .env("GH_TOKEN", GH_SECRET)
                .env("GITHUB_TOKEN", GITHUB_SECRET);
        });
        return;
    }
    assert_eq!(std::env::var("GH_TOKEN").unwrap(), GH_SECRET);
    assert_eq!(std::env::var("GITHUB_TOKEN").unwrap(), GITHUB_SECRET);
    run(async {
        let (origin, join) = server(|listener| {
            let mut metadata = accept(&listener);
            let headers = request(&mut metadata);
            assert!(headers.starts_with("GET /token-check HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            json_response(&mut metadata);
            drop(metadata);

            let mut api = accept(&listener);
            let headers = request(&mut api);
            assert!(headers.starts_with("GET /repos/example/releases/assets/7 HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("\r\naccept: application/octet-stream\r\n")
            );
            api.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: https://objects.githubusercontent.com/blob?sig=fixture-secret-signature\r\n\r\n").unwrap();
            drop(api);

            let mut blob = accept(&listener);
            let headers = request(&mut blob);
            assert!(headers.starts_with("GET /blob?sig=fixture-secret-signature HTTP/1.1\r\n"));
            assert_no_authorization(&headers);
            blob.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
                .unwrap();
        });
        let network = Network::for_test(client(), origin);
        let value: serde_json::Value = network
            .json("https://api.github.com/token-check", flag())
            .await
            .unwrap();
        assert_eq!(value, serde_json::json!({"ok": true}));
        let mut update = asset(b"abc");
        update.sha256 = [0; 32];
        let error = network
            .download(&update, flag(), |_, _| {})
            .await
            .unwrap_err();
        joined(join).await;
        assert!(error.contains("SHA-256"), "wrong failure: {error}");
        assert_redacted(&error);
        assert!(!error.contains(GH_SECRET) && !error.contains(GITHUB_SECRET));
    });
}

#[test]
fn download_rejects_truncation_oversize_and_wrong_digest() {
    let cases: [(&str, &[u8], bool); 5] = [
        ("truncated HTTP body", b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabc", false),
        ("truncated chunked body", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n", false),
        ("oversized Content-Length", b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n", false),
        ("oversized chunked body", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nabcdef\r\n0\r\n\r\n", false),
        ("wrong digest", b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd", true),
    ];
    run(async {
        for (name, response, wrong_digest) in cases {
            let (origin, join) = server(move |listener| {
                let mut stream = accept(&listener);
                let headers = request(&mut stream);
                assert_no_authorization(&headers);
                stream.write_all(response).unwrap();
            });
            let network = Network::for_test(client(), origin);
            let mut update = asset(b"abcd");
            update.url.push_str("?sig=fixture-secret");
            if wrong_digest {
                update.sha256 = [0; 32];
            }
            let error = network
                .download(&update, flag(), |_, _| {})
                .await
                .unwrap_err();
            joined(join).await;
            if wrong_digest {
                assert!(error.contains("SHA-256"), "{name}: {error}");
            }
            assert_redacted(&error);
        }
    });
}

#[test]
fn invalid_declared_asset_sizes_are_rejected_without_a_request() {
    run(async {
        let (stop, stopped) = mpsc::channel();
        let (origin, join) = server(move |listener| unexpected_requests(&listener, stopped));
        let network = Network::for_test(client(), origin);
        for size in [0, MAX_ASSET_SIZE + 1] {
            let mut update = asset(b"abcd");
            update.size = size;
            assert!(network.download(&update, flag(), |_, _| {}).await.is_err());
        }
        stop.send(()).unwrap();
        assert_eq!(joined(join).await, 0, "invalid asset size made a request");
    });
}

#[cfg(not(windows))]
#[test]
fn valid_digest_download_returns_the_exact_bytes_and_removes_the_tempfile() {
    run(async {
        let (origin, join) = server(|listener| {
            let mut stream = accept(&listener);
            request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
                .unwrap();
        });
        let network = Network::for_test(client(), origin);
        let mut progress = Vec::new();
        let file = network
            .download(&asset(b"abc"), flag(), |received, total| {
                progress.push((received, total))
            })
            .await
            .unwrap();
        joined(join).await;
        assert_eq!(fs::read(file.path()).unwrap(), b"abc");
        assert_eq!(progress.last(), Some(&(3, 3)));
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists(), "temporary download survived its owner");
    });
}

#[test]
fn metadata_status_transport_and_parse_errors_do_not_expose_request_details() {
    let responses: [(&str, &[u8]); 3] = [
        (
            "status",
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        (
            "truncated transport",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabc",
        ),
        (
            "invalid JSON",
            b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nfixture-secret",
        ),
    ];
    run(async {
        for (case, response) in responses {
            let (origin, join) = server(move |listener| {
                let mut stream = accept(&listener);
                let headers = request(&mut stream);
                assert!(
                    headers
                        .starts_with("GET /private-fixture-path?sig=fixture-secret HTTP/1.1\r\n")
                );
                stream.write_all(response).unwrap();
            });
            let network = Network::for_test(client(), origin);
            let error = network
                .json::<serde_json::Value>(
                    "https://api.github.com/private-fixture-path?sig=fixture-secret",
                    flag(),
                )
                .await
                .unwrap_err();
            joined(join).await;
            assert_redacted(&error);
            assert!(!error.contains("127.0.0.1"), "{case}: {error}");
        }
    });
}

#[test]
fn json_enforces_declared_and_streamed_eight_mib_limits() {
    const LIMIT: usize = 8 * 1024 * 1024;
    run(async {
        let (origin, join) = server(|listener| {
            let mut stream = accept(&listener);
            request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                LIMIT + 1
            )
            .unwrap();
        });
        let network = Network::for_test(client(), origin);
        let error = network
            .json::<serde_json::Value>(
                "https://api.github.com/private-fixture-path?sig=fixture-secret",
                flag(),
            )
            .await
            .unwrap_err();
        joined(join).await;
        assert_redacted(&error);

        for size in [LIMIT, LIMIT + 1] {
            let (origin, join) = server(move |listener| {
                let mut stream = accept(&listener);
                request(&mut stream);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let mut body = vec![b'a'; size];
                body[0] = b'"';
                body[size - 1] = b'"';
                let sent = stream.write_all(&body);
                if size == LIMIT {
                    sent.unwrap();
                }
            });
            let network = Network::for_test(client(), origin);
            let result = network
                .json::<String>("https://api.github.com/metadata", flag())
                .await;
            joined(join).await;
            if size == LIMIT {
                assert_eq!(result.unwrap().len(), LIMIT - 2);
            } else {
                assert!(result.is_err(), "oversized streamed JSON was accepted");
            }
        }
    });
}

#[test]
fn pending_download_headers_and_body_cancel_and_join_the_file_worker() {
    const TEST: &str = "pending_download_headers_and_body_cancel_and_join_the_file_worker";
    if !in_child(TEST) {
        let directory = tempfile::tempdir().unwrap();
        run_child(TEST, |command| {
            command
                .env(ISOLATED_TEMP, directory.path())
                .env("TMPDIR", directory.path())
                .env("TMP", directory.path())
                .env("TEMP", directory.path());
        });
        return;
    }
    let directory = std::env::var_os(ISOLATED_TEMP).unwrap();
    let directory = Path::new(&directory).to_owned();
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    run(async {
        for streaming in [false, true] {
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let (started, body_started) = tokio::sync::oneshot::channel();
            let (release, held) = mpsc::channel();
            let (origin, join) = server(move |listener| {
                let mut stream = accept(&listener);
                request(&mut stream);
                if streaming {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n").unwrap();
                }
                let _ = ready.send(());
                let _ = held.recv_timeout(SOCKET_TIMEOUT);
                let mut byte = [0];
                stream.read(&mut byte)
            });
            let network = Network::for_test(client(), origin);
            let cancel = flag();
            let cancelling = {
                let flag = Arc::clone(&cancel);
                let directory = directory.clone();
                tokio::spawn(async move {
                    if waiting.await.is_err() {
                        return false;
                    }
                    let observed_file = if streaming {
                        if body_started.await.is_err() {
                            return false;
                        }
                        tokio::time::timeout(Duration::from_secs(2), async {
                            loop {
                                if fs::read_dir(&directory).unwrap().next().is_some() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                        })
                        .await
                        .is_ok()
                    } else {
                        true
                    };
                    flag.store(true, Ordering::Release);
                    observed_file
                })
            };
            let mut update = asset(b"abc");
            update.size = 64;
            let mut started = Some(started);
            let result = tokio::time::timeout(
                Duration::from_secs(4),
                network.download(&update, Arc::clone(&cancel), move |received, _| {
                    if received > 0
                        && let Some(started) = started.take()
                    {
                        let _ = started.send(());
                    }
                }),
            )
            .await;
            cancel.store(true, Ordering::Release);
            let observed_file = cancelling.await.unwrap();
            let _ = release.send(());
            let closed = joined(join).await;
            assert!(
                observed_file,
                "body transfer did not create its isolated temporary file"
            );
            let error = result
                .expect("cancellation must complete before the HTTP timeout")
                .unwrap_err();
            assert!(error.contains("cancelled"), "unexpected error: {error}");
            assert!(
                matches!(&closed, Ok(0))
                    || matches!(&closed, Err(error) if matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe)),
                "cancelled response left the socket open: {closed:?}"
            );
            assert!(
                fs::read_dir(&directory).unwrap().next().is_none(),
                "cancellation returned before the temporary file worker cleaned up"
            );
        }
    });
}
