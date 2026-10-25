use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let model = Self(value.into());
        model.parts()?;
        Ok(model)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn parts(&self) -> Result<(&str, &str), Error> {
        self.0
            .split_once('/')
            .filter(|(p, m)| {
                !p.is_empty()
                    && !m.is_empty()
                    && !p.chars().any(char::is_whitespace)
                    && !m.chars().any(char::is_whitespace)
            })
            .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "model must be provider/model"))
    }
}
impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl FromStr for ModelId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Opaque native information, replayable only to its original provider and API.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub provider: String,
    pub format: String,
    pub data: Value,
}
impl fmt::Debug for ProviderMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderMetadata")
            .field("provider", &self.provider)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

#[non_exhaustive]
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default)]
        metadata: Vec<ProviderMetadata>,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Image(ImageContent),
    Provider(ProviderMetadata),
}
impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            metadata: vec![],
        }
    }
    pub fn metadata(&self) -> &[ProviderMetadata] {
        match self {
            Self::Text { metadata, .. } => metadata,
            Self::ToolCall(c) => &c.metadata,
            Self::Provider(m) => std::slice::from_ref(m),
            _ => &[],
        }
    }
}
impl fmt::Debug for ContentBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Text { .. } => "Text([REDACTED])",
            Self::ToolCall(_) => "ToolCall([REDACTED])",
            Self::ToolResult(_) => "ToolResult([REDACTED])",
            Self::Image(_) => "Image([REDACTED])",
            Self::Provider(_) => "Provider([REDACTED])",
        })
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default)]
    pub metadata: Vec<ProviderMetadata>,
}
impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            metadata: vec![],
        }
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: Option<String>,
    pub content: Value,
    #[serde(default)]
    pub is_error: bool,
}
impl ToolResult {
    pub fn new(call_id: impl Into<String>, content: Value) -> Self {
        Self {
            call_id: call_id.into(),
            name: None,
            content,
            is_error: false,
        }
    }
}
#[non_exhaustive]
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ImageContent {
    Url { url: String },
    Bytes { mime_type: String, data: Vec<u8> },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}
impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::text(text)])
    }
    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self::new(Role::Assistant, content)
    }
    pub fn tool(result: ToolResult) -> Self {
        Self::new(Role::Tool, vec![ContentBlock::ToolResult(result)])
    }
}
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("role", &self.role)
            .field("blocks", &self.content.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchema {
    pub name: String,
    pub schema: Value,
    #[serde(default)]
    pub strict: bool,
}
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationOptions {
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub top_p: Option<f64>,
    pub response_format: Option<JsonSchema>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Native top-level options. Reserved request fields cannot be overwritten.
    #[serde(default)]
    pub provider_options: Vec<ProviderMetadata>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub options: GenerationOptions,
    pub tools: Vec<ToolDefinition>,
}
impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: ModelId(model.into()),
            messages: vec![],
            options: GenerationOptions::default(),
            tools: vec![],
        }
    }
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }
    pub fn system(self, text: impl Into<String>) -> Self {
        self.message(Message::new(Role::System, vec![ContentBlock::text(text)]))
    }
    pub fn user(self, text: impl Into<String>) -> Self {
        self.message(Message::user(text))
    }
    pub fn max_output_tokens(mut self, value: u32) -> Self {
        self.options.max_output_tokens = Some(value);
        self
    }
    pub fn temperature(mut self, value: f64) -> Self {
        self.options.temperature = Some(value);
        self
    }
    pub fn tool(mut self, tool: ToolDefinition) -> Self {
        self.tools.push(tool);
        self
    }
    pub fn response_format(mut self, schema: JsonSchema) -> Self {
        self.options.response_format = Some(schema);
        self
    }
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.options.reasoning_effort = Some(effort);
        self
    }
}
impl fmt::Debug for ChatRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatRequest")
            .field("model", &self.model)
            .field("messages", &self.messages.len())
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub model: ModelId,
    pub deployment: String,
    pub content: Vec<ContentBlock>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    #[serde(default)]
    pub cache_hit: bool,
    pub cost_usd: Option<f64>,
    pub request_id: Option<String>,
    /// Read-only response metadata; not sent back by `into_message()`.
    #[serde(default)]
    pub provider_metadata: Vec<ProviderMetadata>,
}
impl ChatResponse {
    pub fn output_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    pub fn into_message(self) -> Message {
        Message::assistant(self.content)
    }
}
#[derive(Clone, PartialEq)]
pub struct PartialChatResponse {
    pub model: ModelId,
    pub deployment: String,
    pub content: Vec<ContentBlock>,
    pub usage: Option<TokenUsage>,
    pub pending_tool_arguments: std::collections::BTreeMap<usize, String>,
    pub provider_metadata: Vec<ProviderMetadata>,
}
impl fmt::Debug for PartialChatResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PartialChatResponse")
            .field("model", &self.model)
            .field("deployment", &self.deployment)
            .field("content", &self.content)
            .field("usage", &self.usage)
            .field("pending_tools", &self.pending_tool_arguments.len())
            .finish()
    }
}
