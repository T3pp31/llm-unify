use super::*;
use wiremock::matchers::{body_partial_json, header};

#[tokio::test]
async fn native_text_contract() {
    for (provider, api, url, fixture_name) in [
        ("openai", Some("responses"), "/responses", "responses"),
        (
            "openai",
            Some("chat_completions"),
            "/chat/completions",
            "chat",
        ),
        ("anthropic", None, "/messages", "anthropic"),
        ("gemini", None, "/interactions", "gemini"),
    ] {
        let server = MockServer::start().await;
        mount(&server, url, fixture(fixture_name)).await;
        let response = client(config(&server, provider, api))
            .chat(
                ChatRequest::new(format!("{provider}/test-model"))
                    .system("system")
                    .user("hello")
                    .max_output_tokens(100),
            )
            .await
            .unwrap();
        assert_eq!(response.output_text(), "Hello 🌍");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.usage.unwrap().total_tokens, Some(7));
        assert_eq!(response.deployment, provider);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        if matches!(provider, "gemini") || api == Some("responses") {
            assert_eq!(body["store"], false);
        }
    }
}
#[tokio::test]
async fn openai_responses_tools_and_encrypted_metadata_round_trip() {
    let server = MockServer::start().await;
    let response = json!({"status":"completed","output":[{"type":"reasoning","id":"reason","encrypted_content":"encrypted"},{"type":"function_call","id":"item","call_id":"c1","name":"weather","arguments":"{\"city\":\"Tokyo\"}"}]});
    mount(&server, "/responses", response).await;
    let c = client(config(&server, "openai", Some("responses")));
    let response = c
        .chat(
            ChatRequest::new("openai/test-model")
                .user("weather")
                .tool(call()),
        )
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    server.reset().await;
    Mock::given(method("POST")).and(path("/responses")).and(body_partial_json(json!({"store":false,"input":[{"role":"user","content":[{"type":"input_text","text":"weather"}]},{"type":"reasoning","id":"reason","encrypted_content":"encrypted"},{"type":"function_call","id":"item","call_id":"c1","name":"weather","arguments":"{\"city\":\"Tokyo\"}"},{"type":"function_call_output","call_id":"c1","output":"sunny"}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("responses"))).expect(1).mount(&server).await;
    c.chat(
        ChatRequest::new("openai/test-model")
            .user("weather")
            .message(response.into_message())
            .message(Message::tool(ToolResult::new("c1", json!("sunny"))))
            .tool(call()),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn anthropic_tool_history_and_signature_round_trip() {
    let server = MockServer::start().await;
    let mut result = fixture("anthropic");
    result["content"] = json!([{"type":"thinking","thinking":"reason","signature":"sig"},{"type":"tool_use","id":"c1","name":"weather","input":{"city":"Tokyo"}}]);
    result["stop_reason"] = json!("tool_use");
    mount(&server, "/messages", result).await;
    let c = client(config(&server, "anthropic", None));
    let response = c
        .chat(
            ChatRequest::new("anthropic/test-model")
                .user("weather")
                .tool(call()),
        )
        .await
        .unwrap();
    server.reset().await;
    Mock::given(path("/messages")).and(header("anthropic-version","2023-06-01")).and(body_partial_json(json!({"messages":[{"role":"user","content":[{"type":"text","text":"weather"}]},{"role":"assistant","content":[{"type":"thinking","thinking":"reason","signature":"sig"},{"type":"tool_use","id":"c1","name":"weather","input":{"city":"Tokyo"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"sunny","is_error":false}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("anthropic"))).expect(1).mount(&server).await;
    c.chat(
        ChatRequest::new("anthropic/test-model")
            .user("weather")
            .message(response.into_message())
            .message(Message::tool(ToolResult::new("c1", json!("sunny")))),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn gemini_steps_and_signature_round_trip() {
    let server = MockServer::start().await;
    let mut result = fixture("gemini");
    result["steps"] = json!([{"type":"thought","signature":"sig","summary":[]},{"type":"function_call","id":"c1","name":"weather","arguments":{"city":"Tokyo"},"signature":"tool-sig"}]);
    result["status"] = json!("requires_action");
    mount(&server, "/interactions", result).await;
    let c = client(config(&server, "gemini", None));
    let response = c
        .chat(
            ChatRequest::new("gemini/test-model")
                .user("weather")
                .tool(call()),
        )
        .await
        .unwrap();
    server.reset().await;
    Mock::given(path("/interactions")).and(body_partial_json(json!({"store":false,"input":[{"type":"user_input","content":[{"type":"text","text":"weather"}]},{"type":"thought","signature":"sig","summary":[]},{"type":"function_call","id":"c1","name":"weather","arguments":{"city":"Tokyo"},"signature":"tool-sig"},{"type":"function_result","call_id":"c1","name":"weather","result":[{"type":"text","text":"sunny"}],"is_error":false}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("gemini"))).expect(1).mount(&server).await;
    c.chat(
        ChatRequest::new("gemini/test-model")
            .user("weather")
            .message(response.into_message())
            .message(Message::tool(ToolResult::new("c1", json!("sunny")))),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn explicit_capabilities_reject_before_network_but_unknown_options_are_sent() {
    let server = MockServer::start().await;
    let mut config = config(&server, "openai", Some("responses"));
    config.capabilities.insert(
        "openai/known".into(),
        Capabilities {
            temperature: Some(false),
            ..Default::default()
        },
    );
    let c = client(config);
    assert_eq!(
        c.chat(ChatRequest::new("openai/known").user("x").temperature(0.2))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedOption
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    Mock::given(path("/responses"))
        .and(body_partial_json(json!({"temperature":0.2})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("responses")))
        .expect(1)
        .mount(&server)
        .await;
    c.chat(
        ChatRequest::new("openai/unknown")
            .user("x")
            .temperature(0.2),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn metadata_cannot_cross_provider_and_native_options_cannot_override_core() {
    let server = MockServer::start().await;
    let c = client(config(&server, "anthropic", None));
    let mut request = ChatRequest::new("anthropic/test-model").user("x");
    request.options.provider_options.push(ProviderMetadata {
        provider: "gemini".into(),
        format: "interactions".into(),
        data: json!({"foo":1}),
    });
    assert_eq!(
        c.chat(request).await.unwrap_err().kind,
        ErrorKind::UnsupportedOption
    );
    let mut request = ChatRequest::new("anthropic/test-model").user("x");
    request.options.provider_options.push(ProviderMetadata {
        provider: "anthropic".into(),
        format: "messages".into(),
        data: json!({"messages":[]}),
    });
    assert_eq!(
        c.chat(request).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn error_redacts_provider_body_and_credentials() {
    let server = MockServer::start().await;
    Mock::given(path("/responses"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"message":"test-secret sensitive-user-prompt"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client(config(&server, "openai", Some("responses")));
    let request = ChatRequest::new("openai/test-model").user("sensitive-user-prompt");
    assert!(!format!("{c:?} {request:?}").contains("sensitive-user-prompt"));
    let error = c.chat(request).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthError);
    assert!(!format!("{error} {error:?}").contains("test-secret"));
    assert!(!error.retryable);
}
#[tokio::test]
async fn structured_output_reasoning_and_image_conversion() {
    for (provider, api, url, name) in [
        ("openai", Some("responses"), "/responses", "responses"),
        ("anthropic", None, "/messages", "anthropic"),
        ("gemini", None, "/interactions", "gemini"),
    ] {
        let server = MockServer::start().await;
        mount(&server, url, fixture(name)).await;
        let request = ChatRequest::new(format!("{provider}/test-model"))
            .message(Message::new(
                Role::User,
                vec![
                    ContentBlock::text("describe"),
                    ContentBlock::Image(ImageContent::Bytes {
                        mime_type: "image/png".into(),
                        data: vec![1, 2, 3],
                    }),
                ],
            ))
            .response_format(JsonSchema {
                name: "answer".into(),
                schema: json!({"type":"object"}),
                strict: true,
            })
            .reasoning_effort(ReasoningEffort::Low);
        client(config(&server, provider, api))
            .chat(request)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        match provider {
            "openai" => {
                assert_eq!(body["text"]["format"]["schema"], json!({"type":"object"}));
                assert_eq!(body["reasoning"]["effort"], "low");
                assert_eq!(
                    body["input"][0]["content"][1]["image_url"],
                    "data:image/png;base64,AQID"
                );
            }
            "anthropic" => {
                assert_eq!(body["output_config"]["effort"], "low");
                assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "AQID");
            }
            _ => {
                assert_eq!(body["generation_config"]["thinking_level"], "low");
                assert_eq!(body["input"][0]["content"][1]["data"], "AQID");
            }
        }
    }
}
#[tokio::test]
async fn compatible_and_ollama_do_not_require_openai_credentials() {
    let server = MockServer::start().await;
    mount(&server, "/chat/completions", fixture("chat")).await;
    let c = Client::builder()
        .config(config(&server, "ollama", None))
        .build()
        .unwrap();
    c.chat(ChatRequest::new("ollama/org/model").user("x"))
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[0].headers.contains_key("authorization"));
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "org/model");
}
#[tokio::test]
async fn embeddings_are_ordered_and_validate_indices() {
    let server = MockServer::start().await;
    mount(&server,"/embeddings",json!({"data":[{"index":1,"embedding":[0.1,0.2]},{"index":0,"embedding":[0.3,0.4]}],"usage":{"prompt_tokens":4,"total_tokens":4}})).await;
    let c = client(config(&server, "openai", Some("responses")));
    let response = c
        .embed(EmbeddingRequest::new(
            ModelId::parse("openai/embedding-model").unwrap(),
            vec!["a".into(), "b".into()],
        ))
        .await
        .unwrap();
    assert_eq!(response.embeddings[0].index, 0);
    assert_eq!(response.embeddings[1].vector, vec![0.1, 0.2]);
    server.reset().await;
    mount(
        &server,
        "/embeddings",
        json!({"data":[{"index":0,"embedding":[0.1]},{"index":0,"embedding":[0.2]}]}),
    )
    .await;
    assert_eq!(
        c.embed(EmbeddingRequest::new(
            ModelId::parse("openai/embedding-model").unwrap(),
            vec!["a".into(), "b".into()]
        ))
        .await
        .unwrap_err()
        .kind,
        ErrorKind::ProviderError
    );
}
#[tokio::test]
async fn missing_credentials_are_checked_only_when_called() {
    let c = Client::builder().api_key("openai", "").build().unwrap();
    assert_eq!(
        c.chat(ChatRequest::new("openai/test-model").user("x"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::AuthError
    );
}

#[tokio::test]
async fn nested_native_options_extend_without_overriding_canonical_fields() {
    let server = MockServer::start().await;
    Mock::given(path("/interactions"))
        .and(body_partial_json(
            json!({"generation_config":{"temperature":0.4,"seed":42}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("gemini")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(config(&server, "gemini", None));
    let mut request = ChatRequest::new("gemini/model").user("x").temperature(0.4);
    request.options.provider_options.push(ProviderMetadata {
        provider: "gemini".into(),
        format: "interactions".into(),
        data: json!({"generation_config":{"seed":42}}),
    });
    c.chat(request.clone()).await.unwrap();
    request.options.provider_options[0].data = json!({"generation_config":{"temperature":1.0}});
    assert_eq!(
        c.chat(request).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
}

#[tokio::test]
async fn refusals_remain_replayable_native_content() {
    let server = MockServer::start().await;
    mount(&server,"/responses",json!({"status":"completed","output":[{"type":"message","id":"msg_refusal","status":"completed","role":"assistant","content":[{"type":"refusal","refusal":"declined"}]}]})).await;
    let c = client(config(&server, "openai", Some("responses")));
    let response = c
        .chat(ChatRequest::new("openai/model").user("x"))
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::ContentFilter);
    server.reset().await;
    Mock::given(path("/responses")).and(body_partial_json(json!({"input":[{"role":"assistant","content":[{"type":"refusal","refusal":"declined"}]},{"role":"user","content":[{"type":"input_text","text":"another question"}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("responses"))).expect(1).mount(&server).await;
    c.chat(
        ChatRequest::new("openai/model")
            .message(response.into_message())
            .user("another question"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn response_size_and_request_timeout_are_enforced() {
    let server = MockServer::start().await;
    mount(&server, "/responses", fixture("responses")).await;
    let mut settings = config(&server, "openai", Some("responses"));
    settings.http.max_response_bytes = 16;
    assert_eq!(
        client(settings)
            .chat(ChatRequest::new("openai/model").user("x"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProviderError
    );
    server.reset().await;
    Mock::given(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture("responses"))
                .set_delay(std::time::Duration::from_secs(1)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut settings = config(&server, "openai", Some("responses"));
    settings.http.request_timeout_ms = 10;
    settings.retry.max_attempts = 1;
    assert_eq!(
        client(settings)
            .chat(ChatRequest::new("openai/model").user("x"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::TransportError
    );
}

#[tokio::test]
async fn responses_history_preserves_message_phase_and_actual_model_in_both_modes() {
    for streaming in [false, true] {
        let server = MockServer::start().await;
        let item = json!({"type":"message","id":"msg_phase","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"hello","annotations":[]}]});
        let result = json!({"status":"completed","error":null,"model":"actual-model","safety_metadata":{"verified":true},"output":[item.clone()]});
        if streaming {
            mount_sse(&server,"/responses",sse(&[json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello"}),json!({"type":"response.output_item.done","output_index":0,"item":item}),json!({"type":"response.completed","response":result})])).await;
        } else {
            mount(&server, "/responses", result).await;
        }
        let c = client(config(&server, "openai", Some("responses")));
        let request = ChatRequest::new("openai/model").user("x");
        let response = if streaming {
            c.chat_stream(request)
                .await
                .unwrap()
                .collect_response()
                .await
                .unwrap()
        } else {
            c.chat(request).await.unwrap()
        };
        assert_eq!(response.model.as_str(), "openai/actual-model");
        assert_eq!(
            response.provider_metadata[0].data["safety_metadata"]["verified"],
            true
        );
        server.reset().await;
        Mock::given(path("/responses")).and(body_partial_json(json!({"input":[{"type":"message","id":"msg_phase","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"hello","annotations":[]}]},{"role":"user","content":[{"type":"input_text","text":"next"}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("responses"))).expect(1).mount(&server).await;
        c.chat(
            ChatRequest::new("openai/model")
                .message(response.into_message())
                .user("next"),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn manually_created_assistant_history_uses_responses_input_content() {
    let server = MockServer::start().await;
    Mock::given(path("/responses")).and(body_partial_json(json!({"input":[{"role":"assistant","content":[{"type":"input_text","text":"previous"}]},{"role":"user","content":[{"type":"input_text","text":"next"}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(fixture("responses"))).expect(1).mount(&server).await;
    client(config(&server, "openai", Some("responses")))
        .chat(
            ChatRequest::new("openai/model")
                .message(Message::assistant(vec![ContentBlock::text("previous")]))
                .user("next"),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn gemini_incomplete_is_an_explicit_finish_state_in_both_modes() {
    for streaming in [false, true] {
        let server = MockServer::start().await;
        let mut result = fixture("gemini");
        result["status"] = json!("incomplete");
        if streaming {
            mount_sse(&server,"/interactions",sse(&[json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"partial"}}),json!({"event_type":"step.stop","index":0}),json!({"event_type":"interaction.completed","interaction":result})])).await;
        } else {
            mount(&server, "/interactions", result).await;
        }
        let c = client(config(&server, "gemini", None));
        let request = ChatRequest::new("gemini/model").user("x");
        let response = if streaming {
            c.chat_stream(request)
                .await
                .unwrap()
                .collect_response()
                .await
                .unwrap()
        } else {
            c.chat(request).await.unwrap()
        };
        assert_eq!(
            response.finish_reason,
            FinishReason::Unknown("incomplete".into())
        );
        assert_eq!(response.provider_metadata[0].data["status"], "incomplete");
    }
}
