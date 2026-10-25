use crate::{Error, ErrorKind, HttpConfig, RetryConfig};
use futures_util::StreamExt;
use std::time::{Duration, SystemTime};

pub(crate) async fn checked(
    response: reqwest::Response,
    retry: &RetryConfig,
) -> Result<reqwest::Response, Error> {
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(response);
    }
    let mut error = Error::new(
        match status {
            401 | 403 => ErrorKind::AuthError,
            429 => ErrorKind::RateLimitError,
            400 | 404 | 422 => ErrorKind::InvalidRequest,
            _ => ErrorKind::ProviderError,
        },
        "provider rejected request",
    );
    error.status = Some(status);
    error.retryable = retry.statuses.contains(&status)
        && !matches!(error.kind, ErrorKind::AuthError | ErrorKind::InvalidRequest);
    error.request_id = request_id(response.headers());
    error.retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(s)
                    .ok()
                    .and_then(|d| d.duration_since(SystemTime::now()).ok())
            })
        });
    Err(error)
}
pub(crate) fn request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "x-amzn-requestid"]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .filter(|s| {
                    s.len() <= 256
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c))
                })
                .map(str::to_owned)
        })
}
pub(crate) async fn json(
    response: reqwest::Response,
    config: &HttpConfig,
) -> Result<serde_json::Value, Error> {
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| Error::transport(true))?;
        if body.len().saturating_add(chunk.len()) > config.max_response_bytes {
            return Err(Error::protocol());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::protocol())
}
/// Bound the bytes retained by eventsource-stream, including unterminated frames.
pub(crate) fn sse_bytes(
    response: reqwest::Response,
    config: HttpConfig,
) -> impl futures_core::Stream<Item = Result<bytes::Bytes, Error>> + Send {
    async_stream::stream! {
        let mut source = response.bytes_stream();
        let mut event_size = 0usize;
        let mut line_size = 0usize;
        let mut after_cr = false;
        loop {
            let next = tokio::time::timeout(
                Duration::from_millis(config.stream_idle_timeout_ms),
                source.next(),
            )
            .await;
            let chunk = match next {
                Ok(Some(Ok(c))) => c,
                Ok(None) => return,
                _ => {
                    yield Err(Error::transport(true));
                    return;
                }
            };
            for &b in &chunk {
                // CRLF is one line ending; LF and CR are valid SSE line endings.
                if b == b'\n' && after_cr {
                    after_cr = false;
                    continue;
                }
                after_cr = b == b'\r';
                event_size = event_size.saturating_add(1);
                if b == b'\r' || b == b'\n' {
                    if line_size == 0 {
                        event_size = 0;
                    }
                    line_size = 0;
                } else {
                    line_size += 1;
                }
                if event_size > config.max_sse_event_bytes {
                    yield Err(Error::protocol());
                    return;
                }
            }
            yield Ok(chunk);
        }
    }
}
