use super::*;
use std::collections::BTreeMap;

#[test]
fn config_merges_defaults_and_rejects_invalid_values() {
    let config=Config::from_toml("[http]\nrequest_timeout_ms = 1234\n[providers.openai]\nbase_url = 'http://localhost:1234/v1'").unwrap();
    assert_eq!(config.http.request_timeout_ms, 1234);
    assert!(config.http.stream_idle_timeout_ms > 0);
    assert_eq!(config.providers["openai"].api.as_deref(), Some("responses"));
    assert!(Config::from_toml("[retry]\nmax_attempts=0").is_err());
    assert!(
        Config::from_toml("[providers.openai]\nbase_url='https://user:secret@example.com'")
            .is_err()
    );
}
#[test]
fn model_parses_only_first_slash() {
    assert_eq!(
        ModelId::parse("ollama/org/model").unwrap().parts().unwrap(),
        ("ollama", "org/model")
    );
    for model in [
        "",
        "openai",
        "/model",
        "openai/",
        "open ai/model",
        "openai/my model",
    ] {
        assert!(ModelId::parse(model).is_err());
    }
}
#[tokio::test]
async fn invalid_tool_history_rejected_without_network() {
    let c = Client::builder().build().unwrap();
    for request in [
        ChatRequest::new("openai/test-model"),
        ChatRequest::new("openai/test-model")
            .message(Message::tool(ToolResult::new("missing", json!("x")))),
        ChatRequest::new("openai/test-model").message(Message::assistant(vec![
            ContentBlock::ToolCall(ToolCall::new("c1", "f", json!({}))),
        ])),
        ChatRequest::new("openai/test-model")
            .user("x")
            .max_output_tokens(0),
    ] {
        assert_eq!(
            c.chat(request).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
}
#[test]
fn alias_cycles_and_unknown_deployments_are_rejected() {
    let config = Config {
        aliases: BTreeMap::from([
            ("a".into(), "alias/b".into()),
            ("b".into(), "alias/a".into()),
        ]),
        ..Config::default()
    };
    assert!(Client::builder().config(config).build().is_err());
    let mut config = Config::default();
    config.routes.insert(
        "a".into(),
        Route {
            deployments: vec![Deployment {
                model: "openai/model".into(),
                provider: "missing".into(),
                weight: 1,
            }],
            fallbacks: vec![],
        },
    );
    assert!(Client::builder().config(config).build().is_err());
}
#[test]
fn public_client_is_clone_send_and_sync() {
    static_assertions::assert_impl_all!(Client:Clone,Send,Sync);
}

#[test]
fn shipped_example_configuration_builds_and_typos_are_rejected() {
    let config = Config::from_toml(include_str!("../../config/example.toml")).unwrap();
    Client::builder().config(config).build().unwrap();
    assert!(Config::from_toml("[http]\nrequest_timout_ms=1").is_err());
    assert!(Config::from_toml("[providers.openai]\napi='typo'").is_err());
    assert!(Config::from_toml("[capabilities.'openai/model']\nstreamng=true").is_err());
}

#[tokio::test]
async fn malformed_metadata_and_tool_roles_return_errors_instead_of_panicking() {
    let c = Client::builder().build().unwrap();
    let metadata = ProviderMetadata {
        provider: "openai".into(),
        format: "responses".into(),
        data: json!("not an object"),
    };
    let request = ChatRequest::new("openai/model").message(Message::new(
        Role::User,
        vec![ContentBlock::Text {
            text: "x".into(),
            metadata: vec![metadata],
        }],
    ));
    assert_eq!(
        c.chat(request).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    let request = ChatRequest::new("openai/model").message(Message::new(
        Role::Tool,
        vec![ContentBlock::text("invalid")],
    ));
    assert_eq!(
        c.chat(request).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
}
