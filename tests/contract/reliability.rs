use super::*;
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn response(model: ModelId) -> ChatResponse {
    ChatResponse {
        model,
        deployment: "stub".into(),
        content: vec![ContentBlock::text("answer")],
        usage: Some(TokenUsage {
            input_tokens: 5,
            output_tokens: 2,
            total_tokens: Some(7),
            ..Default::default()
        }),
        finish_reason: FinishReason::Stop,
        cache_hit: false,
        cost_usd: None,
        request_id: None,
        provider_metadata: vec![],
    }
}
struct Stub {
    calls: AtomicUsize,
    fail_first: usize,
    auth: bool,
    retry_after: Option<Duration>,
}
impl Stub {
    fn new(fail_first: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail_first,
            auth: false,
            retry_after: None,
        })
    }
}
#[async_trait]
impl Provider for Stub {
    async fn chat(&self, r: &ChatRequest) -> Result<ChatResponse, Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.fail_first {
            let mut error = Error::new(
                if self.auth {
                    ErrorKind::AuthError
                } else {
                    ErrorKind::RateLimitError
                },
                "stub failure",
            );
            error.retryable = !self.auth;
            error.retry_after = self.retry_after;
            return Err(error);
        }
        Ok(response(r.model.clone()))
    }
    async fn chat_stream(&self, r: &ChatRequest) -> Result<ChatStream, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let error = Error::new(ErrorKind::TransportError, "stub disconnected");
        Ok(ChatStream::new(
            r.model.clone(),
            "stub",
            stream::iter(vec![
                Ok(ChatEvent::TextDelta {
                    index: 0,
                    delta: "partial".into(),
                }),
                Err(error),
            ]),
        ))
    }
}
fn route_config() -> Config {
    let mut c = Config::default();
    c.retry.base_delay_ms = 0;
    c.retry.max_delay_ms = 0;
    c.routes.insert(
        "balanced".into(),
        Route {
            deployments: vec![
                Deployment {
                    model: "stub/model".into(),
                    provider: "a".into(),
                    weight: 2,
                },
                Deployment {
                    model: "stub/model".into(),
                    provider: "b".into(),
                    weight: 1,
                },
            ],
            fallbacks: vec![],
        },
    );
    c
}

struct StartupStub(Stub);
#[async_trait]
impl Provider for StartupStub {
    async fn chat(&self, r: &ChatRequest) -> Result<ChatResponse, Error> {
        self.0.chat(r).await
    }
    async fn chat_stream(&self, r: &ChatRequest) -> Result<ChatStream, Error> {
        let response = self.0.chat(r).await?;
        Ok(ChatStream::new(
            r.model.clone(),
            "stub",
            stream::iter(vec![
                Ok(ChatEvent::Usage(response.usage.unwrap())),
                Ok(ChatEvent::Completed {
                    reason: FinishReason::Stop,
                }),
            ]),
        ))
    }
}

