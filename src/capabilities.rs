use crate::{ChatRequest, ContentBlock, Error, ErrorKind, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// None means unknown, never an implicit denial.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub streaming: Option<bool>,
    pub tool_calling: Option<bool>,
    pub image_input: Option<bool>,
    pub structured_output: Option<bool>,
    pub reasoning: Option<bool>,
    pub temperature: Option<bool>,
    pub max_output_tokens: Option<bool>,
    pub top_p: Option<bool>,
}
impl Capabilities {
    pub(crate) fn validate(&self, r: &ChatRequest, stream: bool) -> Result<(), Error> {
        let images = r
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|c| matches!(c, ContentBlock::Image(_)));
        let tools = !r.tools.is_empty()
            || r.messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|c| matches!(c, ContentBlock::ToolCall(_) | ContentBlock::ToolResult(_)));
        for (present, supported, name) in [
            (stream, self.streaming, "streaming"),
            (tools, self.tool_calling, "tool_calling"),
            (images, self.image_input, "image_input"),
            (
                r.options.response_format.is_some(),
                self.structured_output,
                "structured_output",
            ),
            (
                r.options.reasoning_effort.is_some(),
                self.reasoning,
                "reasoning",
            ),
            (
                r.options.temperature.is_some(),
                self.temperature,
                "temperature",
            ),
            (
                r.options.max_output_tokens.is_some(),
                self.max_output_tokens,
                "max_output_tokens",
            ),
            (r.options.top_p.is_some(), self.top_p, "top_p"),
        ] {
            if present && supported == Some(false) {
                return Err(Error::unsupported(name));
            }
        }
        Ok(())
    }
}
pub(crate) fn validate_request(r: &ChatRequest) -> Result<(), Error> {
    r.model.parts()?;
    if r.messages.is_empty() {
        return Err(invalid());
    }
    if r.options.max_output_tokens == Some(0)
        || r.options
            .temperature
            .is_some_and(|v| !v.is_finite() || v < 0.0)
        || r.options
            .top_p
            .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
    {
        return Err(invalid());
    }
    let mut names = BTreeSet::new();
    for t in &r.tools {
        if t.name.is_empty() || !t.parameters.is_object() || !names.insert(&t.name) {
            return Err(invalid());
        }
    }
    if let Some(schema) = &r.options.response_format
        && (schema.name.is_empty() || !schema.schema.is_object())
    {
        return Err(invalid());
    }
    let mut pending = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for m in &r.messages {
        if m.content.is_empty() {
            return Err(invalid());
        }
        if m.role == Role::Tool
            && m.content
                .iter()
                .any(|b| !matches!(b, ContentBlock::ToolResult(_)))
        {
            return Err(invalid());
        }
        if !pending.is_empty()
            && m.content
                .iter()
                .any(|b| !matches!(b, ContentBlock::ToolResult(_)))
        {
            return Err(invalid());
        }
        for c in &m.content {
            for metadata in c.metadata() {
                if metadata.provider.is_empty()
                    || metadata.format.is_empty()
                    || !metadata.data.is_object()
                {
                    return Err(invalid());
                }
            }
            match c {
                ContentBlock::ToolCall(c) => {
                    if m.role != Role::Assistant
                        || c.id.is_empty()
                        || c.name.is_empty()
                        || !c.arguments.is_object()
                        || !seen.insert(c.id.clone())
                    {
                        return Err(invalid());
                    }
                    pending.insert(c.id.clone(), c.name.clone());
                }
                ContentBlock::ToolResult(t) => {
                    let name = pending.remove(&t.call_id).ok_or_else(invalid)?;
                    if !matches!(m.role, Role::Tool | Role::User)
                        || t.name.as_ref().is_some_and(|n| *n != name)
                    {
                        return Err(invalid());
                    }
                }
                ContentBlock::Image(crate::ImageContent::Url { url }) => {
                    let url = reqwest::Url::parse(url).map_err(|_| invalid())?;
                    if !matches!(url.scheme(), "https" | "http") {
                        return Err(invalid());
                    }
                }
                ContentBlock::Image(crate::ImageContent::Bytes { mime_type, data })
                    if (!matches!(
                        mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                    ) || data.is_empty()) =>
                {
                    return Err(invalid());
                }
                _ => {}
            }
        }
    }
    if !pending.is_empty() {
        return Err(invalid());
    }
    if r.options
        .provider_options
        .iter()
        .any(|m| m.provider.is_empty() || m.format.is_empty() || !m.data.is_object())
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> Error {
    Error::new(
        ErrorKind::InvalidRequest,
        "invalid messages, tool sequence or generation options",
    )
}
