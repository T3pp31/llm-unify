use crate::{
    Capabilities, ChatEvent, ChatRequest, ChatResponse, ChatStream, Config, ContentBlock, Error,
    ErrorKind, ImageContent, Provider, ProviderConfig, ProviderMetadata, ReasoningEffort, Role,
    TokenUsage, ToolCall,
};
use async_trait::async_trait;
use aws_sdk_bedrockruntime::{Client, error::ProvideErrorMetadata, types as aws};
use aws_smithy_types::{Blob, Document, Number};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

pub(super) struct BedrockProvider {
    name: String,
    config: ProviderConfig,
    settings: Arc<Config>,
    client: tokio::sync::OnceCell<Client>,
}
impl BedrockProvider {
    pub fn new(name: String, config: ProviderConfig, settings: Arc<Config>) -> Self {
        Self {
            name,
            config,
            settings,
            client: tokio::sync::OnceCell::new(),
        }
    }
    async fn client(&self) -> &Client {
        self.client
            .get_or_init(|| async {
                let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
                if let Some(region) = &self.config.region {
                    loader = loader.region(aws_config::Region::new(region.clone()));
                }
                let sdk = loader.load().await;
                let mut builder = aws_sdk_bedrockruntime::config::Builder::from(&sdk)
                    .retry_config(aws_config::retry::RetryConfig::disabled())
                    .timeout_config(
                        aws_sdk_bedrockruntime::config::timeout::TimeoutConfig::builder()
                            .operation_timeout(Duration::from_millis(
                                self.settings.http.request_timeout_ms,
                            ))
                            .build(),
                    );
                if let Some(endpoint) = &self.config.base_url {
                    builder = builder.endpoint_url(endpoint);
                }
                Client::from_conf(builder.build())
            })
            .await
    }
    fn encode(&self, r: &ChatRequest) -> Result<Encoded, Error> {
        let mut system = vec![];
        let mut messages: Vec<aws::Message> = vec![];
        for m in &r.messages {
            let mut content = vec![];
            for block in &m.content {
                for meta in block.metadata() {
                    validate_meta(meta)?;
                }
                if m.role == Role::System {
                    if let ContentBlock::Text { text, .. } = block {
                        system.push(aws::SystemContentBlock::Text(text.clone()));
                        continue;
                    } else {
                        return Err(Error::unsupported("non-text system content"));
                    }
                }
                let value = match block {
                    ContentBlock::Text { text, metadata } => {
                        if !metadata.is_empty() {
                            return Err(Error::unsupported("native text metadata"));
                        }
                        aws::ContentBlock::Text(text.clone())
                    }
                    ContentBlock::ToolCall(call) => aws::ContentBlock::ToolUse(
                        aws::ToolUseBlock::builder()
                            .tool_use_id(&call.id)
                            .name(&call.name)
                            .input(to_doc(&call.arguments))
                            .build()
                            .map_err(|_| Error::protocol())?,
                    ),
                    ContentBlock::ToolResult(result) => aws::ContentBlock::ToolResult(
                        aws::ToolResultBlock::builder()
                            .tool_use_id(&result.call_id)
                            .content(aws::ToolResultContentBlock::Json(to_doc(&result.content)))
                            .status(if result.is_error {
                                aws::ToolResultStatus::Error
                            } else {
                                aws::ToolResultStatus::Success
                            })
                            .build()
                            .map_err(|_| Error::protocol())?,
                    ),
                    ContentBlock::Image(ImageContent::Bytes { mime_type, data }) => {
                        if m.role != Role::User {
                            return Err(Error::unsupported("assistant image input"));
                        }
                        let format = match mime_type.as_str() {
                            "image/png" => aws::ImageFormat::Png,
                            "image/jpeg" => aws::ImageFormat::Jpeg,
                            "image/gif" => aws::ImageFormat::Gif,
                            "image/webp" => aws::ImageFormat::Webp,
                            _ => return Err(Error::unsupported("image MIME type")),
                        };
                        aws::ContentBlock::Image(
                            aws::ImageBlock::builder()
                                .format(format)
                                .source(aws::ImageSource::Bytes(Blob::new(data.clone())))
                                .build()
                                .map_err(|_| Error::protocol())?,
                        )
                    }
                    ContentBlock::Image(_) => {
                        return Err(Error::unsupported("Bedrock image URLs; use image bytes"));
                    }
                    ContentBlock::Provider(meta) => {
                        aws::ContentBlock::ReasoningContent(parse_reasoning(&meta.data)?)
                    }
                };
                content.push(value);
            }
            if !content.is_empty() {
                let role = if m.role == Role::Assistant {
                    aws::ConversationRole::Assistant
                } else {
                    aws::ConversationRole::User
                };
                if let Some(last) = messages.last_mut().filter(|m| m.role == role) {
                    last.content.extend(content);
                } else {
                    messages.push(
                        aws::Message::builder()
                            .role(role)
                            .set_content(Some(content))
                            .build()
                            .map_err(|_| Error::protocol())?,
                    );
                }
            }
        }
        let inference = aws::InferenceConfiguration::builder()
            .set_temperature(r.options.temperature.map(|v| v as f32))
            .set_top_p(r.options.top_p.map(|v| v as f32))
            .set_max_tokens(
                r.options
                    .max_output_tokens
                    .map(i32::try_from)
                    .transpose()
                    .map_err(|_| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "max_output_tokens exceeds provider range",
                        )
                    })?,
            )
            .build();
        let tools = if r.tools.is_empty() {
            None
        } else {
            let mut tools = vec![];
            for t in &r.tools {
                tools.push(aws::Tool::ToolSpec(
                    aws::ToolSpecification::builder()
                        .name(&t.name)
                        .description(&t.description)
                        .input_schema(aws::ToolInputSchema::Json(to_doc(&t.parameters)))
                        .build()
                        .map_err(|_| Error::protocol())?,
                ));
            }
            Some(
                aws::ToolConfiguration::builder()
                    .set_tools(Some(tools))
                    .build()
                    .map_err(|_| Error::protocol())?,
            )
        };
        let mut output = aws::OutputConfig::builder();
        if let Some(schema) = &r.options.response_format {
            output = output.text_format(
                aws::OutputFormat::builder()
                    .r#type(aws::OutputFormatType::JsonSchema)
                    .structure(aws::OutputFormatStructure::JsonSchema(
                        aws::JsonSchemaDefinition::builder()
                            .schema(schema.schema.to_string())
                            .name(&schema.name)
                            .build()
                            .map_err(|_| Error::protocol())?,
                    ))
                    .build()
                    .map_err(|_| Error::protocol())?,
            );
        }
        if let Some(effort) = &r.options.reasoning_effort {
            output = output.effort(match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            });
        }
        let output = if r.options.response_format.is_some() || r.options.reasoning_effort.is_some()
        {
            Some(output.build())
        } else {
            None
        };
        let mut native = serde_json::Map::new();
        for options in &r.options.provider_options {
            validate_meta(options)?;
            for (k, v) in options.data.as_object().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    "provider options must be an object",
                )
            })? {
                if [
                    "modelId",
                    "messages",
                    "system",
                    "inferenceConfig",
                    "toolConfig",
                    "outputConfig",
                ]
                .contains(&k.as_str())
                    || native.contains_key(k)
                {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "reserved or duplicate provider option",
                    ));
                }
                native.insert(k.clone(), v.clone());
            }
        }
        Ok(Encoded {
            messages,
            system,
            inference,
            tools,
            output,
            native: if native.is_empty() {
                None
            } else {
                Some(to_doc(&native.into()))
            },
        })
    }
}
struct Encoded {
    messages: Vec<aws::Message>,
    system: Vec<aws::SystemContentBlock>,
    inference: aws::InferenceConfiguration,
    tools: Option<aws::ToolConfiguration>,
    output: Option<aws::OutputConfig>,
    native: Option<Document>,
}
#[async_trait]
impl Provider for BedrockProvider {
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {
            streaming: Some(true),
            tool_calling: Some(true),
            image_input: Some(true),
            structured_output: Some(true),
            reasoning: Some(true),
            ..Capabilities::default()
        }
    }
    async fn chat(&self, r: &ChatRequest) -> Result<ChatResponse, Error> {
        let encoded = self.encode(r)?;
        let result = self
            .client()
            .await
            .converse()
            .model_id(r.model.parts()?.1)
            .set_messages(Some(encoded.messages))
            .set_system(Some(encoded.system))
            .inference_config(encoded.inference)
            .set_tool_config(encoded.tools)
            .set_output_config(encoded.output)
            .set_additional_model_request_fields(encoded.native)
            .send()
            .await
            .map_err(|e| {
                sdk_error(
                    e.raw_response().map(|r| r.status().as_u16()),
                    e.as_service_error().and_then(|e| e.code()),
                    matches!(&e, aws_sdk_bedrockruntime::error::SdkError::TimeoutError(_)),
                    &self.settings,
                )
            })?;
        let message = match result.output.as_ref() {
            Some(aws::ConverseOutput::Message(message)) => message,
            _ => return Err(Error::protocol()),
        };
        let mut content = vec![];
        for b in &message.content {
            content.push(match b {
                aws::ContentBlock::Text(text) => ContentBlock::text(text),
                aws::ContentBlock::ToolUse(call) => ContentBlock::ToolCall(ToolCall::new(
                    &call.tool_use_id,
                    &call.name,
                    from_doc(&call.input)?,
                )),
                aws::ContentBlock::ReasoningContent(reasoning) => {
                    ContentBlock::Provider(meta(reasoning_value(reasoning)?))
                }
                _ => return Err(Error::unsupported("Bedrock output content type")),
            });
        }
        let mut response = super::empty_response(
            r,
            &self.name,
            content,
            result.usage.as_ref().map(usage),
            super::openai::finish(result.stop_reason.as_str()),
        );
        let mut metadata = json!({"stopReason":result.stop_reason.as_str()});
        if let Some(fields) = &result.additional_model_response_fields {
            metadata["additionalModelResponseFields"] = from_doc(fields)?;
        }
        if let Some(metrics) = &result.metrics {
            metadata["metrics"] = json!({"latencyMs":metrics.latency_ms});
        }
        response.provider_metadata.push(meta(metadata));
        Ok(response)
    }
    async fn chat_stream(&self, r: &ChatRequest) -> Result<ChatStream, Error> {
        let encoded = self.encode(r)?;
        let mut result = self
            .client()
            .await
            .converse_stream()
            .model_id(r.model.parts()?.1)
            .set_messages(Some(encoded.messages))
            .set_system(Some(encoded.system))
            .inference_config(encoded.inference)
            .set_tool_config(encoded.tools)
            .set_output_config(encoded.output)
            .set_additional_model_request_fields(encoded.native)
            .send()
            .await
            .map_err(|e| {
                sdk_error(
                    e.raw_response().map(|r| r.status().as_u16()),
                    e.as_service_error().and_then(|e| e.code()),
                    matches!(&e, aws_sdk_bedrockruntime::error::SdkError::TimeoutError(_)),
                    &self.settings,
                )
            })?;
        let idle = Duration::from_millis(self.settings.http.stream_idle_timeout_ms);
        let max = self.settings.http.max_sse_event_bytes;
        let events = async_stream::stream! {
            let mut reason = None;
            let mut reasoning: BTreeMap<usize, Value> = BTreeMap::new();
            loop {
                let event = match tokio::time::timeout(idle, result.stream.recv()).await {
                    Ok(Ok(Some(event))) => event,
                    Ok(Ok(None)) => {
                        if let Some(reason) = reason {
                            yield Ok(ChatEvent::Completed { reason });
                        }
                        return;
                    }
                    _ => {
                        yield Err(Error::transport(true));
                        return;
                    }
                };
                match event {
                    aws::ConverseStreamOutput::MessageStart(_) => {}
                    aws::ConverseStreamOutput::ContentBlockStart(event) => {
                        let index = event.content_block_index as usize;
                        if let Some(aws::ContentBlockStart::ToolUse(call)) = event.start {
                            yield Ok(ChatEvent::ToolCallStart {
                                index,
                                id: call.tool_use_id,
                                name: call.name,
                            });
                        } else {
                            yield Err(Error::unsupported("Bedrock stream block type"));
                            return;
                        }
                    }
                    aws::ConverseStreamOutput::ContentBlockDelta(event) => {
                        let index = event.content_block_index as usize;
                        match event.delta {
                            Some(aws::ContentBlockDelta::Text(delta)) => {
                                yield Ok(ChatEvent::TextDelta { index, delta })
                            }
                            Some(aws::ContentBlockDelta::ToolUse(delta)) => {
                                yield Ok(ChatEvent::ToolCallDelta {
                                    index,
                                    delta: delta.input,
                                })
                            }
                            Some(aws::ContentBlockDelta::ReasoningContent(delta)) => {
                                let raw = reasoning
                                    .entry(index)
                                    .or_insert_with(|| json!({"reasoningText":{"text":"","signature":""}}));
                                let field = match &delta {
                                    aws::ReasoningContentBlockDelta::Text(_) => "text",
                                    aws::ReasoningContentBlockDelta::Signature(_) => "signature",
                                    _ => "",
                                };
                                match delta {
                                    aws::ReasoningContentBlockDelta::Text(text)
                                    | aws::ReasoningContentBlockDelta::Signature(text) => {
                                        let mut value = raw["reasoningText"][field]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_owned();
                                        value.push_str(&text);
                                        raw["reasoningText"][field] = value.into();
                                    }
                                    aws::ReasoningContentBlockDelta::RedactedContent(data) => {
                                        *raw = json!({"redactedContent":base64::engine::general_purpose::STANDARD.encode(data.as_ref())});
                                    }
                                    _ => {
                                        yield Err(Error::protocol());
                                        return;
                                    }
                                }
                                if raw.to_string().len() > max {
                                    yield Err(Error::protocol());
                                    return;
                                }
                            }
                            _ => {
                                yield Err(Error::unsupported("Bedrock stream delta type"));
                                return;
                            }
                        }
                    }
                    aws::ConverseStreamOutput::ContentBlockStop(event) => {
                        let index = event.content_block_index as usize;
                        if let Some(raw) = reasoning.remove(&index) {
                            yield Ok(ChatEvent::Metadata {
                                index,
                                metadata: meta(raw),
                            });
                        }
                    }
                    aws::ConverseStreamOutput::MessageStop(event) => {
                        reason = Some(super::openai::finish(event.stop_reason.as_str()))
                    }
                    aws::ConverseStreamOutput::Metadata(event) => {
                        let mut metadata=json!({});
                        if let Some(metrics)=&event.metrics{metadata["metrics"]=json!({"latencyMs":metrics.latency_ms});}
                        yield Ok(ChatEvent::ResponseMetadata(meta(metadata)));
                        if let Some(u) = event.usage {
                            yield Ok(ChatEvent::Usage(usage(&u)));
                        }
                        if let Some(reason) = reason.take() {
                            yield Ok(ChatEvent::Completed { reason });
                            return;
                        }
                    }
                    _ => {
                        yield Err(Error::protocol());
                        return;
                    }
                }
            }
        };
        Ok(ChatStream::new(r.model.clone(), &self.name, events))
    }
}
fn validate_meta(meta: &ProviderMetadata) -> Result<(), Error> {
    if meta.provider != "bedrock" || meta.format != "converse" {
        Err(Error::unsupported("metadata provider/API mismatch"))
    } else {
        Ok(())
    }
}
fn meta(data: Value) -> ProviderMetadata {
    ProviderMetadata {
        provider: "bedrock".into(),
        format: "converse".into(),
        data,
    }
}
fn reasoning_value(value: &aws::ReasoningContentBlock) -> Result<Value, Error> {
    Ok(match value {
        aws::ReasoningContentBlock::ReasoningText(r) => {
            json!({"reasoningText":{"text":r.text,"signature":r.signature}})
        }
        aws::ReasoningContentBlock::RedactedContent(data) => {
            json!({"redactedContent":base64::engine::general_purpose::STANDARD.encode(data.as_ref())})
        }
        _ => return Err(Error::protocol()),
    })
}
fn parse_reasoning(value: &Value) -> Result<aws::ReasoningContentBlock, Error> {
    if let Some(raw) = value.get("reasoningText") {
        Ok(aws::ReasoningContentBlock::ReasoningText(
            aws::ReasoningTextBlock::builder()
                .text(super::string(raw, "text")?)
                .set_signature(raw["signature"].as_str().map(str::to_owned))
                .build()
                .map_err(|_| Error::protocol())?,
        ))
    } else {
        Ok(aws::ReasoningContentBlock::RedactedContent(Blob::new(
            base64::engine::general_purpose::STANDARD
                .decode(super::string(value, "redactedContent")?)
                .map_err(|_| Error::protocol())?,
        )))
    }
}
fn usage(u: &aws::TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: u.input_tokens.max(0) as u64,
        output_tokens: u.output_tokens.max(0) as u64,
        total_tokens: Some(u.total_tokens.max(0) as u64),
        cached_input_tokens: u.cache_read_input_tokens.map(|n| n.max(0) as u64),
        reasoning_tokens: None,
    }
}
fn sdk_error(status: Option<u16>, code: Option<&str>, timeout: bool, config: &Config) -> Error {
    let kind = match (status, code) {
        (Some(401 | 403), _)
        | (_, Some("AccessDeniedException" | "UnrecognizedClientException")) => {
            ErrorKind::AuthError
        }
        (Some(429), _) | (_, Some("ThrottlingException")) => ErrorKind::RateLimitError,
        (Some(400 | 404 | 422), _)
        | (_, Some("ValidationException" | "ResourceNotFoundException")) => {
            ErrorKind::InvalidRequest
        }
        (None, _) => ErrorKind::TransportError,
        _ => ErrorKind::ProviderError,
    };
    let mut error = Error::new(kind, "Bedrock request failed");
    error.status = status;
    error.retryable = timeout || status.is_some_and(|s| config.retry.statuses.contains(&s));
    error
}
fn to_doc(v: &Value) -> Document {
    match v {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::String(s) => Document::String(s.clone()),
        Value::Array(a) => Document::Array(a.iter().map(to_doc).collect()),
        Value::Object(o) => Document::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), to_doc(v)))
                .collect::<HashMap<_, _>>(),
        ),
        Value::Number(n) => Document::Number(if let Some(v) = n.as_u64() {
            Number::PosInt(v)
        } else if let Some(v) = n.as_i64() {
            Number::NegInt(v)
        } else {
            Number::Float(n.as_f64().expect("JSON number"))
        }),
    }
}
fn from_doc(v: &Document) -> Result<Value, Error> {
    Ok(match v {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::String(s) => Value::String(s.clone()),
        Document::Array(a) => Value::Array(a.iter().map(from_doc).collect::<Result<_, _>>()?),
        Document::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), from_doc(v)?)))
                .collect::<Result<_, Error>>()?,
        ),
        Document::Number(n) => match n {
            Number::PosInt(v) => json!(v),
            Number::NegInt(v) => json!(v),
            Number::Float(v) => {
                if !v.is_finite() {
                    return Err(Error::protocol());
                }
                json!(v)
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, path},
    };
    fn provider(server: &MockServer) -> BedrockProvider {
        let settings = Arc::new(Config::default());
        let mut config = settings.providers["bedrock"].clone();
        config.base_url = Some(server.uri());
        let p = BedrockProvider::new("bedrock".into(), config, settings);
        let sdk = aws_sdk_bedrockruntime::config::Builder::new()
            .behavior_version_latest()
            .region(aws_sdk_bedrockruntime::config::Region::new("test-region"))
            .credentials_provider(aws_sdk_bedrockruntime::config::Credentials::new(
                "test-key",
                "test-secret",
                None,
                None,
                "contract-test",
            ))
            .endpoint_url(server.uri())
            .retry_config(aws_config::retry::RetryConfig::disabled())
            .build();
        p.client.set(Client::from_conf(sdk)).unwrap();
        p
    }
    #[tokio::test]
    async fn converse_signs_and_round_trips_reasoning_and_tools() {
        let server = MockServer::start().await;
        let p = provider(&server);
        Mock::given(path("/model/model/converse")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":{"message":{"role":"assistant","content":[{"reasoningContent":{"reasoningText":{"text":"reason","signature":"sig"}}},{"toolUse":{"toolUseId":"c1","name":"weather","input":{"city":"Tokyo"}}}]}},"stopReason":"tool_use","usage":{"inputTokens":5,"outputTokens":2,"totalTokens":7},"metrics":{"latencyMs":1}}))).expect(1).mount(&server).await;
        let response = p
            .chat(&ChatRequest::new("bedrock/model").user("weather"))
            .await
            .unwrap();
        assert_eq!(response.finish_reason, crate::FinishReason::ToolCalls);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests[0].headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("AWS4-HMAC-SHA256 ")
        );
        server.reset().await;
        Mock::given(path("/model/model/converse")).and(body_partial_json(json!({"messages":[{"role":"user","content":[{"text":"weather"}]},{"role":"assistant","content":[{"reasoningContent":{"reasoningText":{"text":"reason","signature":"sig"}}},{"toolUse":{"toolUseId":"c1","name":"weather","input":{"city":"Tokyo"}}}]},{"role":"user","content":[{"toolResult":{"toolUseId":"c1","content":[{"json":"sunny"}],"status":"success"}}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":{"message":{"role":"assistant","content":[{"text":"sunny"}]}},"stopReason":"end_turn","usage":{"inputTokens":5,"outputTokens":2,"totalTokens":7},"metrics":{"latencyMs":1}}))).expect(1).mount(&server).await;
        let request = ChatRequest::new("bedrock/model")
            .user("weather")
            .message(response.into_message())
            .message(crate::Message::tool(crate::ToolResult::new(
                "c1",
                json!("sunny"),
            )));
        assert_eq!(p.chat(&request).await.unwrap().output_text(), "sunny");
    }
    #[tokio::test]
    async fn converse_stream_decodes_binary_eventstream() {
        use aws_smithy_types::event_stream::{Header, HeaderValue, Message};
        let server = MockServer::start().await;
        let p = provider(&server);
        let mut body = vec![];
        for (name, payload) in [
            ("messageStart", json!({"role":"assistant"})),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex":0,"delta":{"text":"hello"}}),
            ),
            ("contentBlockStop", json!({"contentBlockIndex":0})),
            ("messageStop", json!({"stopReason":"end_turn"})),
            (
                "metadata",
                json!({"usage":{"inputTokens":5,"outputTokens":2,"totalTokens":7},"metrics":{"latencyMs":1}}),
            ),
        ] {
            let message = Message::new(payload.to_string().into_bytes())
                .add_header(Header::new(
                    ":message-type",
                    HeaderValue::String("event".into()),
                ))
                .add_header(Header::new(":event-type", HeaderValue::String(name.into())))
                .add_header(Header::new(
                    ":content-type",
                    HeaderValue::String("application/json".into()),
                ));
            aws_smithy_eventstream::frame::write_message_to(&message, &mut body).unwrap();
        }
        Mock::given(path("/model/model/converse-stream"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body, "application/vnd.amazon.eventstream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let response = p
            .chat_stream(&ChatRequest::new("bedrock/model").user("hello"))
            .await
            .unwrap()
            .collect_response()
            .await
            .unwrap();
        assert_eq!(response.output_text(), "hello");
        assert_eq!(response.usage.unwrap().total_tokens, Some(7));
    }
    #[tokio::test]
    async fn converse_does_not_retry_inside_aws_sdk() {
        let server = MockServer::start().await;
        let p = provider(&server);
        Mock::given(path("/model/model/converse"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("x-amzn-errortype", "ThrottlingException")
                    .set_body_json(json!({"message":"throttled"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = p
            .chat(&ChatRequest::new("bedrock/model").user("hello"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::RateLimitError);
        assert!(error.retryable);
    }
}
