mod configuration;
mod providers;
mod reliability;
mod streaming;

use llm_unify::*;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "chat" => include_str!("../fixtures/openai_chat.json"),
        "responses" => include_str!("../fixtures/openai_responses.json"),
        "anthropic" => include_str!("../fixtures/anthropic.json"),
        "gemini" => include_str!("../fixtures/gemini.json"),
        "vertex" => include_str!("../fixtures/vertex.json"),
        _ => panic!("unknown fixture"),
    })
    .unwrap()
}
fn config(server: &MockServer, provider: &str, api: Option<&str>) -> Config {
    let mut config = Config::default();
    let p = config.providers.get_mut(provider).unwrap();
    p.base_url = Some(server.uri());
    p.api = api.map(str::to_owned);
    config.retry.base_delay_ms = 0;
    config.retry.max_delay_ms = 0;
    config
}
fn client(config: Config) -> Client {
    Client::builder()
        .config(config)
        .api_key("openai", "test-secret")
        .api_key("anthropic", "test-secret")
        .api_key("gemini", "test-secret")
        .build()
        .unwrap()
}
async fn mount(server: &MockServer, url: &str, body: Value) {
    Mock::given(method("POST"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}
fn sse(events: &[Value]) -> String {
    events.iter().map(|v| format!("data: {v}\n\n")).collect()
}
async fn mount_sse(server: &MockServer, url: &str, body: String) {
    Mock::given(method("POST"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(server)
        .await;
}
fn call() -> ToolDefinition {
    ToolDefinition::new(
        "weather",
        "Weather",
        json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
    )
}
