//! OpenAI Chat Completions passthrough with a durable, metadata-only usage ledger.
mod ledger;

pub use ledger::{UsageRecord, UsageSummary};

use crate::{Config, GatewayConfig, Price, TokenUsage};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use ledger::{Filter, Ledger};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct Gateway(Arc<Inner>);

struct Inner {
    config: Config,
    settings: GatewayConfig,
    key_hash: [u8; 32],
    provider_keys: BTreeMap<String, String>,
    http: reqwest::Client,
    ledger: Ledger,
    permits: Arc<Semaphore>,
}

/// Static error messages never contain upstream bodies, credentials, or SQL details.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
        }
    }
    fn storage() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "usage_storage_unavailable",
            message: "usage storage unavailable",
        }
    }
    fn upstream() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "upstream_error",
            message: "upstream response failed or was invalid",
        }
    }
    fn payload(&self) -> Value {
        json!({"error":{"message":self.message,"type":self.code,"code":self.code}})
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for ApiError {}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.payload())).into_response()
    }
}

impl Gateway {
    /// Reads upstream credentials from the environment, never from incoming headers.
    pub async fn new(config: Config, api_key: &str) -> Result<Self, ApiError> {
        config
            .validate()
            .map_err(|_| ApiError::bad_request("invalid gateway configuration"))?;
        let settings = config
            .gateway
            .clone()
            .ok_or_else(|| ApiError::bad_request("gateway configuration is required"))?;
        if api_key.is_empty() || api_key.chars().any(char::is_whitespace) {
            return Err(ApiError::bad_request(
                "gateway API key must be nonempty and contain no whitespace",
            ));
        }
        let provider_keys = config
            .providers
            .iter()
            .filter_map(|(name, provider)| {
                provider
                    .api_key_env
                    .as_ref()
                    .and_then(|key| std::env::var(key).ok())
                    .map(|key| (name.clone(), key))
            })
            .collect();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_millis(config.http.request_timeout_ms))
            .build()
            .map_err(|_| ApiError::bad_request("invalid HTTP configuration"))?;
        let ledger = Ledger::open(&settings).await?;
        let permits = Arc::new(Semaphore::new(settings.max_in_flight));
        Ok(Self(Arc::new(Inner {
            config,
            settings,
            key_hash: Sha256::digest(api_key.as_bytes()).into(),
            provider_keys,
            http,
            ledger,
            permits,
        })))
    }

    pub async fn from_env() -> Result<Self, ApiError> {
        let config = Config::from_env()
            .map_err(|_| ApiError::bad_request("invalid gateway configuration"))?;
        let env = &config
            .gateway
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("gateway configuration is required"))?
            .api_key_env;
        let key = std::env::var(env).map_err(|_| {
            ApiError::bad_request("gateway API key environment variable is missing")
        })?;
        Self::new(config, &key).await
    }

    pub fn listen_address(&self) -> std::net::SocketAddr {
        self.0.settings.listen
    }

    /// All routes, including usage history, require the gateway's bearer token.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/usage/requests", get(history))
            .route("/usage/summary", get(summary))
            .layer(DefaultBodyLimit::max(self.0.settings.max_request_bytes))
            .route_layer(middleware::from_fn_with_state(self.0.clone(), authenticate))
            .with_state(self.0.clone())
    }
}

async fn authenticate(State(state): State<Arc<Inner>>, request: Request, next: Next) -> Response {
    let key = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let valid = key.is_some_and(|key| {
        let hash: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        bool::from(hash.ct_eq(&state.key_hash))
    });
    if !valid {
        return ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "valid gateway bearer token required",
        }
        .into_response();
    }
    next.run(request).await
}

async fn history(
    State(state): State<Arc<Inner>>,
    filter: Result<Query<Filter>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(filter) = filter.map_err(|_| ApiError::bad_request("invalid usage filter"))?;
    let limit = filter.limit(state.settings.history_page_size)?;
    let records = state.ledger.records(filter, limit).await?;
    let next_before = if records.len() == limit {
        records.last().map(|r| r.sequence)
    } else {
        None
    };
    Ok(Json(json!({"data":records,"next_before":next_before})))
}