#[tokio::test(start_paused = true)]
async fn streaming_startup_failure_opens_circuit_and_successful_probe_recovers() {
    let stub = Arc::new(StartupStub(Stub {
        calls: AtomicUsize::new(0),
        fail_first: 1,
        auth: false,
        retry_after: None,
    }));
    let mut config = Config::default();
    config.retry.max_attempts = 1;
    config.router.failure_threshold = 1;
    config.router.cooldown_ms = 100;
    let client = Client::builder()
        .config(config)
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    let request = ChatRequest::new("stub/model").user("x");
    assert!(client.chat_stream(request.clone()).await.is_err());
    // A clone must share the open circuit and avoid a second upstream call.
    assert!(client.clone().chat_stream(request.clone()).await.is_err());
    assert_eq!(stub.0.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(101)).await;
    for _ in 0..2 {
        client
            .chat_stream(request.clone())
            .await
            .unwrap()
            .collect_response()
            .await
            .unwrap();
    }
    assert_eq!(stub.0.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn streaming_startup_failure_falls_back_but_auth_failure_does_not() {
    for auth in [false, true] {
        let a = Arc::new(StartupStub(Stub {
            calls: AtomicUsize::new(0),
            fail_first: usize::MAX,
            auth,
            retry_after: None,
        }));
        let b = Arc::new(StartupStub(Stub {
            calls: AtomicUsize::new(0),
            fail_first: 0,
            auth: false,
            retry_after: None,
        }));
        let mut config = route_config();
        config.router.failure_threshold = 1;
        let client = Client::builder()
            .config(config)
            .provider("a", a.clone())
            .provider("b", b.clone())
            .build()
            .unwrap();
        let result = client
            .chat_stream(ChatRequest::new("alias/balanced").user("x"))
            .await;
        if auth {
            assert!(matches!(result, Err(error) if error.kind == ErrorKind::AuthError));
            assert_eq!(b.0.calls.load(Ordering::SeqCst), 0);
        } else {
            let response = result.unwrap().collect_response().await.unwrap();
            assert_eq!(response.deployment, "b");
            assert_eq!(b.0.calls.load(Ordering::SeqCst), 1);
        }
        assert_eq!(a.0.calls.load(Ordering::SeqCst), 1);
    }
}

#[derive(Default)]
struct StalledStartup(AtomicUsize);
#[async_trait]
impl Provider for StalledStartup {
    async fn chat(&self, _: &ChatRequest) -> Result<ChatResponse, Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
    async fn chat_stream(&self, r: &ChatRequest) -> Result<ChatStream, Error> {
        self.chat(r).await?;
        std::future::pending().await
    }
    async fn embed(&self, _: &EmbeddingRequest) -> Result<EmbeddingResponse, Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn total_deadline_counts_as_failure_for_chat_streaming_and_embeddings() {
    for operation in ["chat", "stream", "embed"] {
        let stub = Arc::new(StalledStartup::default());
        let mut config = Config::default();
        config.retry.total_timeout_ms = 10;
        config.router.failure_threshold = 1;
        config.router.cooldown_ms = 100;
        let client = Client::builder()
            .config(config)
            .provider("stub", stub.clone())
            .build()
            .unwrap();
        for _ in 0..2 {
            match operation {
                "stream" => assert!(
                    client
                        .chat_stream(ChatRequest::new("stub/model").user("x"))
                        .await
                        .is_err()
                ),
                "chat" => assert!(
                    client
                        .chat(ChatRequest::new("stub/model").user("x"))
                        .await
                        .is_err()
                ),
                _ => assert!(
                    client
                        .embed(EmbeddingRequest::new(
                            ModelId::parse("stub/model").unwrap(),
                            vec!["x".into()]
                        ))
                        .await
                        .is_err()
                ),
            }
        }
        assert_eq!(stub.0.load(Ordering::SeqCst), 1, "operation={operation}");
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_streaming_startup_releases_lease_without_opening_circuit() {
    let stub = Arc::new(StalledStartup::default());
    let mut config = Config::default();
    config.router.failure_threshold = 1;
    let client = Client::builder()
        .config(config)
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    for expected_calls in 1..=2 {
        let clone = client.clone();
        let task = tokio::spawn(async move {
            let _ = clone
                .chat_stream(ChatRequest::new("stub/model").user("x"))
                .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(stub.0.load(Ordering::SeqCst), expected_calls);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test(start_paused = true)]
async fn retry_after_and_global_attempt_budget() {
    let stub = Arc::new(Stub {
        calls: AtomicUsize::new(0),
        fail_first: 2,
        auth: false,
        retry_after: Some(Duration::from_secs(2)),
    });
    let mut c = Config::default();
    c.router.failure_threshold = 10;
    let client = Client::builder()
        .config(c)
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    let start = tokio::time::Instant::now();
    client
        .chat(ChatRequest::new("stub/model").user("x"))
        .await
        .unwrap();
    assert_eq!(stub.calls.load(Ordering::SeqCst), 3);
    assert_eq!(start.elapsed(), Duration::from_secs(4));
}
#[tokio::test]
async fn attempt_budget_applies_across_fallbacks() {
    let a = Stub::new(100);
    let b = Stub::new(100);
    let mut c = route_config();
    c.router.failure_threshold = 10;
    let client = Client::builder()
        .config(c)
        .provider("a", a.clone())
        .provider("b", b.clone())
        .build()
        .unwrap();
    assert!(
        client
            .chat(ChatRequest::new("alias/balanced").user("x"))
            .await
            .is_err()
    );
    assert_eq!(
        a.calls.load(Ordering::SeqCst) + b.calls.load(Ordering::SeqCst),
        3
    );
}
#[tokio::test]
async fn weighted_round_robin_is_shared_by_client_clones() {
    let a = Stub::new(0);
    let b = Stub::new(0);
    let client = Client::builder()
        .config(route_config())
        .provider("a", a.clone())
        .provider("b", b.clone())
        .build()
        .unwrap();
    for _ in 0..9 {
        let c = client.clone();
        c.chat(ChatRequest::new("alias/balanced").user("x"))
            .await
            .unwrap();
    }
    assert_eq!(a.calls.load(Ordering::SeqCst), 6);
    assert_eq!(b.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test(start_paused = true)]
async fn open_circuit_skips_failed_deployment_then_recovers() {
    let a = Stub::new(1);
    let b = Stub::new(0);
    let mut c = route_config();
    c.router.failure_threshold = 1;
    c.router.cooldown_ms = 100;
    let client = Client::builder()
        .config(c)
        .provider("a", a.clone())
        .provider("b", b.clone())
        .build()
        .unwrap();
    let request = ChatRequest::new("alias/balanced").user("x");
    let response = client.chat(request.clone()).await.unwrap();
    assert_eq!(response.deployment, "b");
    client.chat(request.clone()).await.unwrap();
    assert_eq!(a.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(101)).await;
    for _ in 0..3 {
        client.chat(request.clone()).await.unwrap();
    }
    assert!(a.calls.load(Ordering::SeqCst) > 1);
}
#[tokio::test]
async fn auth_failure_does_not_fallback() {
    let a = Arc::new(Stub {
        calls: AtomicUsize::new(0),
        fail_first: 100,
        auth: true,
        retry_after: None,
    });
    let b = Stub::new(0);
    let client = Client::builder()
        .config(route_config())
        .provider("a", a.clone())
        .provider("b", b.clone())
        .build()
        .unwrap();
    assert_eq!(
        client
            .chat(ChatRequest::new("alias/balanced").user("x"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::AuthError
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn emitted_stream_event_prevents_retry() {
    let stub = Stub::new(0);
    let client = Client::builder()
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    let mut stream = client
        .chat_stream(ChatRequest::new("stub/model").user("x"))
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().kind,
        ErrorKind::StreamInterrupted
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn unknown_price_is_none_and_explicit_price_is_used() {
    let stub = Stub::new(0);
    let c = Client::builder()
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    assert_eq!(
        c.chat(ChatRequest::new("stub/model").user("x"))
            .await
            .unwrap()
            .cost_usd,
        None
    );
    let mut config = Config::default();
    config.prices.insert(
        "stub/model".into(),
        Price {
            input_per_million: 1.0,
            output_per_million: 2.0,
            effective_date: "2026-10-25".into(),
            source: "test".into(),
        },
    );
    let c = Client::builder()
        .config(config)
        .provider("stub", stub)
        .build()
        .unwrap();
    assert_eq!(
        c.chat(ChatRequest::new("stub/model").user("x"))
            .await
            .unwrap()
            .cost_usd,
        Some(9.0 / 1_000_000.0)
    );
}
#[tokio::test(start_paused = true)]
async fn memory_cache_expires_and_evicts_oldest() {
    let cache = MemoryCache::new(1, Duration::from_millis(100)).unwrap();
    let r = response(ModelId::parse("stub/model").unwrap());
    cache.insert("a".into(), r.clone()).await.unwrap();
    tokio::time::advance(Duration::from_millis(1)).await;
    cache.insert("b".into(), r.clone()).await.unwrap();
    assert!(cache.get("a").await.unwrap().is_none());
    assert!(cache.get("b").await.unwrap().is_some());
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(cache.get("b").await.unwrap().is_none());
}
#[tokio::test]
async fn cache_is_shared_by_clones_and_isolated_between_clients() {
    let cache = Arc::new(MemoryCache::new(10, Duration::from_secs(60)).unwrap());
    let stub = Stub::new(0);
    let c = Client::builder()
        .cache(cache.clone())
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    let request = ChatRequest::new("stub/model").user("x");
    assert!(!c.chat(request.clone()).await.unwrap().cache_hit);
    assert!(c.clone().chat(request.clone()).await.unwrap().cache_hit);
    let other = Client::builder()
        .cache(cache)
        .provider("stub", stub.clone())
        .build()
        .unwrap();
    assert!(!other.chat(request).await.unwrap().cache_hit);
    assert_eq!(stub.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn cached_tool_calls_preserve_identity_and_are_marked() {
    let server = MockServer::start().await;
    mount(&server,"/responses",json!({"status":"completed","output":[{"type":"function_call","call_id":"cached-call","name":"weather","arguments":"{\"city\":\"Tokyo\"}"}]})).await;
    let mut config = config(&server, "openai", Some("responses"));
    config.cache.enabled = true;
    let c = client(config);
    let request = ChatRequest::new("openai/test-model")
        .user("weather")
        .tool(call());
    let first = c.chat(request.clone()).await.unwrap();
    let cached = c.chat(request).await.unwrap();
    assert!(cached.cache_hit);
    assert_eq!(cached.content, first.content);
    assert!(matches!(&cached.content[0],ContentBlock::ToolCall(call) if call.id=="cached-call"));
}
#[derive(Default)]
struct RecordingObserver(Mutex<Vec<Observation>>);
impl Observer for RecordingObserver {
    fn observe(&self, event: &Observation) {
        self.0.lock().unwrap().push(event.clone())
    }
}
#[tokio::test]
async fn observer_receives_usage_and_cache_hit_without_prompt() {
    let observer = Arc::new(RecordingObserver::default());
    let stub = Stub::new(0);
    let cache = Arc::new(MemoryCache::new(2, Duration::from_secs(60)).unwrap());
    let c = Client::builder()
        .provider("stub", stub)
        .observer(observer.clone())
        .cache(cache)
        .build()
        .unwrap();
    let request = ChatRequest::new("stub/model").user("sensitive-prompt");
    c.chat(request.clone()).await.unwrap();
    c.chat(request).await.unwrap();
    let observations = observer.0.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert!(!observations[0].cache_hit);
    assert!(observations[1].cache_hit);
    assert_eq!(observations[0].usage.as_ref().unwrap().input_tokens, 5);
    assert!(!format!("{observations:?}").contains("sensitive-prompt"));
}
#[tokio::test]
async fn http_429_retries_but_400_does_not() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    Mock::given(path("/responses"))
        .respond_with(move |_: &wiremock::Request| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(fixture("responses"))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    client(config(&server, "openai", Some("responses")))
        .chat(ChatRequest::new("openai/test-model").user("x"))
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    server.reset().await;
    Mock::given(path("/responses"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(config(&server, "openai", Some("responses")))
            .chat(ChatRequest::new("openai/test-model").user("x"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
}

struct UnavailableCache;
#[async_trait]
impl Cache for UnavailableCache {
    async fn get(&self, _: &str) -> Result<Option<ChatResponse>, Error> {
        Err(Error::new(ErrorKind::TransportError, "cache unavailable"))
    }
    async fn insert(&self, _: String, _: ChatResponse) -> Result<(), Error> {
        Err(Error::new(ErrorKind::TransportError, "cache unavailable"))
    }
}
#[tokio::test]
async fn custom_cache_failures_do_not_fail_successful_api_requests() {
    let stub = Stub::new(0);
    let c = Client::builder()
        .provider("stub", stub.clone())
        .cache(Arc::new(UnavailableCache))
        .build()
        .unwrap();
    for _ in 0..2 {
        let response = c
            .chat(ChatRequest::new("stub/model").user("x"))
            .await
            .unwrap();
        assert_eq!(response.output_text(), "answer");
        assert!(!response.cache_hit);
    }
    assert_eq!(stub.calls.load(Ordering::SeqCst), 2);
}
