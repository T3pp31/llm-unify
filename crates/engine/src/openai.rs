//! OpenAI wire execution shared by native HTTP and typed Rust callers.
//! Preserve tool history and provider fields rather than projecting into the
//! older upstream Rust response types, which discard several OpenAI fields.
use std::{pin::Pin, sync::Arc, time::Duration};

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio_util::codec::Decoder;

use crate::Gateway;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub openai_api_base: String,
    pub openai_api_key_environment: String,
    pub connect_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub num_retries: u64,
    pub retry_delay_seconds: f64,
    pub max_retry_delay_seconds: u64,
    pub stream_idle_timeout_seconds: u64,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_sse_event_bytes: usize,
}

impl Settings {
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let settings: Self = serde_json::from_str(json).map_err(|_| Error::Configuration)?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), Error> {
        let settings = self;
        if settings.schema_version != 1
            || settings.connect_timeout_seconds == 0
            || settings.request_timeout_seconds == 0
            || settings.stream_idle_timeout_seconds == 0
            || settings.max_request_bytes == 0
            || settings.max_response_bytes == 0
            || settings.max_sse_event_bytes == 0
            || settings.openai_api_key_environment.is_empty()
            || settings.openai_api_base.is_empty()
            || !settings.retry_delay_seconds.is_finite()
            || settings.retry_delay_seconds < 0.0
            || Duration::try_from_secs_f64(settings.retry_delay_seconds).is_err()
        {
            return Err(Error::Configuration);
        }
        for seconds in [
            settings.connect_timeout_seconds,
            settings.request_timeout_seconds,
            settings.stream_idle_timeout_seconds,
            settings.max_retry_delay_seconds,
        ] {
            if std::time::Instant::now()
                .checked_add(Duration::from_secs(seconds))
                .is_none()
            {
                return Err(Error::Configuration);
            }
        }
        Ok(())
    }

    pub fn bundled() -> Result<Self, Error> {
        Self::from_json(include_str!("../../../config/parity/native-defaults.json"))
    }
}

#[derive(thiserror::Error)]
pub enum Error {
    #[error("invalid native execution settings")]
    Configuration,
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("setting is not supported by the native OpenAI path: {0}")]
    Unsupported(String),
    #[error("upstream credential is unavailable")]
    MissingCredential,
    #[error("upstream connection failed")]
    Transport,
    #[error("upstream request deadline exceeded")]
    Timeout,
    #[error("upstream response exceeds the configured limit")]
    ResponseTooLarge,
    #[error("upstream returned an invalid response")]
    InvalidResponse,
    #[error("upstream request failed (HTTP {status})")]
    Upstream { status: u16, body: Value },
}

impl std::fmt::Debug for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

pub type ChatStream = Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, Error>> + Send>>;

#[derive(Clone)]
pub struct OpenAi {
    gateway: Arc<Gateway>,
    client: reqwest::Client,
    settings: Settings,
}

