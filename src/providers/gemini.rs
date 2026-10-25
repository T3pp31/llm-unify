use super::openai::{Decoder, finish};
use super::{HttpProvider, empty_response, native, string, text_value};
use crate::{
    ChatEvent, ChatRequest, ChatResponse, ContentBlock, Error, FinishReason, ImageContent, Role,
    TokenUsage, ToolCall,
};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn image(image: &ImageContent, vertex: bool) -> Value {
    match image {
        ImageContent::Url { url } => {
            if vertex {
                json!({"fileData":{"fileUri":url}})
            } else {
                json!({"type":"image","uri":url})
            }
        }
        ImageContent::Bytes { mime_type, data } => {
            if vertex {
                json!({"inlineData":{"mimeType":mime_type,"data":base64::engine::general_purpose::STANDARD.encode(data)}})
            } else {
                json!({"type":"image","mime_type":mime_type,"data":base64::engine::general_purpose::STANDARD.encode(data)})
            }
        }
    }
}
pub(super) fn encode(p: &HttpProvider, r: &ChatRequest, stream: bool) -> Result<Value, Error> {
    p.check_metadata(r)?;
    let mut steps = vec![];
    let mut system = vec![];
    let mut call_names = BTreeMap::new();
    for m in &r.messages {
        let mut content = vec![];
        let kind = if m.role == Role::Assistant {
            "model_output"
        } else {
            "user_input"
        };
        for block in &m.content {
            if m.role == Role::System {
                if let ContentBlock::Text { text, .. } = block {
                    system.push(text.clone());
                    continue;
                } else {
                    return Err(Error::unsupported("non-text system content"));
                }
            }
            match block {
                ContentBlock::Text { text, .. } => {
                    let mut value = native(block, p).unwrap_or_else(|| json!({"type":"text"}));
                    value["text"] = text.clone().into();
                    content.push(value)
                }
                ContentBlock::Image(i) => content.push(image(i, false)),
                ContentBlock::ToolCall(call) => {
                    if !content.is_empty() {
                        steps.push(json!({"type":kind,"content":std::mem::take(&mut content)}));
                    }
                    call_names.insert(call.id.clone(), call.name.clone());
                    let mut value =
                        native(block, p).unwrap_or_else(|| json!({"type":"function_call"}));
                    value["id"] = call.id.clone().into();
                    value["name"] = call.name.clone().into();
                    value["arguments"] = call.arguments.clone();
                    steps.push(value)
                }
                ContentBlock::ToolResult(result) => {
                    if !content.is_empty() {
                        steps.push(json!({"type":kind,"content":std::mem::take(&mut content)}));
                    }
                    let name = result
                        .name
                        .as_ref()
                        .or_else(|| call_names.get(&result.call_id))
                        .ok_or_else(Error::protocol)?;
                    steps.push(json!({"type":"function_result","call_id":result.call_id,"name":name,"result":[{"type":"text","text":text_value(&result.content)}],"is_error":result.is_error}));
                }
                ContentBlock::Provider(metadata) => {
                    if !content.is_empty() {
                        steps.push(json!({"type":kind,"content":std::mem::take(&mut content)}));
                    }
                    steps.push(metadata.data.clone());
                }
            }
        }
        if !content.is_empty() {
            steps.push(json!({"type":kind,"content":content}));
        }
    }
    let mut body = json!({"model":r.model.parts()?.1,"input":steps,"stream":stream,"store":false});
    if !system.is_empty() {
        body["system_instruction"] = system.join("\n\n").into();
    }
    let mut generation = serde_json::Map::new();
    if let Some(t) = r.options.temperature {
        generation.insert("temperature".into(), json!(t));
    }
    if let Some(t) = r.options.top_p {
        generation.insert("top_p".into(), json!(t));
    }
    if let Some(t) = r.options.max_output_tokens {
        generation.insert("max_output_tokens".into(), t.into());
    }
    if let Some(e) = &r.options.reasoning_effort {
        generation.insert("thinking_level".into(), json!(e));
    }
    if !generation.is_empty() {
        body["generation_config"] = generation.into();
    }
    if !r.tools.is_empty() {
        body["tools"]=r.tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters})).collect();
    }
    if let Some(s) = &r.options.response_format {
        body["response_format"] =
            json!({"type":"text","mime_type":"application/json","schema":s.schema});
    }
    p.native_options(
        r,
        &mut body,
        &[
            "model",
            "agent",
            "agent_config",
            "input",
            "stream",
            "store",
            "system_instruction",
            "generation_config",
            "tools",
            "response_format",
            "previous_interaction_id",
            "background",
        ],
    )?;
    Ok(body)
}
fn usage(v: &Value) -> Option<TokenUsage> {
    let input = v["total_input_tokens"].as_u64()?;
    let thought = v["total_thought_tokens"].as_u64();
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: v["total_output_tokens"].as_u64().unwrap_or(0) + thought.unwrap_or(0),
        total_tokens: v["total_tokens"].as_u64(),
        cached_input_tokens: v["total_cached_tokens"].as_u64(),
        reasoning_tokens: thought,
    })
}
pub(super) fn decode(p: &HttpProvider, r: &ChatRequest, v: &Value) -> Result<ChatResponse, Error> {
    let mut content = vec![];
    for step in v["steps"].as_array().ok_or_else(Error::protocol)? {
        match step["type"].as_str() {
            Some("model_output") => {
                for b in step["content"].as_array().ok_or_else(Error::protocol)? {
                    if b["type"] == "text" {
                        let metadata = if b
                            .as_object()
                            .is_some_and(|m| m.keys().any(|k| k != "type" && k != "text"))
                        {
                            vec![p.metadata(b.clone())]
                        } else {
                            vec![]
                        };
                        content.push(ContentBlock::Text {
                            text: string(b, "text")?.into(),
                            metadata,
                        });
                    } else {
                        content.push(ContentBlock::Provider(
                            p.metadata(json!({"type":"model_output","content":[b]})),
                        ));
                    }
                }
            }
            Some("function_call") => {
                if !step["arguments"].is_object() {
                    return Err(Error::protocol());
                }
                let mut call = ToolCall::new(
                    string(step, "id")?,
                    string(step, "name")?,
                    step["arguments"].clone(),
                );
                if step.get("signature").is_some() {
                    call.metadata.push(p.metadata(step.clone()));
                }
                content.push(ContentBlock::ToolCall(call));
            }
            Some(_) => content.push(ContentBlock::Provider(p.metadata(step.clone()))),
            None => return Err(Error::protocol()),
        }
    }
    let reason = match v["status"].as_str() {
        Some("completed") => v["stop_reason"]
            .as_str()
            .map(finish)
            .unwrap_or(FinishReason::Stop),
        Some("requires_action") => FinishReason::ToolCalls,
        Some("incomplete") => FinishReason::Unknown("incomplete".into()),
        _ => return Err(Error::protocol()),
    };
    Ok(empty_response(
        r,
        &p.name,
        content,
        usage(&v["usage"]),
        reason,
    ))
}
pub(super) fn stream(d: &mut Decoder, _event: &str, v: &Value) -> Result<Vec<ChatEvent>, Error> {
    let mut out = vec![];
    match string(v, "event_type")? {
        "interaction.created" | "interaction.status_update" => {}
        "step.start" => {
            let index = d.index(format!("step:{}", v["index"]));
            let step = &v["step"];
            if step["type"] == "function_call" {
                out.push(ChatEvent::ToolCallStart {
                    index,
                    id: string(step, "id")?.into(),
                    name: string(step, "name")?.into(),
                });
                d.started.insert(index);
                if step["arguments"].as_object().is_some_and(|m| !m.is_empty()) {
                    out.push(ChatEvent::ToolCallDelta {
                        index,
                        delta: step["arguments"].to_string(),
                    });
                }
            }
            d.native.insert(index, step.clone());
        }
        "step.delta" => {
            let index = d.index(format!("step:{}", v["index"]));
            let delta = &v["delta"];
            match delta["type"].as_str() {
                Some("text") => out.push(ChatEvent::TextDelta {
                    index,
                    delta: string(delta, "text")?.into(),
                }),
                Some("arguments_delta") => out.push(ChatEvent::ToolCallDelta {
                    index,
                    delta: string(delta, "arguments")?.into(),
                }),
                Some("thought_signature") => {
                    let raw = d.native.get_mut(&index).ok_or_else(Error::protocol)?;
                    let mut signature = raw["signature"].as_str().unwrap_or("").to_owned();
                    signature.push_str(string(delta, "signature")?);
                    raw["signature"] = signature.into();
                }
                Some("thought_summary") => {
                    let raw = d.native.get_mut(&index).ok_or_else(Error::protocol)?;
                    if !raw["summary"].is_array() {
                        raw["summary"] = json!([])
                    }
                    raw["summary"]
                        .as_array_mut()
                        .ok_or_else(Error::protocol)?
                        .push(delta["content"].clone());
                }
                Some("function_call") => {
                    if !d.started.contains(&index) {
                        out.push(ChatEvent::ToolCallStart {
                            index,
                            id: string(delta, "id")?.into(),
                            name: string(delta, "name")?.into(),
                        });
                        d.started.insert(index);
                    }
                    if let Some(args) = delta.get("arguments") {
                        out.push(ChatEvent::ToolCallDelta {
                            index,
                            delta: args
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| args.to_string()),
                        });
                    }
                    if let Some(sig) = delta.get("signature") {
                        d.native.get_mut(&index).ok_or_else(Error::protocol)?["signature"] =
                            sig.clone();
                    }
                }
                _ => return Err(Error::unsupported("interaction stream content type")),
            }
        }
        "step.stop" => {
            let index = d.index(format!("step:{}", v["index"]));
            if let Some(raw) = d.native.remove(&index)
                && raw["type"] != "model_output"
                && (raw["type"] != "function_call" || raw.get("signature").is_some())
            {
                out.push(ChatEvent::Metadata {
                    index,
                    metadata: d.meta(raw),
                });
            }
        }
        "interaction.completed" | "interaction.incomplete" => {
            let interaction = &v["interaction"];
            if let Some(u) = usage(&interaction["usage"]) {
                out.push(ChatEvent::Usage(u));
            }
            let reason = match interaction["status"].as_str() {
                Some("completed") => {
                    if !d.started.is_empty() {
                        FinishReason::ToolCalls
                    } else {
                        FinishReason::Stop
                    }
                }
                Some("requires_action") => FinishReason::ToolCalls,
                Some("incomplete") => FinishReason::Unknown("incomplete".into()),
                _ => return Err(Error::protocol()),
            };
            out.push(ChatEvent::Completed { reason });
        }
        "interaction.failed" | "interaction.cancelled" => return Err(Error::protocol()),
        _ => return Err(Error::protocol()),
    }
    Ok(out)
}

