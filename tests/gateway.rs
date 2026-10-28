#![cfg(feature = "gateway")]

use llm_unify::{Config, GatewayConfig, Price, ProviderConfig, gateway::Gateway};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const KEY: &str = "test-gateway-secret";

struct Harness {
    _directory: TempDir,
    upstream: MockServer,
    config: Config,
    url: String,
    task: JoinHandle<()>,
    client: reqwest::Client,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(gateway: Gateway) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, gateway.router()).await.unwrap();
    });
    (url, task)
}

impl Harness {
    async fn new(change: impl FnOnce(&mut Config)) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let upstream = MockServer::start().await;
        let mut config = Config {
            gateway: Some(GatewayConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                database_path: directory.path().join("usage.sqlite"),
                api_key_env: "LLM_UNIFY_TEST_GATEWAY_KEY".into(),
                max_request_bytes: 4096,
                max_in_flight: 2,
                history_page_size: 20,
                sqlite_busy_timeout_ms: 20,
            }),
            ..Config::default()
        };
        config.providers.insert(
            "ollama".into(),
            ProviderConfig::openai_compatible(upstream.uri()),
        );
        config.prices.insert("ollama/org/model".into(), price());
        config.http.request_timeout_ms = 3000;
        config.http.stream_idle_timeout_ms = 1000;
        config.http.max_response_bytes = 4096;
        config.http.max_sse_event_bytes = 1024;
        change(&mut config);
        let (url, task) = serve(Gateway::new(config.clone(), KEY).await.unwrap()).await;
        Self {
            _directory: directory,
            upstream,
            config,
            url,
            task,
            client: reqwest::Client::new(),
        }
    }
    async fn post(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(KEY)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn get(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path}", self.url))
            .bearer_auth(KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }
    async fn mount(&self, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(response)
            .mount(&self.upstream)
            .await;
    }
    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.config.gateway.as_ref().unwrap().database_path).unwrap()
    }
}

