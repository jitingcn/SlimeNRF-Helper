use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::{Client, Url, header};
use serde::de::DeserializeOwned;

use super::{Progress, cancelled};

const MAX_JSON: usize = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

pub(super) trait Fetch: Sync {
    fn fetch(
        &self,
        url: &str,
        limit: usize,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send;

    fn has_token(&self) -> bool {
        false
    }
}

pub(super) struct Network {
    client: Client,
    token: Option<String>,
    // Tests route approved logical URLs to a local HTTP fixture. URL validation and
    // credential decisions still use the original HTTPS URL, including redirects.
    #[cfg(test)]
    test_origin: Option<Url>,
}

impl Network {
    pub(super) fn new(client: Client) -> Self {
        let token = ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
        Self {
            client,
            token,
            #[cfg(test)]
            test_origin: None,
        }
    }

    #[cfg(test)]
    fn for_test(client: Client, origin: Url, token: Option<String>) -> Self {
        Self {
            client,
            token,
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
}

pub(super) fn checked_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "Invalid firmware source URL".to_owned())?;
    let host = url.host_str().unwrap_or("");
    let allowed = matches!(
        host,
        "api.github.com"
            | "github.com"
            | "nightly.link"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    ) || host.ends_with(".blob.core.windows.net");
    if !allowed
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err("Firmware source or redirect is not an approved HTTPS upstream host".into());
    }
    Ok(url)
}

pub(super) fn status_error(status: u16, remaining: Option<&str>, reset: Option<&str>) -> String {
    match status {
        401 => "GitHub authentication failed; check GH_TOKEN/GITHUB_TOKEN or remove the invalid token".into(),
        403 if remaining == Some("0") => format!(
            "GitHub API rate limit exceeded (reset Unix time: {}). Wait or supply GH_TOKEN/GITHUB_TOKEN",
            reset.unwrap_or("not supplied")
        ),
        403 => "Firmware access denied; the token may lack Actions read permission, or the upstream download link may have expired".into(),
        404 | 410 => "Firmware artifact is unavailable, removed, private, or expired; refresh the version/artifact list".into(),
        429 => "Upstream rate limit exceeded; wait before trying again".into(),
        _ => format!("Firmware upstream returned HTTP {status}"),
    }
}

pub(super) fn safe_error_chain(
    error: &(dyn std::error::Error + 'static),
    token: Option<&str>,
) -> String {
    let mut messages = Vec::new();
    let mut cause = Some(error);
    // Some transport errors carry only a generic outer message. Keep bounded nested
    // causes (DNS, TLS, socket/OS error) without disclosing request URLs or credentials.
    for _ in 0..8 {
        let Some(error) = cause else {
            break;
        };
        let message = error.to_string();
        let message = match token.filter(|token| !token.is_empty()) {
            Some(token) => message.replace(token, "[redacted]"),
            None => message,
        };
        let mut safe = String::new();
        for word in message.split_whitespace().take(128) {
            if !safe.is_empty() {
                safe.push(' ');
            }
            // Remove whole URL/query-bearing words, including detached presigned
            // parameters. No URL path, userinfo, signature, or query is diagnostic.
            let lower = word.to_ascii_lowercase();
            if word.contains("://")
                || word.contains(['?', '='])
                || lower.contains("%3f")
                || lower.contains("%3d")
            {
                safe.push_str("[redacted]");
            } else {
                safe.extend(word.chars().filter(|c| !c.is_control()).take(256));
            }
        }
        if !safe.is_empty() && messages.last() != Some(&safe) {
            messages.push(safe);
        }
        cause = error.source();
    }
    messages.join(": ")
}

fn request_error(error: reqwest::Error, host: &str, token: Option<&str>) -> String {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection"
    } else if error.is_builder() {
        "request configuration"
    } else {
        "transport"
    };
    let error = error.without_url();
    format!(
        "Firmware HTTPS request to {host} failed ({category}): {}",
        safe_error_chain(&error, token)
    )
}

impl Fetch for Network {
    fn has_token(&self) -> bool {
        self.token.is_some()
    }