async fn summary(
    State(state): State<Arc<Inner>>,
    filter: Result<Query<Filter>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(filter) = filter.map_err(|_| ApiError::bad_request("invalid usage filter"))?;
    let limit = filter.limit(state.settings.history_page_size)?;
    if filter.before.is_some() {
        return Err(ApiError::bad_request(
            "before is only supported for request history",
        ));
    }
    // Return an explicit truncation flag rather than silently presenting a partial total.
    let mut rows = state
        .ledger
        .summary(filter, limit.saturating_add(1))
        .await?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok(Json(json!({"data":rows,"truncated":truncated})))
}

async fn chat(
    State(state): State<Arc<Inner>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(mut body) = body.map_err(|error| ApiError {
        status: error.status(),
        code: "invalid_request",
        message: "invalid or oversized JSON request",
    })?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("model must be provider/model"))?
        .to_owned();
    let model_id = crate::ModelId::parse(&model)
        .map_err(|_| ApiError::bad_request("model must be provider/model"))?;
    let (provider_name, upstream_model) = model_id
        .parts()
        .map_err(|_| ApiError::bad_request("invalid model"))?;
    let provider = state
        .config
        .providers
        .get(provider_name)
        .ok_or_else(|| ApiError::bad_request("unknown provider"))?;
    if provider.kind != "openai_compatible"
        && !(provider.kind == "openai" && provider.api.as_deref() == Some("chat_completions"))
    {
        return Err(ApiError::bad_request(
            "gateway requires an OpenAI Chat Completions compatible provider",
        ));
    }
    if body
        .get("messages")
        .and_then(Value::as_array)
        .is_none_or(|m| m.is_empty())
    {
        return Err(ApiError::bad_request("messages must be a nonempty array"));
    }
    let streaming = match body.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(ApiError::bad_request("stream must be a boolean")),
    };
    body["model"] = upstream_model.into();
    if streaming {
        if body
            .get("stream_options")
            .is_some_and(|value| !value.is_object())
        {
            return Err(ApiError::bad_request("stream_options must be an object"));
        }
        if body.get("stream_options").is_none() {
            body["stream_options"] = json!({});
        }
        body["stream_options"]["include_usage"] = true.into();
    }
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("provider base_url is missing"))?;
    let mut upstream = state
        .http
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .json(&body);
    if provider.api_key_env.is_some() {
        let key = state
            .provider_keys
            .get(provider_name)
            .filter(|key| !key.is_empty())
            .ok_or(ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "provider_credentials_missing",
                message: "provider credentials are not configured",
            })?;
        upstream = upstream.bearer_auth(key);
    }
    let permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "gateway_busy",
            message: "gateway concurrency limit reached",
        })?;
    let id = uuid::Uuid::new_v4().to_string();
    let price = state.config.prices.get(&model).cloned();
    state
        .ledger
        .begin(id.clone(), model, provider_name.to_owned(), price.clone())
        .await?;
    let mut record = Record {
        ledger: state.ledger.clone(),
        id,
        price,
        counts: Counts::default(),
        upstream_status: None,
        finished: false,
        _permit: permit,
    };
    let response = match upstream.send().await {
        Ok(response) => response,
        Err(_) => {
            record
                .finish("failed", Some("upstream_transport_error"))
                .await?;
            return Err(ApiError::upstream());
        }
    };
    record.upstream_status = Some(response.status().as_u16());
    if !response.status().is_success() {
        let status = if response.status().is_client_error() || response.status().is_server_error() {
            response.status()
        } else {
            StatusCode::BAD_GATEWAY
        };
        record.finish("failed", Some("upstream_http_error")).await?;
        return Err(ApiError {
            status,
            code: "upstream_http_error",
            message: "upstream rejected request",
        });
    }
    if streaming {
        if response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|s| {
                !s.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("text/event-stream")
            })
        {
            record.finish("failed", Some("invalid_stream")).await?;
            return Err(ApiError::upstream());
        }
        return Ok(stream_response(response, state.config.http.clone(), record));
    }
    let result = crate::transport::json(response, &state.config.http).await;
    let value = match result {
        Ok(value)
            if value.get("choices").is_some_and(Value::is_array)
                && value.get("error").is_none() =>
        {
            value
        }
        _ => {
            record.finish("failed", Some("invalid_response")).await?;
            return Err(ApiError::upstream());
        }
    };
    record.counts = match Counts::read(&value) {
        Ok(counts) => counts,
        Err(error) => {
            record.finish("failed", Some("invalid_usage")).await?;
            return Err(error);
        }
    };
    record.finish("completed", None).await?;
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        "x-request-id",
        record.id.parse().map_err(|_| ApiError::storage())?,
    );
    Ok(response)
}