fn price() -> Price {
    Price {
        input_per_million: 2.0,
        output_per_million: 3.0,
        effective_date: "2026-10-26".into(),
        source: "local-operating-cost-estimate".into(),
    }
}
fn request() -> Value {
    json!({"model":"ollama/org/model","messages":[{"role":"user","content":"private-prompt"}],"temperature":0.2,"tools":[{"type":"function","function":{"name":"weather","parameters":{"type":"object"}}}]})
}
fn response() -> Value {
    json!({"id":"completion-1","model":"org/model","choices":[{"index":0,"message":{"role":"assistant","content":"private-answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}})
}
fn sse(values: &[Value], done: bool) -> String {
    let mut body: String = values
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    body
}

#[tokio::test]
async fn chat_preserves_payload_and_persists_usage_without_secrets() {
    // Given: a compatible upstream returning known token usage.
    let h = Harness::new(|_| {}).await;
    h.mount(ResponseTemplate::new(200).set_body_json(response()))
        .await;
    // When: a caller sends a chat request through the authenticated gateway.
    let result = h.post(request()).await;
    assert_eq!(result.status(), StatusCode::OK);
    let request_id = result.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(result.json::<Value>().await.unwrap(), response());
    // Then: payloads are preserved, credentials are isolated, and one row is committed.
    let calls = h.upstream.received_requests().await.unwrap();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].headers.contains_key("authorization"));
    let mut expected = request();
    expected["model"] = "org/model".into();
    assert_eq!(
        serde_json::from_slice::<Value>(&calls[0].body).unwrap(),
        expected
    );
    let history = h.get("/usage/requests").await;
    let row = &history["data"][0];
    assert_eq!(history["data"].as_array().unwrap().len(), 1);
    assert_eq!(row["request_id"], request_id);
    assert_eq!(row["status"], "completed");
    assert_eq!(row["input_tokens"], 5);
    assert_eq!(row["output_tokens"], 2);
    assert!((row["estimated_cost_usd"].as_f64().unwrap() - 0.000016).abs() < 1e-12);
    for secret in ["private-prompt", "private-answer", KEY] {
        assert!(!history.to_string().contains(secret));
    }
    let summary = h.get("/usage/summary").await;
    assert_eq!(summary["data"][0]["requests"], 1);
    assert_eq!(summary["data"][0]["unknown_usage_requests"], 0);
}

#[tokio::test]
async fn streaming_requests_usage_and_records_once_at_done() {
    // Given: SSE content and a separate final usage event.
    let h = Harness::new(|_| {}).await;
    let frames = [
        json!({"choices":[{"index":0,"delta":{"content":"こんにちは"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}),
    ];
    h.mount(ResponseTemplate::new(200).set_body_raw(sse(&frames, true), "text/event-stream"))
        .await;
    // When: a streaming request is consumed to completion.
    let mut body = request();
    body["stream"] = true.into();
    let result = h.post(body).await;
    assert_eq!(result.status(), StatusCode::OK);
    let request_id = result.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let text = result.text().await.unwrap();
    // Then: content is streamed and the final usage is recorded once, not per frame.
    assert!(text.contains("こんにちは"));
    assert!(text.ends_with("data: [DONE]\n\n"));
    let calls = h.upstream.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&calls[0].body).unwrap();
    assert_eq!(body["stream_options"]["include_usage"], true);
    let history = h.get("/usage/requests").await;
    assert_eq!(history["data"].as_array().unwrap().len(), 1);
    assert_eq!(history["data"][0]["total_tokens"], 7);
    assert_eq!(history["data"][0]["status"], "completed");
    assert_eq!(history["data"][0]["request_id"], request_id);
}

#[tokio::test]
async fn missing_usage_is_distinct_from_explicit_zero_and_partial_usage() {
    // Given: responses with missing usage, explicit zero, and a missing output count.
    let h = Harness::new(|_| {}).await;
    for usage in [
        Value::Null,
        json!({"prompt_tokens":0,"completion_tokens":0}),
        json!({"prompt_tokens":5}),
    ] {
        h.upstream.reset().await;
        let mut value = response();
        value["usage"] = usage;
        h.mount(ResponseTemplate::new(200).set_body_json(value))
            .await;
        // When: each response is recorded.
        assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    }
    // Then: unknown counts and costs remain null; measured zero stays zero.
    let history = h.get("/usage/requests").await;
    assert!(history["data"][0]["output_tokens"].is_null());
    assert!(history["data"][0]["estimated_cost_usd"].is_null());
    assert_eq!(history["data"][1]["estimated_cost_usd"], 0.0);
    assert_eq!(history["data"][1]["total_tokens"], 0);
    assert!(history["data"][2]["input_tokens"].is_null());
    let summary = h.get("/usage/summary").await;
    assert_eq!(summary["data"][0]["unknown_usage_requests"], 2);
    assert_eq!(summary["data"][0]["unknown_cost_requests"], 2);
}

#[tokio::test]
async fn missing_prices_are_unknown_and_zero_rates_are_explicit() {
    // Given: no configured model price.
    let mut h = Harness::new(|config| {
        config.prices.clear();
    })
    .await;
    h.mount(ResponseTemplate::new(200).set_body_json(response()))
        .await;
    // When: known usage is recorded, then the gateway is restarted with zero prices.
    assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    let mut free = price();
    free.input_per_million = 0.0;
    free.output_per_million = 0.0;
    h.config.prices.insert("ollama/org/model".into(), free);
    h.task.abort();
    let (url, task) = serve(Gateway::new(h.config.clone(), KEY).await.unwrap()).await;
    h.url = url;
    h.task = task;
    assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    // Then: past unknown prices remain unknown, while new zero prices yield zero.
    let history = h.get("/usage/requests").await;
    assert_eq!(history["data"][0]["estimated_cost_usd"], 0.0);
    assert!(history["data"][1]["estimated_cost_usd"].is_null());
    assert!(history["data"][1]["pricing"].is_null());
}

#[tokio::test]
async fn restart_preserves_price_snapshot_and_history_pagination() {
    // Given: an on-disk ledger containing a priced request.
    let mut h = Harness::new(|_| {}).await;
    h.mount(ResponseTemplate::new(200).set_body_json(response()))
        .await;
    assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    // When: the gateway is restarted with a new price, and another request is made.
    h.config
        .prices
        .get_mut("ollama/org/model")
        .unwrap()
        .input_per_million = 9.0;
    h.task.abort();
    let (url, task) = serve(Gateway::new(h.config.clone(), KEY).await.unwrap()).await;
    h.url = url;
    h.task = task;
    assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    let first = h.get("/usage/requests?limit=1").await;
    let before = first["next_before"].as_i64().unwrap();
    let second = h
        .get(&format!("/usage/requests?limit=1&before={before}"))
        .await;
    // Then: pagination returns distinct rows and each retains its original rate.
    assert_eq!(first["data"][0]["pricing"]["input_per_million"], 9.0);
    assert_eq!(second["data"][0]["pricing"]["input_per_million"], 2.0);
    assert_ne!(
        first["data"][0]["request_id"],
        second["data"][0]["request_id"]
    );
    let timestamp = first["data"][0]["started_at_ms"].as_i64().unwrap();
    let excluded = h
        .get(&format!(
            "/usage/requests?from_ms={timestamp}&to_ms={}",
            timestamp + 1
        ))
        .await;
    assert!(!excluded["data"].as_array().unwrap().is_empty());
    let empty = h.get("/usage/requests?model=ollama%2Fother").await;
    assert_eq!(empty["data"], json!([]));
}

#[tokio::test]
async fn summary_groups_by_utc_day_and_model_and_reports_truncation() {
    // Given: requests stored under different UTC days and models.
    let h = Harness::new(|_| {}).await;
    h.mount(ResponseTemplate::new(200).set_body_json(response()))
        .await;
    for _ in 0..3 {
        assert_eq!(h.post(request()).await.status(), StatusCode::OK);
    }
    h.database()
        .execute_batch(
            "UPDATE gateway_usage SET started_at_ms=0 WHERE sequence=1;
        UPDATE gateway_usage SET started_at_ms=86400000, model='ollama/other' WHERE sequence=2;
        UPDATE gateway_usage SET started_at_ms=86400000 WHERE sequence=3;",
        )
        .unwrap();
    // When: all groups and a bounded subset are queried.
    let all = h.get("/usage/summary").await;
    let limited = h.get("/usage/summary?limit=1").await;
    let day = h.get("/usage/summary?from_ms=0&to_ms=86400000").await;
    // Then: UTC boundaries and model grouping are exact, and partial results are flagged.
    assert_eq!(all["data"].as_array().unwrap().len(), 3);
    assert_eq!(limited["truncated"], true);
    assert_eq!(day["data"].as_array().unwrap().len(), 1);
    assert_eq!(day["data"][0]["date_utc"], "1970-01-01");
    assert_eq!(day["data"][0]["total_tokens"], 7);
}

#[tokio::test]
async fn unauthorized_requests_cannot_infer_or_read_usage() {
    // Given: all API routes require a gateway token.
    let h = Harness::new(|_| {}).await;
    // When: requests omit authentication or use an invalid token.
    for token in [None, Some("wrong-key")] {
        for path in ["/usage/requests", "/usage/summary", "/v1/chat/completions"] {
            let mut call = if path.starts_with("/v1/") {
                h.client.post(format!("{}{path}", h.url)).json(&request())
            } else {
                h.client.get(format!("{}{path}", h.url))
            };
            if let Some(token) = token {
                call = call.bearer_auth(token);
            }
            // Then: no inference or ledger disclosure is possible.
            assert_eq!(
                call.send().await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_requests_are_rejected_before_upstream_or_accounting() {
    // Given: unknown models, unsupported providers, and invalid request types.
    let h = Harness::new(|_| {}).await;
    let invalid = [
        json!({}),
        json!({"model":"invalid","messages":[{}]}),
        json!({"model":"missing/model","messages":[{}]}),
        json!({"model":"anthropic/model","messages":[{}]}),
        json!({"model":"ollama/model","messages":[]}),
        json!({"model":"ollama/model","messages":[{}],"stream":"true"}),
        json!({"model":"ollama/model","messages":[{}],"stream":true,"stream_options":null}),
    ];
    // When: invalid requests are submitted.
    for body in invalid {
        let result = h.post(body).await;
        // Then: they fail with a bounded, non-secret error and no upstream call.
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            result.json::<Value>().await.unwrap()["error"]["code"],
            "invalid_request"
        );
    }
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
    assert_eq!(h.get("/usage/requests").await["data"], json!([]));
}

#[tokio::test]
async fn malformed_json_and_oversized_bodies_are_rejected() {
    // Given: a configured request body size bound.
    let h = Harness::new(|config| {
        config.gateway.as_mut().unwrap().max_request_bytes = 256;
    })
    .await;
    // When: malformed JSON or an oversized request is submitted.
    let malformed = h
        .client
        .post(format!("{}/v1/chat/completions", h.url))
        .bearer_auth(KEY)
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    let mut large = request();
    large["messages"][0]["content"] = "x".repeat(1024).into();
    // Then: invalid input is rejected without reaching upstream.
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h.post(large).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn upstream_http_errors_are_recorded_without_retry_or_secret_body() {
    // Given: upstream rate limiting with a sensitive error body.
    let h = Harness::new(|_| {}).await;
    h.mount(ResponseTemplate::new(429).set_body_string("private-upstream-error"))
        .await;
    // When: a request is rejected by upstream.
    let result = h.post(request()).await;
    let request_id = result.headers().get("x-request-id").cloned();
    // Then: the status is preserved, no retry occurs, and sensitive text is not exposed.
    assert_eq!(result.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        !result
            .text()
            .await
            .unwrap()
            .contains("private-upstream-error")
    );
    assert_eq!(h.upstream.received_requests().await.unwrap().len(), 1);
    let history = h.get("/usage/requests").await;
    assert_eq!(
        request_id.as_ref().and_then(|id| id.to_str().ok()),
        history["data"][0]["request_id"].as_str()
    );
    assert_eq!(history["data"][0]["status"], "failed");
    assert_eq!(history["data"][0]["upstream_status"], 429);
    assert!(history["data"][0]["estimated_cost_usd"].is_null());
}

#[tokio::test]
async fn gateway_failures_keep_unique_local_ids_for_ledger_correlation() {
    let h = Harness::new(|_| {}).await;
    let mut ids = std::collections::BTreeSet::new();
    for (upstream, streaming, expected_status, expected_error) in [
        (
            ResponseTemplate::new(503),
            false,
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream_http_error",
        ),
        (
            ResponseTemplate::new(200).set_body_string("invalid-json"),
            false,
            StatusCode::BAD_GATEWAY,
            "invalid_response",
        ),
        (
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices":[],"usage":{"prompt_tokens":-1}})),
            false,
            StatusCode::BAD_GATEWAY,
            "invalid_usage",
        ),
        (
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "application/json"),
            true,
            StatusCode::BAD_GATEWAY,
            "invalid_stream",
        ),
        (
            ResponseTemplate::new(200).set_body_raw("data: {bad}\n\n", "text/event-stream"),
            true,
            StatusCode::OK,
            "invalid_stream_event",
        ),
    ] {
        h.upstream.reset().await;
        h.mount(upstream.insert_header("x-request-id", "upstream-id"))
            .await;
        let mut body = request();
        body["stream"] = streaming.into();
        let response = h
            .client
            .post(format!("{}/v1/chat/completions", h.url))
            .bearer_auth(KEY)
            .header("x-request-id", "caller-id")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        let id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert!(ids.insert(id.clone()));
        let text = response.text().await.unwrap();
        assert!(!text.contains("[DONE]"));
        let history = h.get("/usage/requests").await;
        let row = history["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["request_id"] == id)
            .unwrap();
        assert_eq!(row["status"], "failed");
        assert_eq!(row["error_code"], expected_error);
        assert_eq!(h.upstream.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn transport_failure_keeps_id_and_does_not_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connection = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(socket.read(&mut buffer).await.unwrap() > 0);
        // Close after receiving the request, without returning HTTP headers.
    });
    let h = Harness::new(|config| {
        config.providers.get_mut("ollama").unwrap().base_url = Some(format!("http://{address}"));
    })
    .await;
    let response = h.post(request()).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    connection.await.unwrap();
    let history = h.get("/usage/requests").await;
    assert_eq!(history["data"].as_array().unwrap().len(), 1);
    assert_eq!(history["data"][0]["request_id"], id);
    assert_eq!(history["data"][0]["error_code"], "upstream_transport_error");
}

#[tokio::test]
async fn malformed_upstream_usage_and_responses_are_not_successes() {
    // Given: invalid response shapes, invalid count types, and overflowing counts.
    let h = Harness::new(|_| {}).await;
    let bodies = [
        json!({"error":"private-error"}),
        json!({"choices":[],"usage":{"prompt_tokens":-1}}),
        json!({"choices":[],"usage":{"prompt_tokens":"5"}}),
        json!({"choices":[],"usage":{"prompt_tokens":u64::MAX}}),
        json!({"choices":[],"usage":{"prompt_tokens":i64::MAX,"completion_tokens":1}}),
    ];
    // When: upstream returns each malformed body.
    for body in bodies {
        h.upstream.reset().await;
        h.mount(ResponseTemplate::new(200).set_body_json(body))
            .await;
        // Then: no malformed response is reported as successful.
        assert_eq!(h.post(request()).await.status(), StatusCode::BAD_GATEWAY);
    }
    let rows = h.get("/usage/requests").await;
    assert!(
        rows["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "failed")
    );
}

#[tokio::test]
async fn response_size_limit_and_invalid_json_are_recorded_as_failures() {
    // Given: a small upstream response limit and non-JSON output.
    let h = Harness::new(|config| {
        config.http.max_response_bytes = 512;
    })
    .await;
    // When: upstream returns oversized or unparseable output.
    for text in ["x".repeat(1024), "invalid-json".into()] {
        h.upstream.reset().await;
        h.mount(ResponseTemplate::new(200).set_body_raw(text, "application/json"))
            .await;
        // Then: the client receives an upstream error and the ledger records a failure.
        assert_eq!(h.post(request()).await.status(), StatusCode::BAD_GATEWAY);
    }
    assert_eq!(
        h.get("/usage/summary").await["data"][0]["failed_requests"],
        2
    );
}

#[tokio::test]
async fn truncated_and_invalid_sse_do_not_emit_successful_done() {
    // Given: incomplete SSE, malformed events, a wrong content type, and oversized frames.
    let h = Harness::new(|config| {
        config.http.max_sse_event_bytes = 512;
    })
    .await;
    let cases = [
        (
            sse(
                &[json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}})],
                false,
            ),
            "text/event-stream",
        ),
        ("data: {bad}\n\n".into(), "text/event-stream"),
        ("data: [DONE]\n\n".into(), "application/json"),
        (
            format!("data: {}\n\n", "x".repeat(1024)),
            "text/event-stream",
        ),
    ];
    // When: each broken stream is consumed.
    for (body, content_type) in cases {
        h.upstream.reset().await;
        h.mount(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .await;
        let mut body = request();
        body["stream"] = true.into();
        let result = h.post(body).await;
        let text = result.text().await.unwrap();
        // Then: a sanitized error is emitted and no [DONE] success marker is fabricated.
        assert!(text.contains("upstream_error"));
        assert!(!text.contains("[DONE]"));
    }
    let rows = h.get("/usage/requests").await;
    assert!(
        rows["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "failed")
    );
    // A final usage event received before a broken ending is still retained.
    assert_eq!(rows["data"][3]["total_tokens"], 7);
}

#[tokio::test]
async fn invalid_usage_filters_are_rejected() {
    // Given: bounded page sizes and half-open time ranges.
    let h = Harness::new(|_| {}).await;
    // When: filters violate types, limits, ranges, or endpoint semantics.
    for path in [
        "/usage/requests?limit=0",
        "/usage/requests?limit=21",
        "/usage/requests?before=0",
        "/usage/requests?from_ms=-1",
        "/usage/requests?from_ms=2&to_ms=1",
        "/usage/requests?limit=bad",
        "/usage/requests?unknown=1",
        "/usage/summary?before=1",
    ] {
        let response = h
            .client
            .get(format!("{}{path}", h.url))
            .bearer_auth(KEY)
            .send()
            .await
            .unwrap();
        // Then: invalid queries cannot silently change the report's meaning.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn locked_ledger_prevents_upstream_calls() {
    // Given: the ledger cannot durably insert a pending request.
    let h = Harness::new(|_| {}).await;
    let lock = h.database();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    // When: the request cannot acquire the DB write lock within its configured timeout.
    let result = h.post(request()).await;
    // Then: it fails closed, with no unaccounted upstream request.
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        result.json::<Value>().await.unwrap()["error"]["code"],
        "usage_storage_unavailable"
    );
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
    lock.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn invalid_configuration_and_database_startup_fail_cleanly() {
    // Given: invalid limits, transient storage, a directory path, and a future DB schema.
    let h = Harness::new(|_| {}).await;
    // When: startup receives unusable configuration or storage.
    let mut config = h.config.clone();
    config.gateway.as_mut().unwrap().max_in_flight = 0;
    assert!(Gateway::new(config, KEY).await.is_err());
    let mut config = h.config.clone();
    config.gateway.as_mut().unwrap().database_path = ":memory:".into();
    assert!(Gateway::new(config, KEY).await.is_err());
    let mut config = h.config.clone();
    config.gateway.as_mut().unwrap().database_path = h._directory.path().into();
    let error = Gateway::new(config, KEY).await.err().unwrap();
    // Then: failures are explicit and do not disclose filesystem paths.
    assert_eq!(error.to_string(), "usage storage unavailable");
    h.database()
        .execute_batch("PRAGMA user_version=99")
        .unwrap();
    assert!(Gateway::new(h.config.clone(), KEY).await.is_err());
    assert!(Gateway::new(h.config.clone(), "").await.is_err());
}

struct ControlledStream {
    url: String,
    frames: tokio::sync::mpsc::Sender<bytes::Bytes>,
    task: JoinHandle<()>,
}
impl Drop for ControlledStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl ControlledStream {
    async fn new() -> Self {
        use axum::{body::Body, response::IntoResponse, routing::post};
        let (frames, receiver) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
        let receiver = std::sync::Arc::new(tokio::sync::Mutex::new(Some(receiver)));
        let router = axum::Router::new().route(
            "/chat/completions",
            post(move || {
                let receiver = receiver.clone();
                async move {
                    let mut receiver = receiver.lock().await.take().unwrap();
                    let stream = async_stream::stream! {
                        while let Some(frame)=receiver.recv().await {
                            yield Ok::<_,std::convert::Infallible>(frame);
                        }
                    };
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(stream),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, frames, task }
    }
    async fn send(&self, text: &str) {
        self.frames
            .send(bytes::Bytes::copy_from_slice(text.as_bytes()))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn disconnected_clients_are_cancelled_and_release_concurrency() {
    // Given: a gateway allowing only one active, deliberately unfinished stream.
    let upstream = ControlledStream::new().await;
    let h = Harness::new(|config| {
        config.gateway.as_mut().unwrap().max_in_flight = 1;
        config
            .providers
            .insert("control".into(), config.providers["ollama"].clone());
        config.providers.get_mut("ollama").unwrap().base_url = Some(upstream.url.clone());
    })
    .await;
    upstream
        .send("data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n")
        .await;
    let mut body = request();
    body["stream"] = true.into();
    let mut result = h.post(body).await;
    assert!(result.chunk().await.unwrap().is_some());
    // When: a concurrent request arrives, then the original client disconnects.
    assert_eq!(
        h.post(request()).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(result);
    let rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = h.get("/usage/requests").await;
            if rows["data"][0]["status"] == "cancelled" {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Then: cancellation is persisted with unknown cost, and no second attempt was made.
    assert_eq!(rows["data"].as_array().unwrap().len(), 1);
    assert!(rows["data"][0]["estimated_cost_usd"].is_null());
    assert_eq!(rows["data"][0]["error_code"], "request_cancelled");
    h.mount(ResponseTemplate::new(200).set_body_json(response()))
        .await;
    let mut next = request();
    next["model"] = "control/org/model".into();
    assert_eq!(h.post(next).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn stream_finalization_storage_failure_does_not_emit_done() {
    // Given: a stream whose pending row exists but final DB updates are blocked.
    let upstream = ControlledStream::new().await;
    let h = Harness::new(|config| {
        config.providers.get_mut("ollama").unwrap().base_url = Some(upstream.url.clone());
    })
    .await;
    upstream
        .send("data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n")
        .await;
    let mut body = request();
    body["stream"] = true.into();
    let mut result = h.post(body).await;
    let request_id = result.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(result.chunk().await.unwrap().is_some());
    let lock = h.database();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    // When: upstream completes while storage remains unavailable.
    upstream.send("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n").await;
    let remainder = result.text().await.unwrap();
    // Then: completion is withheld, a storage error is visible, and success is not recorded.
    assert!(remainder.contains("usage_storage_unavailable"));
    assert!(!remainder.contains("[DONE]"));
    let status: String = lock
        .query_row("SELECT status FROM gateway_usage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(status, "pending");
    let recorded_id: String = lock
        .query_row("SELECT request_id FROM gateway_usage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(recorded_id, request_id);
    lock.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn idle_streams_time_out_as_failures() {
    // Given: an upstream stream that stops producing events.
    let upstream = ControlledStream::new().await;
    let h = Harness::new(|config| {
        config.http.stream_idle_timeout_ms = 30;
        config.providers.get_mut("ollama").unwrap().base_url = Some(upstream.url.clone());
    })
    .await;
    upstream.send("data: {\"choices\":[]}\n\n").await;
    // When: the configured idle deadline expires.
    let mut body = request();
    body["stream"] = true.into();
    let text = tokio::time::timeout(Duration::from_secs(3), async {
        h.post(body).await.text().await.unwrap()
    })
    .await
    .unwrap();
    // Then: the stream ends with an error, not a fabricated successful completion.
    assert!(text.contains("upstream_error"));
    assert!(!text.contains("[DONE]"));
    assert_eq!(
        h.get("/usage/requests").await["data"][0]["status"],
        "failed"
    );
}

#[tokio::test]
async fn unfinished_rows_survive_restart_as_pending_not_free_successes() {
    // Given: a pending row remaining after a process failed before finalization.
    let mut h = Harness::new(|_| {}).await;
    h.database()
        .execute(
            "INSERT INTO gateway_usage(request_id,started_at_ms,model,provider,status)
        VALUES ('unfinished',0,'ollama/org/model','ollama','pending')",
            [],
        )
        .unwrap();
    // When: the gateway opens the same persistent database again.
    h.task.abort();
    let (url, task) = serve(Gateway::new(h.config.clone(), KEY).await.unwrap()).await;
    h.url = url;
    h.task = task;
    let report = h.get("/usage/summary").await;
    // Then: the unresolved attempt remains visible and its usage/cost stays unknown.
    assert_eq!(report["data"][0]["pending_requests"], 1);
    assert_eq!(report["data"][0]["completed_requests"], 0);
    assert_eq!(report["data"][0]["unknown_cost_requests"], 1);
    assert!(report["data"][0]["estimated_cost_usd"].is_null());
}

#[tokio::test]
async fn redirects_and_missing_provider_credentials_do_not_make_extra_calls() {
    // Given: an upstream redirect to another server.
    let h = Harness::new(|_| {}).await;
    let destination = MockServer::start().await;
    h.mount(ResponseTemplate::new(307).insert_header("Location", destination.uri()))
        .await;
    // When: the upstream attempts a redirect, or configured credentials are absent.
    assert_eq!(h.post(request()).await.status(), StatusCode::BAD_GATEWAY);
    let missing = Harness::new(|config| {
        config.providers.get_mut("ollama").unwrap().api_key_env = Some(format!(
            "LLM_UNIFY_MISSING_{}",
            uuid::Uuid::new_v4().simple()
        ));
    })
    .await;
    let result = missing.post(request()).await;
    // Then: redirects are not followed and a gateway token cannot replace upstream credentials.
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        result.json::<Value>().await.unwrap()["error"]["code"],
        "provider_credentials_missing"
    );
    assert!(destination.received_requests().await.unwrap().is_empty());
    assert!(
        missing
            .upstream
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn extreme_gateway_settings_return_errors_instead_of_panicking() {
    // Given: settings exceeding SQLite, semaphore, or pagination integer ranges.
    let h = Harness::new(|_| {}).await;
    // When: each setting is validated before use by its dependency.
    for field in ["busy_timeout", "page_size", "permits"] {
        let mut config = h.config.clone();
        let settings = config.gateway.as_mut().unwrap();
        match field {
            "busy_timeout" => settings.sqlite_busy_timeout_ms = u64::MAX,
            "page_size" => settings.history_page_size = usize::MAX,
            _ => settings.max_in_flight = usize::MAX,
        }
        // Then: startup fails with a configuration error, without panicking or spawning work.
        assert!(Gateway::new(config, KEY).await.is_err(), "{field}");
    }
}
