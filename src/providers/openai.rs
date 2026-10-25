use super::{HttpProvider, empty_response, native, string, text_value};
use crate::{
    ChatEvent, ChatRequest, ChatResponse, ContentBlock, Error, FinishReason, ImageContent,
    ProviderMetadata, Role, TokenUsage, ToolCall,
};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn path(p: &HttpProvider) -> Result<&'static str, Error> {
    match p.format() {
        "responses" => Ok("responses"),
        "chat_completions" => Ok("chat/completions"),
        _ => Err(crate::config::configuration()),
    }
}
pub(super) fn image_url(image: &ImageContent) -> String {
    match image {
        ImageContent::Url { url } => url.clone(),
        ImageContent::Bytes { mime_type, data } => format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(data)
        ),
    }
}
fn role(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
fn message(role: Role, content: Vec<Value>, fields: &serde_json::Map<String, Value>) -> Value {
    let mut value = json!({"role":self::role(role),"content":content});
    value
        .as_object_mut()
        .expect("message object")
        .extend(fields.clone());
    value
}
fn block_metadata(item: &Value, block: &Value) -> Value {
    let mut block = block.clone();
    if item["id"].is_string() && item["status"].is_string() {
        block["__message"] = super::envelope(item);
    }
    block
}
pub(super) fn encode(p: &HttpProvider, r: &ChatRequest, stream: bool) -> Result<Value, Error> {
    p.check_metadata(r)?;
    let responses = p.format() == "responses";
    let mut messages = vec![];
    for m in &r.messages {
        let mut content = vec![];
        let mut calls = vec![];
        let mut message_fields = serde_json::Map::new();
        for block in &m.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    let mut b = native(block, p).unwrap_or_else(|| json!({}));
                    if let Some(header) = b.as_object_mut().and_then(|b| b.remove("__message")) {
                        if !content.is_empty() && message_fields.get("id") != header.get("id") {
                            messages.push(message(
                                m.role,
                                std::mem::take(&mut content),
                                &message_fields,
                            ));
                        }
                        message_fields = header.as_object().ok_or_else(Error::protocol)?.clone();
                        message_fields.remove("role");
                    }
                    if responses && m.role == Role::Assistant && message_fields.contains_key("id") {
                        b["type"] = json!("output_text");
                        if b.get("annotations").is_none() {
                            b["annotations"] = json!([]);
                        }
                    } else {
                        b = json!({"type":if responses{"input_text"}else{"text"}});
                    }
                    b["text"] = text.clone().into();
                    content.push(b);
                }
                ContentBlock::Image(image) => content.push(if responses {
                    json!({"type":"input_image","image_url":image_url(image)})
                } else {
                    json!({"type":"image_url","image_url":{"url":image_url(image)}})
                }),
                ContentBlock::ToolCall(call) => {
                    if responses {
                        if !content.is_empty() {
                            messages.push(message(
                                m.role,
                                std::mem::take(&mut content),
                                &message_fields,
                            ));
                            message_fields.clear();
                        }
                        let mut b = native(block, p).unwrap_or_else(|| json!({}));
                        b["type"] = json!("function_call");
                        b["call_id"] = call.id.clone().into();
                        b["name"] = call.name.clone().into();
                        b["arguments"] = call.arguments.to_string().into();
                        messages.push(b);
                    } else {
                        calls.push(json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}}));
                    }
                }
                ContentBlock::ToolResult(result) => {
                    if responses {
                        messages.push(json!({"type":"function_call_output","call_id":result.call_id,"output":text_value(&result.content)}))
                    } else {
                        messages.push(json!({"role":"tool","tool_call_id":result.call_id,"content":text_value(&result.content)}))
                    }
                }
                ContentBlock::Provider(meta) => {
                    if responses && meta.data["type"] == "refusal" {
                        let mut b = meta.data.clone();
                        let header = b
                            .as_object_mut()
                            .and_then(|b| b.remove("__message"))
                            .ok_or_else(|| {
                                Error::new(
                                    crate::ErrorKind::InvalidRequest,
                                    "native refusal is missing its message envelope",
                                )
                            })?;
                        if !content.is_empty() && message_fields.get("id") != header.get("id") {
                            messages.push(message(
                                m.role,
                                std::mem::take(&mut content),
                                &message_fields,
                            ));
                        }
                        message_fields = header.as_object().ok_or_else(Error::protocol)?.clone();
                        message_fields.remove("role");
                        content.push(b);
                    } else if responses {
                        if !content.is_empty() {
                            messages.push(message(
                                m.role,
                                std::mem::take(&mut content),
                                &message_fields,
                            ));
                            message_fields.clear();
                        }
                        messages.push(meta.data.clone())
                    } else if let Some(fields) = meta.data["message_fields"].as_object() {
                        if fields
                            .keys()
                            .any(|k| ["role", "content", "tool_calls"].contains(&k.as_str()))
                        {
                            return Err(Error::unsupported("native reserved message fields"));
                        }
                        message_fields.extend(fields.clone());
                    } else if meta.data["refusal"].is_string() {
                        message_fields.insert("refusal".into(), meta.data["refusal"].clone());
                    } else {
                        return Err(Error::unsupported("native message block"));
                    }
                }
            }
        }
        if !content.is_empty() || !calls.is_empty() || !message_fields.is_empty() {
            let mut value = message(m.role, content, &message_fields);
            if !calls.is_empty() {
                value["tool_calls"] = calls.into();
            }
            messages.push(value);
        }
    }
    let mut body = json!({"model":r.model.parts()?.1,"stream":stream});
    body[if responses { "input" } else { "messages" }] = messages.into();
    if responses {
        body["store"] = false.into();
        body["include"] = json!(["reasoning.encrypted_content"]);
    } else if stream {
        body["stream_options"] = json!({"include_usage":true});
    }
    if let Some(value) = r.options.temperature {
        body["temperature"] = json!(value)
    }
    if let Some(value) = r.options.top_p {
        body["top_p"] = json!(value)
    }
    if let Some(value) = r.options.max_output_tokens {
        body[if responses {
            "max_output_tokens"
        } else {
            "max_completion_tokens"
        }] = value.into();
    }
    if !r.tools.is_empty() {
        body["tools"]=r.tools.iter().map(|t|if responses{json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters})}else{json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}})}).collect();
    }
    if let Some(schema) = &r.options.response_format {
        let schema = json!({"type":"json_schema","name":schema.name,"schema":schema.schema,"strict":schema.strict});
        if responses {
            body["text"] = json!({"format":schema});
        } else {
            let mut definition = schema;
            definition
                .as_object_mut()
                .ok_or_else(Error::protocol)?
                .remove("type");
            body["response_format"] = json!({"type":"json_schema","json_schema":definition});
        }
    }
    if let Some(effort) = &r.options.reasoning_effort {
        if responses {
            body["reasoning"] = json!({"effort":effort});
        } else {
            body["reasoning_effort"] = json!(effort);
        }
    }
    p.native_options(
        r,
        &mut body,
        &[
            "model",
            "input",
            "messages",
            "stream",
            "store",
            "include",
            "tools",
            "temperature",
            "top_p",
            "max_output_tokens",
            "max_tokens",
            "max_completion_tokens",
            "response_format",
            "text",
            "reasoning",
            "reasoning_effort",
            "previous_response_id",
            "conversation",
            "background",
            "stream_options",
            "n",
        ],
    )?;
    Ok(body)
}
pub(super) fn usage(v: Option<&Value>, responses: bool) -> Option<TokenUsage> {
    let v = v.filter(|v| v.is_object())?;
    let input = v[if responses {
        "input_tokens"
    } else {
        "prompt_tokens"
    }]
    .as_u64()?;
    let output = v[if responses {
        "output_tokens"
    } else {
        "completion_tokens"
    }]
    .as_u64()
    .unwrap_or(0);
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: v["total_tokens"].as_u64(),
        cached_input_tokens: v[if responses {
            "input_tokens_details"
        } else {
            "prompt_tokens_details"
        }]["cached_tokens"]
            .as_u64(),
        reasoning_tokens: v[if responses {
            "output_tokens_details"
        } else {
            "completion_tokens_details"
        }]["reasoning_tokens"]
            .as_u64(),
    })
}
pub(super) fn finish(value: &str) -> FinishReason {
    match value {
        "stop" | "end_turn" | "stop_sequence" | "completed" | "STOP" => FinishReason::Stop,
        "length" | "max_tokens" | "max_output_tokens" | "MAX_TOKENS" => FinishReason::Length,
        "tool_calls" | "tool_use" | "requires_action" => FinishReason::ToolCalls,
        "content_filter" | "refusal" | "SAFETY" | "RECITATION" => FinishReason::ContentFilter,
        other => FinishReason::Unknown(other.into()),
    }
}
pub(super) fn decode(p: &HttpProvider, r: &ChatRequest, v: &Value) -> Result<ChatResponse, Error> {
    if v.get("error").is_some_and(|error| !error.is_null()) {
        return Err(Error::protocol());
    }
    let mut content = vec![];
    let reason = if p.format() == "responses" {
        for item in v["output"].as_array().ok_or_else(Error::protocol)? {
            match item["type"].as_str() {
                Some("message") => {
                    for b in item["content"].as_array().ok_or_else(Error::protocol)? {
                        if b["type"] == "output_text" {
                            let mut metadata = vec![];
                            let raw = block_metadata(item, b);
                            if raw.get("__message").is_some()
                                || extra_fields(b, &["type", "text", "annotations", "logprobs"])
                            {
                                metadata.push(p.metadata(raw));
                            }
                            content.push(ContentBlock::Text {
                                text: string(b, "text")?.into(),
                                metadata,
                            });
                        } else {
                            content
                                .push(ContentBlock::Provider(p.metadata(block_metadata(item, b))));
                        }
                    }
                }
                Some("function_call") => {
                    let arguments: Value = serde_json::from_str(string(item, "arguments")?)
                        .map_err(|_| Error::protocol())?;
                    if !arguments.is_object() {
                        return Err(Error::protocol());
                    }
                    let mut call =
                        ToolCall::new(string(item, "call_id")?, string(item, "name")?, arguments);
                    call.metadata.push(p.metadata(item.clone()));
                    content.push(ContentBlock::ToolCall(call));
                }
                Some(_) => content.push(ContentBlock::Provider(p.metadata(item.clone()))),
                _ => return Err(Error::protocol()),
            }
        }
        match v["status"].as_str() {
            Some("completed") => {
                if content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolCall(_)))
                {
                    FinishReason::ToolCalls
                } else if content
                    .iter()
                    .any(|b| matches!(b,ContentBlock::Provider(m) if m.data["type"]=="refusal"))
                {
                    FinishReason::ContentFilter
                } else {
                    FinishReason::Stop
                }
            }
            Some("incomplete") => finish(
                v["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or("incomplete"),
            ),
            _ => return Err(Error::protocol()),
        }
    } else {
        let choices = v["choices"]
            .as_array()
            .filter(|c| c.len() == 1)
            .ok_or_else(Error::protocol)?;
        let choice = &choices[0];
        let message = &choice["message"];
        if let Some(text) = message["content"].as_str() {
            content.push(ContentBlock::text(text));
        }
        if let Some(refusal) = message["refusal"].as_str() {
            content.push(ContentBlock::Provider(
                p.metadata(json!({"refusal":refusal})),
            ));
        }
        if let Some(fields) = message.as_object() {
            let fields: serde_json::Map<String, Value> = fields
                .iter()
                .filter(|(k, v)| {
                    !["role", "content", "tool_calls", "refusal"].contains(&k.as_str())
                        && !v.is_null()
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !fields.is_empty() {
                content.push(ContentBlock::Provider(
                    p.metadata(json!({"message_fields":fields})),
                ));
            }
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for c in calls {
                let arguments: Value = serde_json::from_str(string(&c["function"], "arguments")?)
                    .map_err(|_| Error::protocol())?;
                if !arguments.is_object() {
                    return Err(Error::protocol());
                }
                content.push(ContentBlock::ToolCall(ToolCall::new(
                    string(c, "id")?,
                    string(&c["function"], "name")?,
                    arguments,
                )));
            }
        }
        if message["refusal"].is_string() {
            FinishReason::ContentFilter
        } else {
            finish(string(choice, "finish_reason")?)
        }
    };
    Ok(empty_response(
        r,
        &p.name,
        content,
        usage(v.get("usage"), p.format() == "responses"),
        reason,
    ))
}
fn extra_fields(v: &Value, known: &[&str]) -> bool {
    v.as_object().is_some_and(|m| {
        m.iter().any(|(k, v)| {
            !known.contains(&k.as_str()) && !v.is_null()
                || matches!(k.as_str(), "annotations" | "logprobs")
                    && v.as_array().is_some_and(|a| !a.is_empty())
        })
    })
}

pub(super) struct Decoder {
    pub kind: String,
    pub format: String,
    pub identity: String,
    pub reason: Option<FinishReason>,
    pub usage: TokenUsage,
    pub indices: BTreeMap<String, usize>,
    pub started: BTreeSet<usize>,
    pub next_index: usize,
    pub native: BTreeMap<usize, Value>,
    pub native_text: BTreeMap<usize, String>,
    response_metadata: Value,
}
impl Decoder {
    pub fn new(kind: String, format: String, identity: String) -> Self {
        Self {
            kind,
            format,
            identity,
            reason: None,
            usage: TokenUsage::default(),
            indices: BTreeMap::new(),
            started: BTreeSet::new(),
            next_index: 0,
            native: BTreeMap::new(),
            native_text: BTreeMap::new(),
            response_metadata: json!({}),
        }
    }
    pub fn index(&mut self, key: String) -> usize {
        if let Some(index) = self.indices.get(&key) {
            *index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.indices.insert(key, index);
            index
        }
    }
    pub fn meta(&self, data: Value) -> ProviderMetadata {
        ProviderMetadata {
            provider: self.identity.clone(),
            format: self.format.clone(),
            data,
        }
    }
    pub fn decode(&mut self, event: &str, v: &Value) -> Result<Vec<ChatEvent>, Error> {
        if event == "error"
            || v.get("error").is_some_and(|error| !error.is_null())
            || v["event_type"] == "error"
            || v["type"] == "error"
        {
            return Err(Error::protocol());
        }
        let mut events = match self.kind.as_str() {
            "anthropic" => super::anthropic::stream(self, event, v),
            "gemini" => super::gemini::stream(self, event, v),
            "vertex" => super::gemini::stream_vertex(self, v),
            _ => self.openai(event, v),
        }?;
        let response = match self.kind.as_str() {
            "anthropic" => {
                if v["type"] == "message_start" {
                    self.response_metadata = super::envelope(&v["message"]);
                } else if v["type"] == "message_delta" {
                    if let (Some(target), Some(delta)) = (
                        self.response_metadata.as_object_mut(),
                        v["delta"].as_object(),
                    ) {
                        target.extend(delta.clone());
                    }
                    if let Some(usage) = v.get("usage") {
                        self.response_metadata["usage"] = usage.clone();
                    }
                }
                self.response_metadata.clone()
            }
            "gemini" => v
                .get("interaction")
                .map(super::envelope)
                .unwrap_or_else(|| json!({})),
            "vertex" => super::envelope(v),
            _ if self.format == "responses" => v
                .get("response")
                .map(super::envelope)
                .unwrap_or_else(|| json!({})),
            _ => {
                if v.is_object() {
                    self.response_metadata = super::envelope(v);
                }
                self.response_metadata.clone()
            }
        };
        if let Some(position) = events
            .iter()
            .position(|e| matches!(e, ChatEvent::Completed { .. }))
        {
            events.insert(position, ChatEvent::ResponseMetadata(self.meta(response)));
        }
        Ok(events)
    }
    fn openai(&mut self, _event: &str, v: &Value) -> Result<Vec<ChatEvent>, Error> {
        let mut out = vec![];
        if self.format == "chat_completions" {
            if v == "[DONE]" {
                out.push(ChatEvent::Completed {
                    reason: self.reason.clone().ok_or_else(Error::protocol)?,
                });
                return Ok(out);
            }
            for choice in v["choices"].as_array().ok_or_else(Error::protocol)? {
                if choice["index"].as_u64().unwrap_or(0) != 0 {
                    return Err(Error::unsupported("multiple streaming choices"));
                }
                if let Some(text) = choice["delta"]["content"].as_str() {
                    let index = self.index("text".into());
                    out.push(ChatEvent::TextDelta {
                        index,
                        delta: text.into(),
                    });
                }
                if let Some(refusal) = choice["delta"]["refusal"].as_str() {
                    let index = self.index("refusal".into());
                    self.native_text.entry(index).or_default().push_str(refusal);
                }
                if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
                    for call in calls {
                        let original = call["index"].as_u64().ok_or_else(Error::protocol)?;
                        let index = self.index(format!("tool:{original}"));
                        if !self.started.contains(&index) {
                            out.push(ChatEvent::ToolCallStart {
                                index,
                                id: string(call, "id")?.into(),
                                name: string(&call["function"], "name")?.into(),
                            });
                            self.started.insert(index);
                        }
                        if let Some(delta) = call["function"]["arguments"].as_str() {
                            out.push(ChatEvent::ToolCallDelta {
                                index,
                                delta: delta.into(),
                            });
                        }
                    }
                }
                if let Some(reason) = choice["finish_reason"].as_str() {
                    self.reason = Some(finish(reason));
                }
            }
            if let Some(usage) = usage(v.get("usage"), false) {
                out.push(ChatEvent::Usage(usage));
            }
            // Refusal fragments are native data rather than display text.
            if self.reason.is_some() {
                for (index, text) in std::mem::take(&mut self.native_text) {
                    out.push(ChatEvent::Metadata {
                        index,
                        metadata: self.meta(json!({"refusal":text})),
                    });
                }
            }
            return Ok(out);
        }
        let event = v["type"].as_str().ok_or_else(Error::protocol)?;
        match event {
            "response.output_item.added" => {
                let item = &v["item"];
                if item["type"] == "function_call" {
                    let index = self.index(format!("item:{}", v["output_index"]));
                    out.push(ChatEvent::ToolCallStart {
                        index,
                        id: string(item, "call_id")?.into(),
                        name: string(item, "name")?.into(),
                    });
                    self.started.insert(index);
                }
            }
            "response.output_text.delta" => {
                let index =
                    self.index(format!("text:{}:{}", v["output_index"], v["content_index"]));
                out.push(ChatEvent::TextDelta {
                    index,
                    delta: string(v, "delta")?.into(),
                });
            }
            "response.refusal.delta" => {
                self.reason = Some(FinishReason::ContentFilter);
                let index = self.index(format!(
                    "refusal:{}:{}",
                    v["output_index"], v["content_index"]
                ));
                self.native_text
                    .entry(index)
                    .or_default()
                    .push_str(string(v, "delta")?);
            }
            "response.function_call_arguments.delta" => {
                let index = self.index(format!("item:{}", v["output_index"]));
                out.push(ChatEvent::ToolCallDelta {
                    index,
                    delta: string(v, "delta")?.into(),
                });
            }
            "response.output_item.done" => {
                let item = &v["item"];
                match item["type"].as_str() {
                    Some("function_call") => {
                        let index = self.index(format!("item:{}", v["output_index"]));
                        out.push(ChatEvent::Metadata {
                            index,
                            metadata: self.meta(item.clone()),
                        });
                    }
                    Some("message") => {
                        if let Some(blocks) = item["content"].as_array() {
                            for (i, b) in blocks.iter().enumerate() {
                                if b["type"] == "output_text" {
                                    let index =
                                        self.index(format!("text:{}:{i}", v["output_index"]));
                                    out.push(ChatEvent::Metadata {
                                        index,
                                        metadata: self.meta(block_metadata(item, b)),
                                    });
                                } else if b["type"] == "refusal" {
                                    let index =
                                        self.index(format!("refusal:{}:{i}", v["output_index"]));
                                    self.reason = Some(FinishReason::ContentFilter);
                                    self.native.insert(index, block_metadata(item, b));
                                    self.native_text.entry(index).or_insert_with(|| {
                                        b["refusal"].as_str().unwrap_or("").into()
                                    });
                                }
                            }
                        }
                    }
                    Some(_) => {
                        let index = self.index(format!("item:{}", v["output_index"]));
                        out.push(ChatEvent::Metadata {
                            index,
                            metadata: self.meta(item.clone()),
                        });
                    }
                    None => return Err(Error::protocol()),
                }
            }
            "response.completed" | "response.incomplete" => {
                for (index, text) in std::mem::take(&mut self.native_text) {
                    let raw = self
                        .native
                        .remove(&index)
                        .unwrap_or_else(|| json!({"type":"refusal","refusal":text}));
                    out.push(ChatEvent::Metadata {
                        index,
                        metadata: self.meta(raw),
                    });
                }
                if let Some(usage) = usage(v["response"].get("usage"), true) {
                    out.push(ChatEvent::Usage(usage));
                }
                let reason = if event == "response.incomplete" {
                    finish(
                        v["response"]["incomplete_details"]["reason"]
                            .as_str()
                            .unwrap_or("incomplete"),
                    )
                } else if !self.started.is_empty() {
                    FinishReason::ToolCalls
                } else {
                    self.reason.clone().unwrap_or(FinishReason::Stop)
                };
                out.push(ChatEvent::Completed { reason });
            }
            "response.failed" | "response.cancelled" => return Err(Error::protocol()),
            _ => {}
        }
        Ok(out)
    }
}
