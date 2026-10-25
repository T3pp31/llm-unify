use super::openai::{Decoder, finish};
use super::{HttpProvider, empty_response, native, string, text_value};
use crate::{
    ChatEvent, ChatRequest, ChatResponse, ContentBlock, Error, ImageContent, Role, TokenUsage,
    ToolCall,
};
use base64::Engine;
use serde_json::{Value, json};

pub(super) fn encode(p: &HttpProvider, r: &ChatRequest, stream: bool) -> Result<Value, Error> {
    p.check_metadata(r)?;
    let mut system = vec![];
    let mut messages: Vec<Value> = vec![];
    for m in &r.messages {
        let mut content = vec![];
        for block in &m.content {
            let value = match block {
                ContentBlock::Text { text, .. } => {
                    let mut b = native(block, p).unwrap_or_else(|| json!({"type":"text"}));
                    b["text"] = text.clone().into();
                    b
                }
                ContentBlock::ToolCall(call) => {
                    let mut b = native(block, p).unwrap_or_else(|| json!({"type":"tool_use"}));
                    b["id"] = call.id.clone().into();
                    b["name"] = call.name.clone().into();
                    b["input"] = call.arguments.clone();
                    b
                }
                ContentBlock::ToolResult(result) => {
                    json!({"type":"tool_result","tool_use_id":result.call_id,"content":text_value(&result.content),"is_error":result.is_error})
                }
                ContentBlock::Provider(metadata) => metadata.data.clone(),
                ContentBlock::Image(image) => {
                    json!({"type":"image","source":match image{ImageContent::Url{url}=>json!({"type":"url","url":url}),ImageContent::Bytes{mime_type,data}=>json!({"type":"base64","media_type":mime_type,"data":base64::engine::general_purpose::STANDARD.encode(data)})}})
                }
            };
            if m.role == Role::System && !matches!(block, ContentBlock::Text { .. }) {
                return Err(Error::unsupported("non-text system content"));
            }
            content.push(value);
        }
        if m.role == Role::System {
            system.extend(content);
        } else {
            let role = if m.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            if let Some(last) = messages.last_mut().filter(|v| v["role"] == role) {
                last["content"]
                    .as_array_mut()
                    .ok_or_else(Error::protocol)?
                    .extend(content)
            } else {
                messages.push(json!({"role":role,"content":content}));
            }
        }
    }
    let max = r
        .options
        .max_output_tokens
        .or(p.config.default_max_output_tokens)
        .ok_or_else(crate::config::configuration)?;
    let mut body =
        json!({"model":r.model.parts()?.1,"messages":messages,"max_tokens":max,"stream":stream});
    if !system.is_empty() {
        body["system"] = system.into();
    }
    if let Some(t) = r.options.temperature {
        body["temperature"] = json!(t)
    }
    if let Some(t) = r.options.top_p {
        body["top_p"] = json!(t)
    }
    if !r.tools.is_empty() {
        body["tools"] = r
            .tools
            .iter()
            .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters}))
            .collect();
    }
    let mut output = serde_json::Map::new();
    if let Some(s) = &r.options.response_format {
        output.insert(
            "format".into(),
            json!({"type":"json_schema","schema":s.schema}),
        );
    }
    if let Some(effort) = &r.options.reasoning_effort {
        output.insert("effort".into(), json!(effort));
    }
    if !output.is_empty() {
        body["output_config"] = output.into();
    }
    p.native_options(
        r,
        &mut body,
        &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "stream",
            "tools",
            "temperature",
            "top_p",
            "output_config",
        ],
    )?;
    Ok(body)
}
fn usage(v: &Value) -> Option<TokenUsage> {
    let input = v["input_tokens"].as_u64()?;
    let output = v["output_tokens"].as_u64().unwrap_or(0);
    let cached = v["cache_read_input_tokens"].as_u64();
    let input =
        input + cached.unwrap_or(0) + v["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: Some(input + output),
        cached_input_tokens: cached,
        reasoning_tokens: None,
    })
}
pub(super) fn decode(p: &HttpProvider, r: &ChatRequest, v: &Value) -> Result<ChatResponse, Error> {
    let mut content = vec![];
    for b in v["content"].as_array().ok_or_else(Error::protocol)? {
        match b["type"].as_str() {
            Some("text") => {
                let mut metadata = vec![];
                if b.as_object()
                    .is_some_and(|m| m.keys().any(|k| k != "type" && k != "text"))
                {
                    metadata.push(p.metadata(b.clone()));
                }
                content.push(ContentBlock::Text {
                    text: string(b, "text")?.into(),
                    metadata,
                });
            }
            Some("tool_use") => {
                if !b["input"].is_object() {
                    return Err(Error::protocol());
                }
                let mut c = ToolCall::new(string(b, "id")?, string(b, "name")?, b["input"].clone());
                if b.as_object().is_some_and(|m| {
                    m.keys()
                        .any(|k| !["type", "id", "name", "input"].contains(&k.as_str()))
                }) {
                    c.metadata.push(p.metadata(b.clone()));
                }
                content.push(ContentBlock::ToolCall(c));
            }
            Some(_) => content.push(ContentBlock::Provider(p.metadata(b.clone()))),
            None => return Err(Error::protocol()),
        }
    }
    Ok(empty_response(
        r,
        &p.name,
        content,
        usage(&v["usage"]),
        finish(string(v, "stop_reason")?),
    ))
}
pub(super) fn stream(d: &mut Decoder, event: &str, v: &Value) -> Result<Vec<ChatEvent>, Error> {
    let event = v["type"].as_str().unwrap_or(event);
    let mut out = vec![];
    match event {
        "message_start" => {
            if let Some(u) = usage(&v["message"]["usage"]) {
                d.usage = u;
                out.push(ChatEvent::Usage(d.usage.clone()));
            }
        }
        "content_block_start" => {
            let original = v["index"].as_u64().ok_or_else(Error::protocol)?;
            let index = d.index(format!("block:{original}"));
            let block = &v["content_block"];
            match block["type"].as_str() {
                Some("text") => {
                    out.push(ChatEvent::TextDelta {
                        index,
                        delta: block["text"].as_str().unwrap_or("").into(),
                    });
                }
                Some("tool_use") => {
                    out.push(ChatEvent::ToolCallStart {
                        index,
                        id: string(block, "id")?.into(),
                        name: string(block, "name")?.into(),
                    });
                    if block["input"].as_object().is_some_and(|m| !m.is_empty()) {
                        out.push(ChatEvent::ToolCallDelta {
                            index,
                            delta: block["input"].to_string(),
                        });
                    }
                }
                Some(_) => {}
                None => return Err(Error::protocol()),
            }
            d.native.insert(index, block.clone());
        }
        "content_block_delta" => {
            let index = d.index(format!("block:{}", v["index"]));
            let delta = &v["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    let text = string(delta, "text")?;
                    out.push(ChatEvent::TextDelta {
                        index,
                        delta: text.into(),
                    });
                    d.native_text.entry(index).or_default().push_str(text);
                }
                Some("input_json_delta") => out.push(ChatEvent::ToolCallDelta {
                    index,
                    delta: string(delta, "partial_json")?.into(),
                }),
                Some("thinking_delta") | Some("signature_delta") => {
                    let field = if delta["type"] == "thinking_delta" {
                        "thinking"
                    } else {
                        "signature"
                    };
                    let b = d.native.get_mut(&index).ok_or_else(Error::protocol)?;
                    let mut text = b[field].as_str().unwrap_or("").to_owned();
                    text.push_str(string(delta, field)?);
                    b[field] = text.into();
                }
                Some("citations_delta") => {
                    let b = d.native.get_mut(&index).ok_or_else(Error::protocol)?;
                    if !b["citations"].is_array() {
                        b["citations"] = json!([]);
                    }
                    b["citations"]
                        .as_array_mut()
                        .ok_or_else(Error::protocol)?
                        .push(delta["citation"].clone());
                }
                _ => return Err(Error::protocol()),
            }
        }
        "content_block_stop" => {
            let index = d.index(format!("block:{}", v["index"]));
            if let Some(mut b) = d.native.remove(&index) {
                let emit = match b["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = d.native_text.remove(&index) {
                            b["text"] = text.into();
                        }
                        b.get("citations").is_some()
                    }
                    Some("tool_use") => false,
                    _ => true,
                };
                if emit {
                    out.push(ChatEvent::Metadata {
                        index,
                        metadata: d.meta(b),
                    });
                }
            }
        }
        "message_delta" => {
            if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                d.reason = Some(finish(reason));
            }
            if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                d.usage.output_tokens = n;
                d.usage.total_tokens = Some(d.usage.input_tokens + n);
                out.push(ChatEvent::Usage(d.usage.clone()));
            }
        }
        "message_stop" => out.push(ChatEvent::Completed {
            reason: d.reason.clone().ok_or_else(Error::protocol)?,
        }),
        "ping" => {}
        _ => return Err(Error::protocol()),
    }
    Ok(out)
}