impl OpenAi {
    pub fn new(gateway: Arc<Gateway>, settings: Settings) -> Result<Self, Error> {
        settings.validate()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(settings.connect_timeout_seconds))
            .timeout(Duration::from_secs(settings.request_timeout_seconds))
            .build()
            .map_err(|_| Error::Configuration)?;
        Ok(Self {
            gateway,
            client,
            settings,
        })
    }

    /// Whether this adapter owns the deployment's OpenAI Chat wire protocol.
    pub fn handles(&self, name: &str) -> bool {
        self.gateway.models.get(name).is_some_and(|deployment| {
            deployment.custom_llm_provider.as_deref().map_or_else(
                || deployment.model.starts_with("openai/"),
                |provider| provider == "openai",
            )
        })
    }

    async fn resolve(&self, configured: &str) -> Result<String, Error> {
        match configured.strip_prefix("os.environ/") {
            Some(name) => self
                .gateway
                .secrets
                .get_secret_str(name)
                .await
                .map_err(|_| Error::MissingCredential)?
                .map(|value| value.expose().to_owned())
                .ok_or(Error::MissingCredential),
            None => Ok(configured.to_owned()),
        }
    }

    /// Keep the raw result available for streaming without buffering it.
    pub async fn send_chat(
        &self,
        name: &str,
        mut body: Map<String, Value>,
    ) -> Result<reqwest::Response, Error> {
        if !self.handles(name) {
            return Err(Error::Unsupported("provider".into()));
        }
        let deployment = self
            .gateway
            .models
            .get(name)
            .ok_or(Error::Invalid("unknown model"))?;
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .filter(|messages| !messages.is_empty())
            .ok_or(Error::Invalid("messages must be a nonempty array"))?;
        if messages.iter().any(|message| !message.is_object()) {
            return Err(Error::Invalid("each message must be an object"));
        }
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if let Some(message) = message.as_object_mut()
                    && message.get("role").and_then(Value::as_str) == Some("assistant")
                    && message.contains_key("tool_calls")
                    && message.get("content") == Some(&Value::Null)
                {
                    message.remove("content");
                }
            }
        }
        if body
            .get("stream")
            .is_some_and(|stream| !stream.is_null() && !stream.is_boolean())
        {
            return Err(Error::Invalid("stream must be a boolean"));
        }
        // These are LiteLLM controls, never provider parameters. Reject until active.
        for key in [
            "cache",
            "caching",
            "fallbacks",
            "num_retries",
            "max_retries",
            "metadata",
            "guardrails",
            "callbacks",
            "api_key",
            "api_base",
            "custom_llm_provider",
        ] {
            if body.contains_key(key) {
                return Err(Error::Unsupported(key.into()));
            }
        }
        body.insert(
            "model".into(),
            Value::String(
                deployment
                    .model
                    .strip_prefix("openai/")
                    .unwrap_or(&deployment.model)
                    .into(),
            ),
        );
        let base = self
            .resolve(
                deployment
                    .api_base
                    .as_deref()
                    .unwrap_or(&self.settings.openai_api_base),
            )
            .await?;
        let mut url = reqwest::Url::parse(&base).map_err(|_| Error::Configuration)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Configuration);
        }
        url.set_path(&format!(
            "{}/chat/completions",
            url.path().trim_end_matches('/')
        ));
        let key = match deployment.api_key.as_deref() {
            Some(key) => self.resolve(key).await?,
            None => self
                .gateway
                .secrets
                .get_secret_str(&self.settings.openai_api_key_environment)
                .await
                .map_err(|_| Error::MissingCredential)?
                .map(|key| key.expose().to_owned())
                .ok_or(Error::MissingCredential)?,
        };
        if key.trim().is_empty() {
            return Err(Error::MissingCredential);
        }
        let request = self.client.post(url).bearer_auth(key).json(&body);
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(self.settings.request_timeout_seconds))
            .ok_or(Error::Configuration)?;
        for attempt in 0..=self.settings.num_retries {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let response = request
                .try_clone()
                .ok_or(Error::Configuration)?
                .timeout(remaining)
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        Error::Timeout
                    } else {
                        Error::Transport
                    }
                })?;
            if attempt == self.settings.num_retries
                || !(response.status().as_u16() == 429 || response.status().is_server_error())
            {
                return Ok(response);
            }
            let seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(self.settings.retry_delay_seconds)
                .min(self.settings.max_retry_delay_seconds as f64);
            let delay = Duration::try_from_secs_f64(seconds).map_err(|_| Error::Configuration)?;
            drop(response);
            if delay >= deadline.saturating_duration_since(tokio::time::Instant::now()) {
                return Err(Error::Timeout);
            }
            tokio::time::sleep(delay).await;
        }
        Err(Error::Configuration)
    }

    pub async fn read_json(&self, response: reqwest::Response) -> Result<Value, Error> {
        if response
            .content_length()
            .is_some_and(|length| length > self.settings.max_response_bytes as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let status = response.status();
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|error| {
                if error.is_timeout() {
                    Error::Timeout
                } else {
                    Error::Transport
                }
            })?;
            if bytes.len() > self.settings.max_response_bytes.saturating_sub(data.len()) {
                return Err(Error::ResponseTooLarge);
            }
            data.extend_from_slice(&bytes);
        }
        let body: Value = serde_json::from_slice(&data).map_err(|_| Error::InvalidResponse)?;
        if !status.is_success() {
            return Err(Error::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    pub async fn chat(&self, name: &str, body: Map<String, Value>) -> Result<Value, Error> {
        if body.get("stream").and_then(Value::as_bool) == Some(true) {
            return Err(Error::Unsupported("use send_chat for streaming".into()));
        }
        let response = self.send_chat(name, body).await?;
        let mut body = self
            .read_json(response)
            .await
            .map_err(|error| normalize_error(error, name))?;
        normalize_chat_response(&mut body, name)?;
        Ok(body)
    }

    pub async fn chat_stream(
        &self,
        name: &str,
        mut body: Map<String, Value>,
    ) -> Result<ChatStream, Error> {
        body.insert("stream".into(), Value::Bool(true));
        let response = self.send_chat(name, body).await?;
        if !response.status().is_success() {
            self.read_json(response)
                .await
                .map_err(|error| normalize_error(error, name))?;
            return Err(Error::InvalidResponse);
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err(Error::InvalidResponse);
        }
        let settings = self.settings.clone();
        let name = name.to_owned();
        Ok(decode_stream(response.bytes_stream(), name, settings))
    }
}

// Pinned references: exceptions.py::RateLimitError and
// router_utils/common_utils.py::fallback_failure_message (no fallback configured).
fn normalize_error(error: Error, name: &str) -> Error {
    match error {
        Error::Upstream { status: 429, body } => {
            let message = body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("upstream rate limit");
            Error::Upstream {
                status: 429,
                body: serde_json::json!({"error": {
                    "message": format!("litellm.RateLimitError: RateLimitError: OpenAIException - {message}\n\nLiteLLM: model group '{name}' failed with the error above. No fallback was attempted."),
                    "type": "throttling_error", "param": null, "code": "429"
                }}),
            }
        }
        error => error,
    }
}

fn decode_stream(
    input: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    name: String,
    settings: Settings,
) -> ChatStream {
    Box::pin(async_stream::try_stream! {
        let mut input = Box::pin(input);
        let mut codec = litellm_framer::sse::SseCodec::default();
        let mut pending = BytesMut::new();
        let mut received = 0usize;
        let mut finished = false;
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(settings.stream_idle_timeout_seconds), input.next()).await.map_err(|_| Error::Timeout)? {
            let chunk = chunk.map_err(|error| if error.is_timeout() { Error::Timeout } else { Error::Transport })?;
            if chunk.len() > settings.max_response_bytes.saturating_sub(received) {
                Err(Error::ResponseTooLarge)?;
            }
            received += chunk.len();
            let mut remaining = chunk.as_ref();
            while !remaining.is_empty() {
                let space = settings.max_sse_event_bytes.saturating_sub(pending.len());
                if space == 0 { Err(Error::ResponseTooLarge)?; }
                let count = space.min(remaining.len());
                pending.extend_from_slice(&remaining[..count]);
                remaining = &remaining[count..];
                while let Some(event) = codec.decode(&mut pending).map_err(|_| Error::InvalidResponse)? {
                    if event.data.trim() == "[DONE]" {
                        if !finished { Err(Error::InvalidResponse)?; }
                        yield Bytes::from_static(b"data: [DONE]\n\n");
                        return;
                    }
                    let mut body: Value = serde_json::from_str(&event.data).map_err(|_| Error::InvalidResponse)?;
                    let object = body.as_object_mut().ok_or(Error::InvalidResponse)?;
                    if object.contains_key("error") { Err(Error::InvalidResponse)?; }
                    let choices = object.get("choices").and_then(Value::as_array).ok_or(Error::InvalidResponse)?;
                    finished |= choices.iter().any(|choice| choice.get("finish_reason").is_some_and(|reason| !reason.is_null()));
                    normalize_stream_chunk(&mut body, &name)?;
                    yield Bytes::from(format!("data: {body}\n\n"));
                }
            }
        }
        // Transport EOF, even after a finish_reason, does not prove a complete SSE body.
        Err(Error::InvalidResponse)?;
    })
}

