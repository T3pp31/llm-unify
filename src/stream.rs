use crate::{
    ChatResponse, ContentBlock, Error, ErrorKind, FinishReason, ModelId, PartialChatResponse,
    ProviderMetadata, TokenUsage, ToolCall,
};
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    pin::Pin,
    task::{Context, Poll},
};

#[non_exhaustive]
#[derive(Clone)]
pub enum ChatEvent {
    TextDelta {
        index: usize,
        delta: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        delta: String,
    },
    Metadata {
        index: usize,
        metadata: ProviderMetadata,
    },
    ResponseMetadata(ProviderMetadata),
    Usage(TokenUsage),
    Completed {
        reason: FinishReason,
    },
}
impl fmt::Debug for ChatEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TextDelta { .. } => "TextDelta([REDACTED])",
            Self::ToolCallStart { .. } => "ToolCallStart([REDACTED])",
            Self::ToolCallDelta { .. } => "ToolCallDelta([REDACTED])",
            Self::Metadata { .. } => "Metadata([REDACTED])",
            Self::ResponseMetadata(_) => "ResponseMetadata([REDACTED])",
            Self::Usage(_) => "Usage",
            Self::Completed { .. } => "Completed",
        })
    }
}
impl ChatEvent {
    pub(crate) fn payload_len(&self) -> usize {
        match self {
            Self::TextDelta { delta, .. } | Self::ToolCallDelta { delta, .. } => delta.len(),
            Self::ToolCallStart { id, name, .. } => id.len().saturating_add(name.len()),
            Self::Metadata { metadata, .. } | Self::ResponseMetadata(metadata) => {
                metadata.data.to_string().len()
            }
            _ => 0,
        }
    }
}

pub struct StreamAccumulator {
    model: ModelId,
    deployment: String,
    blocks: BTreeMap<usize, ContentBlock>,
    arguments: BTreeMap<usize, String>,
    usage: Option<TokenUsage>,
    reason: Option<FinishReason>,
    provider_metadata: Vec<ProviderMetadata>,
}
impl StreamAccumulator {
    pub fn new(model: ModelId, deployment: impl Into<String>) -> Self {
        Self {
            model,
            deployment: deployment.into(),
            blocks: BTreeMap::new(),
            arguments: BTreeMap::new(),
            usage: None,
            reason: None,
            provider_metadata: vec![],
        }
    }
    pub fn push(&mut self, event: &ChatEvent) -> Result<(), Error> {
        if self.reason.is_some() {
            return Err(Error::protocol());
        }
        match event {
            ChatEvent::TextDelta { index, delta } => {
                let entry = self
                    .blocks
                    .entry(*index)
                    .or_insert_with(|| ContentBlock::text(""));
                if let ContentBlock::Text { text, .. } = entry {
                    text.push_str(delta)
                } else {
                    return Err(Error::protocol());
                }
            }
            ChatEvent::ToolCallStart { index, id, name } => {
                if self.blocks.contains_key(index)
                    || id.is_empty()
                    || name.is_empty()
                    || self
                        .blocks
                        .values()
                        .any(|b| matches!(b,ContentBlock::ToolCall(call) if call.id==*id))
                {
                    return Err(Error::protocol());
                }
                self.blocks.insert(
                    *index,
                    ContentBlock::ToolCall(ToolCall::new(id, name, Value::Null)),
                );
                self.arguments.insert(*index, String::new());
            }
            ChatEvent::ToolCallDelta { index, delta } => self
                .arguments
                .get_mut(index)
                .ok_or_else(Error::protocol)?
                .push_str(delta),
            ChatEvent::Metadata { index, metadata } => match self.blocks.get_mut(index) {
                Some(ContentBlock::Text {
                    metadata: items, ..
                }) => items.push(metadata.clone()),
                Some(ContentBlock::ToolCall(call)) => call.metadata.push(metadata.clone()),
                None => {
                    self.blocks
                        .insert(*index, ContentBlock::Provider(metadata.clone()));
                }
                _ => return Err(Error::protocol()),
            },
            ChatEvent::Usage(usage) => self.usage = Some(usage.clone()),
            ChatEvent::ResponseMetadata(metadata) => {
                self.model = crate::providers::reported_model(&self.model, &metadata.data)?;
                self.provider_metadata.push(metadata.clone());
            }
            ChatEvent::Completed { reason } => {
                for (index, text) in &self.arguments {
                    let value: Value =
                        serde_json::from_str(if text.is_empty() { "{}" } else { text })
                            .map_err(|_| Error::protocol())?;
                    if !value.is_object() {
                        return Err(Error::protocol());
                    }
                    if let Some(ContentBlock::ToolCall(call)) = self.blocks.get_mut(index) {
                        call.arguments = value;
                    }
                }
                self.reason = Some(reason.clone());
            }
        }
        Ok(())
    }
    pub fn partial(&self) -> PartialChatResponse {
        PartialChatResponse {
            model: self.model.clone(),
            deployment: self.deployment.clone(),
            content: self.blocks.values().cloned().collect(),
            usage: self.usage.clone(),
            pending_tool_arguments: self.arguments.clone(),
            provider_metadata: self.provider_metadata.clone(),
        }
    }
    pub fn finish(self) -> Result<ChatResponse, Error> {
        let reason = self
            .reason
            .clone()
            .ok_or_else(|| Error::interrupted(self.partial()))?;
        Ok(ChatResponse {
            model: self.model,
            deployment: self.deployment,
            content: self.blocks.into_values().collect(),
            usage: self.usage,
            finish_reason: reason,
            cache_hit: false,
            cost_usd: None,
            request_id: None,
            provider_metadata: self.provider_metadata,
        })
    }
}

/// A guarded event stream. Dropping it cancels the owned HTTP response.
pub struct ChatStream {
    pub model: ModelId,
    pub deployment: String,
    inner: Pin<Box<dyn Stream<Item = Result<ChatEvent, Error>> + Send>>,
}
impl ChatStream {
    pub fn new<S>(model: ModelId, deployment: impl Into<String>, events: S) -> Self
    where
        S: Stream<Item = Result<ChatEvent, Error>> + Send + 'static,
    {
        let deployment = deployment.into();
        let mut state = StreamAccumulator::new(model.clone(), deployment.clone());
        let inner = async_stream::stream! {
            futures_util::pin_mut!(events);
            let mut emitted = false;
            while let Some(item) = events.next().await {
                match item {
                    Ok(event) => {
                        if state.push(&event).is_err() {
                            yield Err(Error::interrupted(state.partial()));
                            return;
                        }
                        let done = matches!(event, ChatEvent::Completed { .. });
                        emitted = true;
                        yield Ok(event);
                        if done {
                            return;
                        }
                    }
                    Err(mut error) => {
                        if emitted || error.kind == ErrorKind::StreamInterrupted {
                            error = Error::interrupted(state.partial());
                        }
                        yield Err(error);
                        return;
                    }
                }
            }
            yield Err(Error::interrupted(state.partial()));
        };
        Self {
            model,
            deployment,
            inner: Box::pin(inner),
        }
    }
    pub async fn collect_response(mut self) -> Result<ChatResponse, Error> {
        let mut accumulator = StreamAccumulator::new(self.model.clone(), self.deployment.clone());
        while let Some(event) = self.next().await {
            accumulator.push(&event?)?;
        }
        accumulator.finish()
    }
}
impl Stream for ChatStream {
    type Item = Result<ChatEvent, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
