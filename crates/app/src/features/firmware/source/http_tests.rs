use super::super::tests::run;
use super::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{atomic::AtomicUsize, mpsc},
    thread,
    time::Instant,
};

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .user_agent("SmolHelper-firmware/1")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn server(serve: impl FnOnce(TcpStream) + Send + 'static) -> (Url, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    listener.set_nonblocking(true).unwrap();
    let join = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "loopback request never arrived");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        };
        // Winsock inherits the listener's nonblocking mode on accept; these
        // synchronous fixture reads rely on explicit blocking mode and deadlines.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        serve(stream);
    });
    (origin, join)
}

fn request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16 * 1024);
    }
    String::from_utf8(bytes).unwrap().to_ascii_lowercase()
}

async fn joined(join: thread::JoinHandle<()>) {
    tokio::task::spawn_blocking(move || join.join().unwrap())
        .await
        .unwrap();
}

#[test]
fn pending_headers_and_pending_stream_are_cancellable() {
    for streaming in [false, true] {
        run(async {
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let (release, held) = mpsc::channel();
            let (origin, join) = server(move |mut stream| {
                request(&mut stream);
                if streaming {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n",
                        )
                        .unwrap();
                }
                ready.send(()).unwrap();
                // No response headers / no next body chunk until the fetch returns.
                let _ = held.recv_timeout(Duration::from_secs(3));
            });
            let network = Network::for_test(client(), origin, None);
            let cancel = Arc::new(AtomicBool::new(false));
            let flag = cancel.clone();
            let cancelling = tokio::spawn(async move {
                waiting.await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
                flag.store(true, Ordering::Relaxed);
            });
            let mut received = 0;
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                network.fetch("https://github.com/pending", 64, &cancel, &mut |_| {
                    received += 1
                }),
            )
            .await
            .expect("cancellation must not wait for the HTTP timeout");
            release.send(()).unwrap();
            cancelling.await.unwrap();
            joined(join).await;
            assert!(result.unwrap_err().contains("cancelled"));
            if streaming {
                assert!(received > 0, "fixture must enter response streaming");
            }
        });
    }
}

#[test]
fn cloned_shared_client_reuses_connection_after_owners_drop() {
    run(async {
        let accepted = Arc::new(AtomicUsize::new(0));
        let observed = accepted.clone();
        let (origin, join) = server(move |mut stream| {
            observed.fetch_add(1, Ordering::Relaxed);
            for path in ["/first", "/second", "/third"] {
                assert!(request(&mut stream).starts_with(&format!("get {path} ")));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
                    .unwrap();
            }
        });
        let shared = client();
        let first = Network::for_test(shared.clone(), origin.clone(), None);
        let second = Network::for_test(shared.clone(), origin.clone(), None);
        drop(shared);
        let cancel = AtomicBool::new(false);
        assert_eq!(
            first
                .fetch("https://github.com/first", 3, &cancel, &mut |_| {})
                .await
                .unwrap(),
            b"abc"
        );
        drop(first);
        assert_eq!(
            second
                .fetch("https://github.com/second", 3, &cancel, &mut |_| {})
                .await
                .unwrap(),
            b"abc"
        );
        let third = Network::for_test(second.client.clone(), origin, None);
        drop(second);
        assert_eq!(
            third
                .fetch("https://github.com/third", 3, &cancel, &mut |_| {})
                .await
                .unwrap(),
            b"abc"
        );
        joined(join).await;
        assert_eq!(accepted.load(Ordering::Relaxed), 1);
    });
}

#[test]
fn streamed_and_declared_lengths_enforce_bounds_before_publication() {
    for response in [
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n"
            .as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabc".as_slice(),
    ] {
        let response = response.to_vec();
        run(async move {
            let (origin, join) = server(move |mut stream| {
                request(&mut stream);
                stream.write_all(&response).unwrap();
            });
            let network = Network::for_test(client(), origin, None);
            let result = network
                .fetch(
                    "https://github.com/bounded",
                    4,
                    &AtomicBool::new(false),
                    &mut |_| {},
                )
                .await;
            assert!(result.is_err());
            joined(join).await;
        });
    }
}