pub(super) fn vertex_path(
    p: &HttpProvider,
    r: &ChatRequest,
    stream: bool,
) -> Result<String, Error> {
    let project = p
        .config
        .project
        .as_deref()
        .ok_or_else(crate::config::configuration)?;
    let location = p
        .config
        .location
        .as_deref()
        .ok_or_else(crate::config::configuration)?;
    let model = r.model.parts()?.1;
    if [project, location, model].iter().any(|v| {
        v.is_empty() || v.contains(['?', '#', '/', '%']) || v.chars().any(char::is_whitespace)
    }) {
        return Err(crate::config::configuration());
    }
    Ok(format!(
        "projects/{project}/locations/{location}/publishers/google/models/{model}:{}",
        if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        }
    ))
}
pub(super) fn encode_vertex(p: &HttpProvider, r: &ChatRequest) -> Result<Value, Error> {
    p.check_metadata(r)?;
    let mut contents: Vec<Value> = vec![];
    let mut system = vec![];
    let mut names = BTreeMap::new();
    for m in &r.messages {
        let mut parts = vec![];
        for block in &m.content {
            let part = match block {
                ContentBlock::Text { text, .. } => {
                    let mut raw = native(block, p).unwrap_or_else(|| json!({}));
                    raw["text"] = text.clone().into();
                    raw
                }
                ContentBlock::Image(i) => image(i, true),
                ContentBlock::ToolCall(call) => {
                    let mut raw = native(block, p).unwrap_or_else(|| json!({}));
                    let wire_id = if raw.get("functionCall").is_some() {
                        raw["functionCall"]["id"].as_str().map(str::to_owned)
                    } else {
                        Some(call.id.clone())
                    };
                    names.insert(call.id.clone(), (call.name.clone(), wire_id.clone()));
                    if !raw["functionCall"].is_object() {
                        raw["functionCall"] = json!({});
                    }
                    raw["functionCall"]["name"] = call.name.clone().into();
                    raw["functionCall"]["args"] = call.arguments.clone();
                    if let Some(id) = wire_id {
                        raw["functionCall"]["id"] = id.into();
                    }
                    raw
                }
                ContentBlock::ToolResult(result) => {
                    let name = result
                        .name
                        .as_ref()
                        .or_else(|| names.get(&result.call_id).map(|(name, _)| name))
                        .ok_or_else(Error::protocol)?;
                    let response = if result.is_error {
                        json!({"error":result.content})
                    } else if result.content.is_object() {
                        result.content.clone()
                    } else {
                        json!({"result":result.content})
                    };
                    let mut part = json!({"functionResponse":{"name":name,"response":response}});
                    if let Some(id) = names.get(&result.call_id).and_then(|(_, id)| id.as_ref()) {
                        part["functionResponse"]["id"] = id.clone().into();
                    }
                    part
                }
                ContentBlock::Provider(metadata) => metadata.data.clone(),
            };
            if m.role == Role::System && !matches!(block, ContentBlock::Text { .. }) {
                return Err(Error::unsupported("non-text system content"));
            }
            parts.push(part);
        }
        if m.role == Role::System {
            system.extend(parts);
        } else {
            let role = if m.role == Role::Assistant {
                "model"
            } else {
                "user"
            };
            if let Some(last) = contents.last_mut().filter(|m| m["role"] == role) {
                last["parts"]
                    .as_array_mut()
                    .ok_or_else(Error::protocol)?
                    .extend(parts);
            } else {
                contents.push(json!({"role":role,"parts":parts}));
            }
        }
    }
    let mut body = json!({"contents":contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts":system});
    }
    let mut generation = json!({});
    if let Some(v) = r.options.temperature {
        generation["temperature"] = json!(v)
    }
    if let Some(v) = r.options.top_p {
        generation["topP"] = json!(v)
    }
    if let Some(v) = r.options.max_output_tokens {
        generation["maxOutputTokens"] = json!(v)
    }
    if let Some(s) = &r.options.response_format {
        generation["responseMimeType"] = json!("application/json");
        generation["responseJsonSchema"] = s.schema.clone();
    }
    if let Some(e) = &r.options.reasoning_effort {
        generation["thinkingConfig"] = json!({"thinkingLevel":match e{crate::ReasoningEffort::Low=>"LOW",crate::ReasoningEffort::Medium=>"MEDIUM",crate::ReasoningEffort::High=>"HIGH"}});
    }
    if generation.as_object().is_some_and(|m| !m.is_empty()) {
        body["generationConfig"] = generation;
    }
    if !r.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations":r.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"parametersJsonSchema":t.parameters})).collect::<Vec<_>>()}]);
    }
    p.native_options(
        r,
        &mut body,
        &["contents", "systemInstruction", "generationConfig", "tools"],
    )?;
    Ok(body)
}
fn vertex_usage(v: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: v["promptTokenCount"].as_u64()?,
        output_tokens: v["candidatesTokenCount"].as_u64().unwrap_or(0)
            + v["thoughtsTokenCount"].as_u64().unwrap_or(0),
        total_tokens: v["totalTokenCount"].as_u64(),
        cached_input_tokens: v["cachedContentTokenCount"].as_u64(),
        reasoning_tokens: v["thoughtsTokenCount"].as_u64(),
    })
}
fn vertex_part(p: &HttpProvider, b: &Value, _index: usize) -> Result<ContentBlock, Error> {
    if let Some(call) = b.get("functionCall") {
        let id = call["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("call_{:032x}", rand::random::<u128>()));
        let mut call = ToolCall::new(id, string(call, "name")?, call["args"].clone());
        if !call.arguments.is_object() {
            return Err(Error::protocol());
        }
        if b.get("thoughtSignature").is_some() || b["functionCall"]["id"].is_null() {
            call.metadata.push(p.metadata(b.clone()));
        }
        Ok(ContentBlock::ToolCall(call))
    } else if b["thought"] == true {
        Ok(ContentBlock::Provider(p.metadata(b.clone())))
    } else if let Some(text) = b["text"].as_str() {
        let metadata = if b.as_object().is_some_and(|m| m.keys().any(|k| k != "text")) {
            vec![p.metadata(b.clone())]
        } else {
            vec![]
        };
        Ok(ContentBlock::Text {
            text: text.into(),
            metadata,
        })
    } else {
        Ok(ContentBlock::Provider(p.metadata(b.clone())))
    }
}
pub(super) fn decode_vertex(
    p: &HttpProvider,
    r: &ChatRequest,
    v: &Value,
) -> Result<ChatResponse, Error> {
    if v["promptFeedback"]["blockReason"].is_string() {
        return Ok(empty_response(
            r,
            &p.name,
            vec![],
            vertex_usage(&v["usageMetadata"]),
            FinishReason::ContentFilter,
        ));
    }
    let candidates = v["candidates"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(Error::protocol)?;
    let candidate = &candidates[0];
    let mut content = vec![];
    if let Some(parts) = candidate["content"]["parts"].as_array() {
        for (i, b) in parts.iter().enumerate() {
            content.push(vertex_part(p, b, i)?);
        }
    }
    let reason = if content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall(_)))
    {
        FinishReason::ToolCalls
    } else {
        finish(string(candidate, "finishReason")?)
    };
    Ok(empty_response(
        r,
        &p.name,
        content,
        vertex_usage(&v["usageMetadata"]),
        reason,
    ))
}
pub(super) fn stream_vertex(d: &mut Decoder, v: &Value) -> Result<Vec<ChatEvent>, Error> {
    let mut out = vec![];
    if v["promptFeedback"]["blockReason"].is_string() {
        out.push(ChatEvent::Completed {
            reason: FinishReason::ContentFilter,
        });
        return Ok(out);
    }
    if let Some(candidates) = v["candidates"].as_array() {
        if candidates.len() > 1 {
            return Err(Error::unsupported("multiple candidates"));
        }
        for candidate in candidates {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for part in parts {
                    if let Some(call) = part.get("functionCall") {
                        d.indices.remove("active_text");
                        let index = if let Some(id) = call["id"].as_str() {
                            d.index(format!("call:{id}"))
                        } else {
                            let index = d.next_index;
                            d.next_index += 1;
                            index
                        };
                        if !d.started.insert(index) {
                            return Err(Error::unsupported("incremental vertex function calls"));
                        }
                        let id = call["id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("call_{:032x}", rand::random::<u128>()));
                        out.push(ChatEvent::ToolCallStart {
                            index,
                            id,
                            name: string(call, "name")?.into(),
                        });
                        out.push(ChatEvent::ToolCallDelta {
                            index,
                            delta: call["args"].to_string(),
                        });
                        if part.get("thoughtSignature").is_some() || call["id"].is_null() {
                            out.push(ChatEvent::Metadata {
                                index,
                                metadata: d.meta(part.clone()),
                            });
                        }
                    } else if part["thought"] == true {
                        d.indices.remove("active_text");
                        let index = d.next_index;
                        d.next_index += 1;
                        out.push(ChatEvent::Metadata {
                            index,
                            metadata: d.meta(part.clone()),
                        });
                    } else if let Some(text) = part["text"].as_str() {
                        let index = d.index("active_text".into());
                        out.push(ChatEvent::TextDelta {
                            index,
                            delta: text.into(),
                        });
                        if part.get("thoughtSignature").is_some() {
                            out.push(ChatEvent::Metadata {
                                index,
                                metadata: d.meta(part.clone()),
                            });
                            d.indices.remove("active_text");
                        }
                    } else {
                        return Err(Error::unsupported("vertex stream content type"));
                    }
                }
            }
            if let Some(reason) = candidate["finishReason"].as_str() {
                d.reason = Some(if d.started.is_empty() {
                    finish(reason)
                } else {
                    FinishReason::ToolCalls
                });
            }
        }
    }
    if let Some(u) = vertex_usage(&v["usageMetadata"]) {
        out.push(ChatEvent::Usage(u));
    }
    if let Some(reason) = d.reason.take() {
        out.push(ChatEvent::Completed { reason });
    }
    Ok(out)
}
