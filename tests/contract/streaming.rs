use super::*;
use futures_util::StreamExt;

#[tokio::test]
async fn all_native_text_streams_complete() {
    let cases = [
        (
            "openai",
            Some("chat_completions"),
            "/chat/completions",
            format!(
                "{}data: [DONE]\n\n",
                sse(&[
                    json!({"choices":[{"index":0,"delta":{"content":"こんにちは"},"finish_reason":null}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}})
                ])
            ),
        ),
        (
            "openai",
            Some("responses"),
            "/responses",
            sse(&[
                json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"こんにちは"}),
                json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}),
            ]),
        ),
        (
            "anthropic",
            None,
            "/messages",
            sse(&[
                json!({"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":0}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"こんにちは"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                json!({"type":"message_stop"}),
            ]),
        ),
        (
            "gemini",
            None,
            "/interactions",
            sse(&[
                json!({"event_type":"interaction.created","interaction":{"status":"in_progress"}}),
                json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
                json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"こんにちは"}}),
                json!({"event_type":"step.stop","index":0}),
                json!({"event_type":"interaction.completed","interaction":{"status":"completed","usage":{"total_input_tokens":5,"total_output_tokens":2,"total_tokens":7}}}),
            ]),
        ),
    ];
    for (provider, api, url, body) in cases {
        let server = MockServer::start().await;
        mount_sse(&server, url, body).await;
        let result = client(config(&server, provider, api))
            .chat_stream(ChatRequest::new(format!("{provider}/test-model")).user("hello"))
            .await
            .unwrap()
            .collect_response()
            .await
            .unwrap();
        assert_eq!(result.output_text(), "こんにちは");
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.usage.unwrap().total_tokens, Some(7));
    }
}
#[tokio::test]
async fn openai_tool_json_is_parsed_only_at_completion() {
    let server = MockServer::start().await;
    let body = sse(&[
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"weather"}}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"city\":"}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"Tokyo\"}"}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ]);
    mount_sse(&server, "/responses", body).await;
    let response = client(config(&server, "openai", Some("responses")))
        .chat_stream(ChatRequest::new("openai/test-model").user("x").tool(call()))
        .await
        .unwrap()
        .collect_response()
        .await
        .unwrap();
    let ContentBlock::ToolCall(call) = &response.content[0] else {
        panic!("expected tool")
    };
    assert_eq!(call.arguments, json!({"city":"Tokyo"}));
}
#[tokio::test]
async fn anthropic_and_gemini_stream_tool_arguments_and_signatures() {
    let cases = [
        (
            "anthropic",
            "/messages",
            sse(&[
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reason"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c1","name":"weather","input":{}}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"Tokyo\"}"}}),
                json!({"type":"content_block_stop","index":1}),
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
                json!({"type":"message_stop"}),
            ]),
        ),
        (
            "gemini",
            "/interactions",
            sse(&[
                json!({"event_type":"step.start","index":0,"step":{"type":"thought","summary":[]}}),
                json!({"event_type":"step.delta","index":0,"delta":{"type":"thought_signature","signature":"sig"}}),
                json!({"event_type":"step.stop","index":0}),
                json!({"event_type":"step.start","index":1,"step":{"type":"function_call","id":"c1","name":"weather"}}),
                json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"{\"city\":"}}),
                json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"\"Tokyo\"}"}}),
                json!({"event_type":"step.stop","index":1}),
                json!({"event_type":"interaction.completed","interaction":{"status":"requires_action"}}),
            ]),
        ),
    ];
    for (provider, url, body) in cases {
        let server = MockServer::start().await;
        mount_sse(&server, url, body).await;
        let response = client(config(&server, provider, None))
            .chat_stream(
                ChatRequest::new(format!("{provider}/test-model"))
                    .user("x")
                    .tool(call()),
            )
            .await
            .unwrap()
            .collect_response()
            .await
            .unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        let ContentBlock::Provider(metadata) = &response.content[0] else {
            panic!("expected metadata")
        };
        assert_eq!(metadata.data["signature"], "sig");
        let ContentBlock::ToolCall(call) = &response.content[1] else {
            panic!("expected tool")
        };
        assert_eq!(call.arguments, json!({"city":"Tokyo"}));
    }
}
#[tokio::test]
async fn interruption_preserves_partial_text_and_never_retries() {
    let server = MockServer::start().await;
    mount_sse(&server,"/responses",sse(&[json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"})])).await;
    let mut stream = client(config(&server, "openai", Some("responses")))
        .chat_stream(ChatRequest::new("openai/test-model").user("x"))
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ChatEvent::TextDelta { .. }
    ));
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::StreamInterrupted);
    let partial = error.partial.unwrap();
    assert!(matches!(&partial.content[0],ContentBlock::Text{text,..} if text=="partial"));
    assert!(stream.next().await.is_none());
}
#[tokio::test]
async fn incomplete_tool_json_returns_partial_arguments() {
    let server = MockServer::start().await;
    mount_sse(&server,"/responses",sse(&[json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"weather"}}),json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"city\":"}),json!({"type":"response.completed","response":{"status":"completed"}})])).await;
    let error = client(config(&server, "openai", Some("responses")))
        .chat_stream(ChatRequest::new("openai/test-model").user("x").tool(call()))
        .await
        .unwrap()
        .collect_response()
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::StreamInterrupted);
    assert_eq!(
        error.partial.unwrap().pending_tool_arguments[&0],
        "{\"city\":"
    );
}
#[tokio::test]
async fn sse_chunk_and_utf8_boundaries_are_supported() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bytes = format!(
        "{}data: [DONE]\r\n\r\n",
        sse(&[
            json!({"choices":[{"index":0,"delta":{"content":"日本語"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        ])
    )
    .into_bytes();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        let read = socket.read(&mut buf).await.unwrap();
        assert!(read > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
        for byte in bytes {
            socket.write_all(b"1\r\n").await.unwrap();
            socket.write_all(&[byte]).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
        }
        socket.write_all(b"0\r\n\r\n").await.ok();
    });
    let mut config = Config::default();
    let p = config.providers.get_mut("openai").unwrap();
    p.base_url = Some(format!("http://{address}"));
    p.api = Some("chat_completions".into());
    let response = client(config)
        .chat_stream(ChatRequest::new("openai/test-model").user("x"))
        .await
        .unwrap()
        .collect_response()
        .await;
    task.await.unwrap();
    assert_eq!(response.unwrap().output_text(), "日本語");
}
#[tokio::test]
async fn oversized_unterminated_sse_is_rejected() {
    let server = MockServer::start().await;
    mount_sse(&server, "/responses", format!("data: {}", "x".repeat(100))).await;
    let mut config = config(&server, "openai", Some("responses"));
    config.http.max_sse_event_bytes = 32;
    config.retry.max_attempts = 1;
    let result = client(config)
        .chat_stream(ChatRequest::new("openai/test-model").user("x"))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn stream_accumulation_limit_returns_only_previously_emitted_content() {
    let server = MockServer::start().await;
    mount_sse(&server,"/responses",sse(&[json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"abc"}),json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"def"}),json!({"type":"response.completed","response":{"status":"completed"}})])).await;
    let mut settings = config(&server, "openai", Some("responses"));
    settings.http.max_response_bytes = 4;
    let error = client(settings)
        .chat_stream(ChatRequest::new("openai/model").user("x"))
        .await
        .unwrap()
        .collect_response()
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::StreamInterrupted);
    assert!(
        matches!(&error.partial.unwrap().content[0],ContentBlock::Text{text,..} if text=="abc")
    );
}

#[tokio::test]
async fn partial_tool_arguments_are_redacted_in_debug() {
    let model = ModelId::parse("stub/model").unwrap();
    let stream = ChatStream::new(
        model,
        "stub",
        futures_util::stream::iter(vec![
            Ok(ChatEvent::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "f".into(),
            }),
            Ok(ChatEvent::ToolCallDelta {
                index: 0,
                delta: "sensitive-argument".into(),
            }),
        ]),
    );
    let error = stream.collect_response().await.unwrap_err();
    assert!(!format!("{:?}", error.partial).contains("sensitive-argument"));
}