#[test]
fn redirects_validate_every_hop_and_scope_bearer_to_api_only() {
    run(async {
        let (origin, join) = server(move |mut stream| {
            let api = request(&mut stream);
            assert!(api.contains("authorization: bearer fixture-secret\r\n"));
            assert!(api.contains("x-github-api-version: 2022-11-28\r\n"));
            stream.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: https://nightly.link/public\r\n\r\n").unwrap();
            let public = request(&mut stream);
            assert!(public.starts_with("get /public "));
            assert!(!public.contains("authorization:"));
            assert!(!public.contains("x-github-api-version:"));
            stream.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: https://release-assets.githubusercontent.com/blob?sig=fixture-signature\r\n\r\n").unwrap();
            let blob = request(&mut stream);
            assert!(blob.starts_with("get /blob?sig=fixture-signature "));
            assert!(!blob.contains("authorization:"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
                .unwrap();
        });
        let network = Network::for_test(client(), origin, Some("fixture-secret".into()));
        assert_eq!(
            network
                .fetch(
                    "https://api.github.com/start",
                    4,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .await
                .unwrap(),
            b"abc"
        );
        joined(join).await;
    });
    for target in [
        "http://github.com/insecure",
        "https://github.com.evil.test/unapproved",
        "https://user:password@api.github.com/credentials",
        "https://github.com:444/port",
        "http://127.0.0.1/private",
    ] {
        let response =
            format!("HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: {target}\r\n\r\n");
        run(async move {
            let (origin, join) = server(move |mut stream| {
                request(&mut stream);
                stream.write_all(response.as_bytes()).unwrap();
            });
            let network = Network::for_test(client(), origin, Some("fixture-secret".into()));
            let error = network
                .fetch(
                    "https://api.github.com/redirect",
                    4,
                    &AtomicBool::new(false),
                    &mut |_| {},
                )
                .await
                .unwrap_err();
            assert!(error.contains("approved HTTPS"));
            assert!(!error.contains("password") && !error.contains("fixture-secret"));
            joined(join).await;
        });
    }
}

#[test]
fn redirect_chain_is_bounded() {
    run(async {
        let (origin, join) = server(move |mut stream| {
            for _ in 0..6 {
                request(&mut stream);
                stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: /again\r\n\r\n",
                    )
                    .unwrap();
            }
        });
        let network = Network::for_test(client(), origin, None);
        assert!(
            network
                .fetch(
                    "https://github.com/again",
                    4,
                    &AtomicBool::new(false),
                    &mut |_| {}
                )
                .await
                .unwrap_err()
                .contains("Too many")
        );
        joined(join).await;
    });
}

#[test]
fn cancellation_joins_blocking_work_and_preserves_send_futures() {
    run(async {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let observed = finished.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let cancelling = tokio::spawn(async move {
            waiting.await.unwrap();
            flag.store(true, Ordering::Relaxed);
        });
        let runtime_thread = thread::current().id();
        fn send<T: Send>(value: T) -> T {
            value
        }
        let result: Result<(), String> = send(blocking_work(
            &cancel,
            &mut |_| {},
            move |flag, progress| {
                assert_ne!(thread::current().id(), runtime_thread);
                started.send(()).unwrap();
                while !flag.load(Ordering::Relaxed) {
                    progress(Progress::bytes("Parsing fixture", 1, 1));
                    thread::sleep(Duration::from_millis(1));
                }
                observed.store(true, Ordering::Relaxed);
                Ok(())
            },
        ))
        .await;
        cancelling.await.unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(
            finished.load(Ordering::Relaxed),
            "cancellation must join, never detach"
        );
    });
}

#[test]
fn loopback_catalog_download_pipeline_and_public_futures_are_send() {
    run(async {
        fn send<T: Send>(value: T) -> T {
            value
        }
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let asset = serde_json::json!({
            "id": 7, "name": "receiver.hex", "size": 3,
            "digest": format!("sha256:{digest}"),
            "browser_download_url": "https://github.com/receiver.hex"
        });
        let releases = serde_json::to_vec(&serde_json::json!([{
            "id": 42, "tag_name": "261019", "published_at": "2026-10-19",
            "html_url": "https://github.com/release/42", "assets": [asset.clone()]
        }]))
        .unwrap();
        let assets = serde_json::to_vec(&serde_json::json!([asset])).unwrap();
        let (origin, join) = server(move |mut stream| {
            for (path, body) in [
                ("/repos/jitingcn/SlimeVR-Tracker-nRF/releases?", releases),
                (
                    "/repos/jitingcn/SlimeVR-Tracker-nRF/releases/42/assets?",
                    assets,
                ),
                ("/receiver.hex ", b"abc".to_vec()),
            ] {
                assert!(
                    request(&mut stream).starts_with(&format!("get {}", path.to_ascii_lowercase()))
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let shared = client();
        let network = Network::for_test(shared.clone(), origin, None);
        let config = crate::features::firmware::remote_config::RemoteConfig::default();
        let cancel = AtomicBool::new(false);
        let versions = send(super::super::catalog::list_versions_with(
            &config,
            &network,
            super::super::SourceChannel::Release,
            &cancel,
            &mut |_| {},
        ))
        .await
        .unwrap();
        let artifacts = send(super::super::catalog::list_artifacts_with(
            &config,
            &network,
            &versions[0],
            &cancel,
            &mut |_| {},
        ))
        .await
        .unwrap();
        let files = send(super::super::download::download_with(
            &config,
            &network,
            &artifacts[0],
            &cancel,
            &mut |_| {},
        ))
        .await
        .unwrap();
        assert_eq!(files[0].bytes, b"abc");
        assert_eq!(files[0].sha256, digest);
        assert!(files[0].provenance.contains("matches upstream digest"));
        joined(join).await;

        // Compile-time Send checks on every public API; pre-cancellation guarantees
        // these checks never contact external upstreams.
        cancel.store(true, Ordering::Relaxed);
        assert!(
            send(super::super::list_versions(
                &shared,
                &config,
                super::super::SourceChannel::Release,
                &cancel,
                &mut |_| {},
            ))
            .await
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(
            send(super::super::latest(
                &shared,
                &config,
                super::super::SourceChannel::Release,
                &cancel,
                &mut |_| {},
            ))
            .await
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(
            send(super::super::list_artifacts(
                &shared,
                &config,
                &versions[0],
                &cancel,
                &mut |_| {},
            ))
            .await
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(
            send(super::super::download(
                &shared,
                &config,
                &artifacts[0],
                &cancel,
                &mut |_| {},
            ))
            .await
            .unwrap_err()
            .contains("cancelled")
        );
    });
}