#[derive(Clone, Copy, Default)]
pub(super) struct Counts {
    input: Option<u64>,
    output: Option<u64>,
    total: Option<u64>,
}
impl Counts {
    fn read(value: &Value) -> Result<Self, ApiError> {
        let usage = match value.get("usage") {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Object(usage)) => usage,
            _ => return Err(ApiError::upstream()),
        };
        let get = |key| match usage.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .filter(|n| *n <= i64::MAX as u64)
                .map(Some)
                .ok_or_else(ApiError::upstream),
        };
        let input = get("prompt_tokens")?;
        let output = get("completion_tokens")?;
        let mut total = get("total_tokens")?;
        if total.is_none()
            && let (Some(input), Some(output)) = (input, output)
        {
            total = Some(
                input
                    .checked_add(output)
                    .filter(|n| *n <= i64::MAX as u64)
                    .ok_or_else(ApiError::upstream)?,
            );
        }
        Ok(Self {
            input,
            output,
            total,
        })
    }
    fn cost(self, price: Option<&Price>) -> Option<f64> {
        let usage = TokenUsage {
            input_tokens: self.input?,
            output_tokens: self.output?,
            total_tokens: self.total,
            ..TokenUsage::default()
        };
        Some(price?.cost(&usage)).filter(|cost| cost.is_finite())
    }
}

struct Record {
    ledger: Ledger,
    id: String,
    price: Option<Price>,
    counts: Counts,
    upstream_status: Option<u16>,
    finished: bool,
    _permit: OwnedSemaphorePermit,
}
impl Record {
    async fn finish(
        &mut self,
        status: &'static str,
        error: Option<&'static str>,
    ) -> Result<(), ApiError> {
        self.ledger
            .finish(
                self.id.clone(),
                status,
                self.upstream_status,
                self.counts,
                self.counts.cost(self.price.as_ref()),
                error,
            )
            .await?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let ledger = self.ledger.clone();
        let id = self.id.clone();
        let counts = self.counts;
        let cost = counts.cost(self.price.as_ref());
        let status = self.upstream_status;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if ledger
                    .finish(
                        id,
                        "cancelled",
                        status,
                        counts,
                        cost,
                        Some("request_cancelled"),
                    )
                    .await
                    .is_err()
                {
                    eprintln!(
                        "gateway: could not finalize cancelled request; pending row retained"
                    );
                }
            });
        }
    }
}

fn stream_response(
    response: reqwest::Response,
    config: crate::HttpConfig,
    record: Record,
) -> Response {
    let id = record.id.clone();
    let stream = async_stream::stream! {
        let mut record = record;
        let events = crate::transport::sse_bytes(response, config.clone()).eventsource();
        futures_util::pin_mut!(events);
        let mut size = 0usize;
        let mut failure = None;
        while let Some(event) = events.next().await {
            let event = match event { Ok(event) => event, Err(_) => { failure = Some("stream_interrupted"); break; } };
            size = size.saturating_add(event.data.len());
            if size > config.max_response_bytes { failure = Some("response_too_large"); break; }
            if event.data == "[DONE]" {
                match record.finish("completed", None).await {
                    Ok(()) => yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n")),
                    Err(error) => yield Ok(Bytes::from(format!("data: {}\n\n", error.payload()))),
                }
                return;
            }
            let value = match serde_json::from_str::<Value>(&event.data) {
                Ok(value) if value.get("choices").is_some_and(Value::is_array) && value.get("error").is_none() => value,
                _ => { failure = Some("invalid_stream_event"); break; }
            };
            if value.get("usage").is_some_and(|u| !u.is_null()) {
                match Counts::read(&value) {
                    Ok(counts) => record.counts = counts,
                    Err(_) => { failure = Some("invalid_usage"); break; }
                }
            }
            yield Ok(Bytes::from(format!("data: {}\n\n", event.data.replace('\n', "\ndata: "))));
        }
        let error = match record.finish("failed", Some(failure.unwrap_or("stream_interrupted"))).await {
            Ok(()) => ApiError::upstream(),
            Err(error) => error,
        };
        yield Ok(Bytes::from(format!("data: {}\n\n", error.payload())));
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        "text/event-stream".parse().expect("static header"),
    );
    headers.insert("cache-control", "no-cache".parse().expect("static header"));
    if let Ok(id) = id.parse() {
        headers.insert("x-request-id", id);
    }
    (headers, Body::from_stream(stream)).into_response()
}