fn normalize_stream_chunk(body: &mut Value, name: &str) -> Result<(), Error> {
    let object = body.as_object_mut().ok_or(Error::InvalidResponse)?;
    for choice in object
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .ok_or(Error::InvalidResponse)?
    {
        let choice = choice.as_object_mut().ok_or(Error::InvalidResponse)?;
        for key in ["finish_reason", "logprobs"] {
            if choice.get(key) == Some(&Value::Null) {
                choice.remove(key);
            }
        }
    }
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        let completion = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let details = usage
            .entry("completion_tokens_details")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(details) = details.as_object_mut() {
            let reasoning = details
                .entry("reasoning_tokens")
                .or_insert(Value::from(0))
                .as_u64()
                .unwrap_or(0);
            let audio = details
                .get("audio_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            details.entry("text_tokens").or_insert(Value::from(
                completion.saturating_sub(reasoning).saturating_sub(audio),
            ));
        }
    }
    object.insert("model".into(), Value::String(name.into()));
    Ok(())
}

/// Mirror the proxy's public model alias and the SDK's choice extension container.
pub fn normalize_chat_response(body: &mut Value, name: &str) -> Result<(), Error> {
    let object = body.as_object_mut().ok_or(Error::InvalidResponse)?;
    let choices = object
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .ok_or(Error::InvalidResponse)?;
    if choices.is_empty() {
        return Err(Error::InvalidResponse);
    }
    for choice in choices {
        let choice = choice.as_object_mut().ok_or(Error::InvalidResponse)?;
        if !choice.get("message").is_some_and(Value::is_object) {
            return Err(Error::InvalidResponse);
        }
        choice
            .entry("provider_specific_fields")
            .or_insert_with(|| serde_json::json!({}));
    }
    object.insert("model".into(), Value::String(name.into()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, method, path},
    };

    async fn engine(upstream: &MockServer, settings: Settings) -> OpenAi {
        let config = crate::parse_config(&format!(
            "model_list:\n  - model_name: public\n    litellm_params:\n      model: openai/internal\n      api_base: {}/v1\n      api_key: test-upstream-key\ngeneral_settings:\n  master_key: test-master-key\n", upstream.uri()
        )).unwrap();
        let (gateway, _) = crate::build(&config).await.unwrap();
        OpenAi::new(gateway, settings).unwrap()
    }

    fn request() -> Map<String, Value> {
        json!({"model":"public", "messages":[{"role":"user", "content":"hello"}]})
            .as_object()
            .unwrap()
            .clone()
    }

    fn response() -> Value {
        json!({"id":"chatcmpl-fixture", "object":"chat.completion", "model":"internal", "choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}})
    }

    #[tokio::test]
    async fn tools_and_usage_survive_native_http_execution() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-upstream-key"))
            .and(body_partial_json(json!({"model":"internal"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(response()))
            .expect(1)
            .mount(&upstream)
            .await;
        let engine = engine(&upstream, Settings::bundled().unwrap()).await;
        let mut body = request();
        let tools = json!([{"type":"function", "function":{"name":"weather", "parameters":{"type":"object"}}}]);
        body.insert("tools".into(), tools.clone());
        let response = engine.chat("public", body).await.unwrap();
        assert_eq!(response["model"], "public");
        assert_eq!(response["usage"]["total_tokens"], 3);
        let sent = upstream.received_requests().await.unwrap();
        let sent: Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(sent["tools"], tools);
    }

    #[tokio::test]
    async fn upstream_429_retries_are_bounded_and_return_the_proxy_error_contract() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_json(json!({"error":{"message":"slow"}})),
            )
            .expect(3)
            .mount(&upstream)
            .await;
        let engine = engine(&upstream, Settings::bundled().unwrap()).await;
        match engine.chat("public", request()).await.unwrap_err() {
            Error::Upstream { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body["error"]["type"], "throttling_error");
                assert_eq!(body["error"]["code"], "429");
            }
            error => panic!("unexpected error: {error}"),
        }
    }

    #[tokio::test]
    async fn unsupported_controls_are_rejected_before_upstream() {
        let upstream = MockServer::start().await;
        let engine = engine(&upstream, Settings::bundled().unwrap()).await;
        for key in ["cache", "fallbacks", "metadata", "api_key", "guardrails"] {
            let mut body = request();
            body.insert(key.into(), json!(true));
            assert!(matches!(
                engine.chat("public", body).await,
                Err(Error::Unsupported(_))
            ));
        }
        assert!(upstream.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversized_and_invalid_json_responses_are_not_successes() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response()))
            .mount(&upstream)
            .await;
        let mut settings = Settings::bundled().unwrap();
        settings.max_response_bytes = 16;
        assert!(matches!(
            engine(&upstream, settings)
                .await
                .chat("public", request())
                .await,
            Err(Error::ResponseTooLarge)
        ));
        upstream.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&upstream)
            .await;
        assert!(matches!(
            engine(&upstream, Settings::bundled().unwrap())
                .await
                .chat("public", request())
                .await,
            Err(Error::InvalidResponse)
        ));
    }

    #[tokio::test]
    async fn redirects_do_not_send_a_second_request() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/redirect", upstream.uri()))
                    .set_body_json(json!({"error":"redirect"})),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        let error = engine(&upstream, Settings::bundled().unwrap())
            .await
            .chat("public", request())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Upstream { status: 302, .. }));
    }

    fn wire() -> String {
        let event = json!({"id":"stream-fixture","model":"internal","choices":[{"index":0,"delta":{"content":"こんにちは"},"finish_reason":"stop"}]});
        format!("data: {event}\r\n\r\ndata: [DONE]\r\n\r\n")
    }

    #[tokio::test]
    async fn streaming_preserves_utf8_across_every_chunk_boundary() {
        let wire = wire();
        for split in 1..wire.len() {
            let chunks = vec![
                Ok(Bytes::copy_from_slice(&wire.as_bytes()[..split])),
                Ok(Bytes::copy_from_slice(&wire.as_bytes()[split..])),
            ];
            let output = decode_stream(
                futures_util::stream::iter(chunks),
                "public".into(),
                Settings::bundled().unwrap(),
            )
            .collect::<Vec<_>>()
            .await;
            assert_eq!(output.len(), 2, "split={split}");
            let first = std::str::from_utf8(output[0].as_ref().unwrap()).unwrap();
            assert!(first.contains("こんにちは"));
            assert!(first.contains("public"));
            assert_eq!(output[1].as_ref().unwrap().as_ref(), b"data: [DONE]\n\n");
        }
    }

    #[tokio::test]
    async fn truncated_or_invalid_streams_never_emit_successful_done() {
        for wire in [
            "data: [DONE]\n\n".to_owned(),
            "data: bad-json\n\n".to_owned(),
            wire().replace("data: [DONE]\r\n\r\n", ""),
            "data: {".to_owned(),
        ] {
            let output = decode_stream(
                futures_util::stream::iter(vec![Ok(Bytes::from(wire))]),
                "public".into(),
                Settings::bundled().unwrap(),
            )
            .collect::<Vec<_>>()
            .await;
            assert!(output.last().unwrap().is_err());
            assert!(
                !output
                    .iter()
                    .filter_map(|value| value.as_ref().ok())
                    .any(|bytes| bytes.as_ref() == b"data: [DONE]\n\n")
            );
        }
    }

    #[tokio::test]
    async fn unterminated_sse_is_bounded_by_configuration() {
        let mut settings = Settings::bundled().unwrap();
        settings.max_sse_event_bytes = 32;
        let output = decode_stream(
            futures_util::stream::iter(vec![Ok(Bytes::from(
                "data: ".to_owned() + &"x".repeat(64),
            ))]),
            "public".into(),
            settings,
        )
        .collect::<Vec<_>>()
        .await;
        assert!(matches!(
            output.last().unwrap(),
            Err(Error::ResponseTooLarge)
        ));
    }

    #[test]
    fn debugging_upstream_errors_does_not_expose_the_response_body() {
        let error = Error::Upstream {
            status: 400,
            body: json!({"sensitive":"prompt-content"}),
        };
        assert!(!format!("{error:?}").contains("prompt-content"));
    }
}
