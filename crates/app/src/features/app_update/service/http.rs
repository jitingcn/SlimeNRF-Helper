use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::{Client, Response, StatusCode, Url, header};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::release::{Asset, MAX_ASSET_SIZE};

const MAX_JSON: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const CANCEL_POLL: Duration = Duration::from_millis(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const JSON_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

pub(super) struct Network {
    client: Client,
    // Only tests remap an already validated logical HTTPS URL to a loopback fixture.
    #[cfg(test)]
    test_origin: Option<Url>,
}

impl Network {
    pub(super) fn new(client: Client) -> Self {
        Self {
            client,
            #[cfg(test)]
            test_origin: None,
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(client: Client, origin: Url) -> Self {
        Self {
            client,
            test_origin: Some(origin),
        }
    }

    fn request_url(&self, url: &Url) -> Url {
        #[cfg(test)]
        if let Some(origin) = &self.test_origin {
            let mut local = origin.clone();
            local.set_path(url.path());
            local.set_query(url.query());
            return local;
        }
        url.clone()
    }

    async fn response(
        &self,
        raw: &str,
        download: bool,
        cancel: &AtomicBool,
    ) -> Result<Response, String> {
        let mut url = checked_url(raw)?;
        for redirects in 0..=MAX_REDIRECTS {
            cancelled(cancel)?;
            let mut request = self
                .client
                .get(self.request_url(&url))
                .header(header::ACCEPT_ENCODING, "identity")
                .timeout(if download {
                    DOWNLOAD_TIMEOUT
                } else {
                    JSON_TIMEOUT
                });
            if url.host_str() == Some("api.github.com") {
                request = request.header("X-GitHub-Api-Version", "2022-11-28").header(
                    header::ACCEPT,
                    if download {
                        "application/octet-stream"
                    } else {
                        "application/vnd.github+json"
                    },
                );
            }
            // Never consult GH_TOKEN/GITHUB_TOKEN or attach authorization headers.
            let response = cancellable(
                async { request.send().await.map_err(request_error) },
                cancel,
            )
            .await?;
            if response.status().is_redirection() {
                if redirects == MAX_REDIRECTS {
                    return Err("Too many app update redirects".into());
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("App update redirect has no valid destination")?;
                let next = url
                    .join(location)
                    .map_err(|_| "Invalid app update redirect")?;
                url = checked_url(next.as_str())?;
                continue;
            }
            if response.status() != StatusCode::OK {
                return Err(status_error(response.status()));
            }
            return Ok(response);
        }
        Err("Too many app update redirects".into())
    }

    pub(super) async fn json<T: DeserializeOwned + Send + 'static>(
        &self,
        url: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<T, String> {
        let mut response = self.response(url, false, &cancel).await?;
        let total = response.content_length();
        if total.is_some_and(|size| size > MAX_JSON as u64) {
            return Err("GitHub update response exceeds the safe metadata limit".into());
        }
        let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(1024 * 1024) as usize);
        loop {
            let chunk = cancellable(
                async { response.chunk().await.map_err(request_error) },
                &cancel,
            )
            .await?;
            let Some(chunk) = chunk else { break };
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > MAX_JSON)
            {
                return Err("GitHub update response exceeds the safe metadata limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if total.is_some_and(|size| size != bytes.len() as u64) {
            return Err("GitHub update response was truncated".into());
        }
        cancelled(&cancel)?;
        // JSON parsing is bounded but still stays off the continuously driven I/O thread.
        let parse = tokio::task::spawn_blocking(move || {
            serde_json::from_slice(&bytes).map_err(|_| "Invalid GitHub update metadata".to_owned())
        });
        let parsed = parse
            .await
            .map_err(|_| "App update metadata worker failed".to_owned())?;
        cancelled(&cancel)?;
        parsed
    }

    pub(super) async fn download(
        &self,
        asset: &Asset,
        cancel: Arc<AtomicBool>,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<NamedTempFile, String> {
        if asset.size == 0 || asset.size > MAX_ASSET_SIZE {
            return Err("Release asset has an invalid or excessive size".into());
        }
        let mut response = self.response(&asset.url, true, &cancel).await?;
        if response
            .content_length()
            .is_some_and(|size| size != asset.size)
        {
            return Err("Release asset response has an inconsistent size".into());
        }
        cancelled(&cancel)?;

        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let expected_size = asset.size;
        let expected_digest = asset.sha256;
        let worker_cancel = Arc::clone(&cancel);
        let writer = tokio::task::spawn_blocking(move || {
            write_payload(receiver, expected_size, expected_digest, &worker_cancel)
        });

        let transfer: Result<(), String> = async {
            let mut received = 0u64;
            loop {
                let chunk = cancellable(
                    async { response.chunk().await.map_err(request_error) },
                    &cancel,
                )
                .await?;
                let Some(chunk) = chunk else { break };
                received = received
                    .checked_add(chunk.len() as u64)
                    .ok_or("Release asset exceeds its expected size")?;
                if received > expected_size {
                    return Err("Release asset exceeds its expected size".into());
                }
                // The bounded queue applies backpressure rather than buffering the EXE.
                if !cancellable(async { Ok(sender.send(chunk).await.is_ok()) }, &cancel).await? {
                    return Err("Cannot write temporary app update".into());
                }
                progress(received, expected_size);
            }
            if received != expected_size {
                return Err("Release asset was truncated or has an inconsistent size".into());
            }
            Ok(())
        }
        .await;
        drop(sender);
        // Even on cancellation or a broken response, never detach the file worker.
        let written = writer
            .await
            .map_err(|_| "App update file worker failed".to_owned())?;
        cancelled(&cancel)?;
        transfer?;
        written
    }
}

fn write_payload<B: AsRef<[u8]>>(
    mut receiver: tokio::sync::mpsc::Receiver<B>,
    expected_size: u64,
    expected_digest: [u8; 32],
    cancel: &AtomicBool,
) -> Result<NamedTempFile, String> {
    let mut file = NamedTempFile::new()
        .map_err(|error| format!("Cannot create a temporary app update: {error}"))?;
    let mut digest = Sha256::new();
    let mut written = 0u64;
    while let Some(chunk) = receiver.blocking_recv() {
        cancelled(cancel)?;
        let bytes = chunk.as_ref();
        written = written
            .checked_add(bytes.len() as u64)
            .ok_or("Release asset exceeds its expected size")?;
        if written > expected_size {
            return Err("Release asset exceeds its expected size".into());
        }
        file.as_file_mut()
            .write_all(bytes)
            .map_err(|error| format!("Cannot write temporary app update: {error}"))?;
        digest.update(bytes);
    }
    cancelled(cancel)?;
    if written != expected_size {
        return Err("Release asset was truncated or has an inconsistent size".into());
    }
    let actual: [u8; 32] = digest.finalize().into();
    if actual != expected_digest {
        return Err("Release asset SHA-256 does not match GitHub metadata".into());
    }
    file.as_file_mut()
        .flush()
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| format!("Cannot finish temporary app update: {error}"))?;
    cancelled(cancel)?;
    #[cfg(windows)]
    super::windows::validate_payload(file.path())?;
    cancelled(cancel)?;
    Ok(file)
}

fn checked_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "Invalid app update URL")?;
    let allowed = matches!(
        url.host_str(),
        Some(
            "api.github.com"
                | "github.com"
                | "release-assets.githubusercontent.com"
                | "objects.githubusercontent.com"
                | "github-releases.githubusercontent.com"
        )
    );
    if !allowed
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.fragment().is_some()
    {
        return Err("App update source or redirect is not an approved HTTPS host".into());
    }
    Ok(url)
}

fn status_error(status: StatusCode) -> String {
    match status.as_u16() {
        403 | 429 => "GitHub denied or rate-limited the app update request; try again later".into(),
        404 | 410 => {
            "The selected GitHub release or asset is no longer available; check again".into()
        }
        code => format!("GitHub app update request returned HTTP {code}"),
    }
}

fn request_error(error: reqwest::Error) -> String {
    let category = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "failed during transport"
    };
    format!("App update HTTPS request {category}; check your connection and try again")
}

pub(super) fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("App update cancelled".into())
    } else {
        Ok(())
    }
}

async fn cancellable<T>(
    future: impl Future<Output = Result<T, String>>,
    cancel: &AtomicBool,
) -> Result<T, String> {
    let mut future = std::pin::pin!(future);
    loop {
        cancelled(cancel)?;
        if let Ok(result) = tokio::time::timeout(CANCEL_POLL, future.as_mut()).await {
            cancelled(cancel)?;
            return result;
        }
    }
}