    async fn fetch(
        &self,
        raw: &str,
        limit: usize,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<Vec<u8>, String> {
        let mut url = checked_url(raw)?;
        for redirects in 0..=5 {
            cancelled(cancel)?;
            let is_api = url.host_str() == Some("api.github.com");
            let mut request = self.client.get(self.request_url(&url));
            if is_api {
                request = request
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .header(header::ACCEPT, "application/vnd.github+json");
                if let Some(token) = &self.token {
                    request = request.bearer_auth(token);
                }
            }
            let mut response = cancellable(
                async {
                    request.send().await.map_err(|e| {
                        request_error(
                            e,
                            url.host_str().unwrap_or("upstream"),
                            self.token.as_deref(),
                        )
                    })
                },
                cancel,
            )
            .await?;
            cancelled(cancel)?;
            if response.status().is_redirection() {
                if redirects == 5 {
                    return Err("Too many firmware download redirects".into());
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("Firmware redirect has no valid Location")?;
                let next = url
                    .join(location)
                    .map_err(|_| "Invalid firmware redirect")?;
                url = checked_url(next.as_str())?;
                continue;
            }
            if !response.status().is_success() {
                return Err(status_error(
                    response.status().as_u16(),
                    response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .and_then(|v| v.to_str().ok()),
                    response
                        .headers()
                        .get("x-ratelimit-reset")
                        .and_then(|v| v.to_str().ok()),
                ));
            }
            let total = response.content_length().unwrap_or(0);
            if total > limit as u64 {
                return Err(format!(
                    "Firmware response exceeds the {} MiB limit",
                    limit / 1024 / 1024
                ));
            }
            let mut bytes = Vec::with_capacity((total as usize).min(limit).min(1024 * 1024));
            loop {
                let chunk = cancellable(
                    async {
                        response.chunk().await.map_err(|e| {
                            request_error(
                                e,
                                url.host_str().unwrap_or("upstream"),
                                self.token.as_deref(),
                            )
                        })
                    },
                    cancel,
                )
                .await?;
                let Some(chunk) = chunk else { break };
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|size| size > limit)
                {
                    return Err("Firmware data exceeds its safe size limit".into());
                }
                bytes.extend_from_slice(&chunk);
                progress(Progress::bytes(
                    "Downloading firmware data",
                    bytes.len() as u64,
                    total,
                ));
            }
            cancelled(cancel)?;
            if total != 0 && bytes.len() as u64 != total {
                return Err("Firmware download is truncated or has an inconsistent size".into());
            }
            return Ok(bytes);
        }
        Err("Too many firmware download redirects".into())
    }
}

pub(super) fn read_bounded(
    reader: &mut dyn Read,
    limit: usize,
    total: u64,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<u8>, String> {
    cancelled(cancel)?;
    if total > limit as u64 {
        return Err("Firmware data exceeds its safe size limit".into());
    }
    let mut bytes = Vec::with_capacity((total as usize).min(1024 * 1024));
    consume_bounded(reader, limit, total, cancel, progress, |chunk| {
        bytes.extend_from_slice(chunk);
    })?;
    Ok(bytes)
}

pub(super) fn consume_bounded(
    reader: &mut dyn Read,
    limit: usize,
    total: u64,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
    mut consume: impl FnMut(&[u8]),
) -> Result<(), String> {
    cancelled(cancel)?;
    if total > limit as u64 {
        return Err("Firmware data exceeds its safe size limit".into());
    }
    let declared = total as usize;
    let mut consumed = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let available = (declared - consumed).saturating_add(1).min(buffer.len());
        let n = reader
            .read(&mut buffer[..available])
            .map_err(|e| format!("Cannot read firmware data: {e}"))?;
        cancelled(cancel)?;
        if n == 0 {
            break;
        }
        if n > declared - consumed {
            return Err("Firmware download is truncated or has an inconsistent size".into());
        }
        consume(&buffer[..n]);
        consumed += n;
        progress(Progress::bytes(
            "Downloading firmware data",
            consumed as u64,
            total,
        ));
    }
    if consumed != declared {
        return Err("Firmware download is truncated or has an inconsistent size".into());
    }
    Ok(())
}

pub(super) async fn json<T: DeserializeOwned + Send + 'static>(
    fetch: &impl Fetch,
    url: &str,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<T, String> {
    let bytes = fetch.fetch(url, MAX_JSON, cancel, progress).await?;
    blocking_work(cancel, progress, move |_, _| {
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid GitHub response: {e}"))
    })
    .await
}

const CANCEL_POLL: Duration = Duration::from_millis(20);

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

/// Blocking CPU/archive work is owned, cancellation-cooperative, and always joined.
/// Never select cancellation against a JoinHandle: dropping it detaches live work.
pub(super) async fn blocking_work<T: Send + 'static>(
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
    work: impl FnOnce(&AtomicBool, &mut (dyn FnMut(Progress) + Send)) -> Result<T, String>
    + Send
    + 'static,
) -> Result<T, String> {
    cancelled(cancel)?;
    let worker_cancel = Arc::new(AtomicBool::new(false));
    let worker_flag = worker_cancel.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
    let mut join = tokio::task::spawn_blocking(move || {
        work(&worker_flag, &mut |value| {
            // Bounded backpressure; the async owner drains until the worker joins.
            let _ = sender.blocking_send(value);
        })
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            worker_cancel.store(true, Ordering::Relaxed);
        }
        while let Ok(value) = receiver.try_recv() {
            progress(value);
        }
        if let Ok(result) = tokio::time::timeout(CANCEL_POLL, &mut join).await {
            while let Ok(value) = receiver.try_recv() {
                progress(value);
            }
            cancelled(cancel)?;
            return result.map_err(|e| format!("Firmware parsing worker failed: {e}"))?;
        }
    }
}
