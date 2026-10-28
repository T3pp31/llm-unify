//! Built-in native adapters. Custom adapters implement [`crate::Provider`].
mod anthropic;
#[cfg(feature = "bedrock")]
mod bedrock;
mod gemini;
mod openai;
#[cfg(feature = "vertex")]
mod vertex;

use crate::{
    Capabilities, ChatRequest, ChatResponse, ChatStream, Config, ContentBlock, EmbeddingRequest,
    EmbeddingResponse, Error, ErrorKind, Provider, ProviderConfig, ProviderMetadata, TokenUsage,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Secret(pub String);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
pub(crate) struct HttpProvider {
    pub name: String,
    pub config: ProviderConfig,
    pub settings: Arc<Config>,
    pub http: reqwest::Client,
    pub key: Option<Secret>,
    #[cfg(feature = "vertex")]
    pub credentials: tokio::sync::OnceCell<google_cloud_auth::credentials::AccessTokenCredentials>,
}
impl HttpProvider {
    pub fn new(
        name: String,
        config: ProviderConfig,
        settings: Arc<Config>,
        http: reqwest::Client,
        key: Option<String>,
    ) -> Self {
        Self {
            name,
            config,
            settings,
            http,
            key: key.map(Secret),
            #[cfg(feature = "vertex")]
            credentials: tokio::sync::OnceCell::new(),
        }
    }
    pub fn format(&self) -> &str {
        match self.config.kind.as_str() {
            "openai" => self.config.api.as_deref().unwrap_or("responses"),
            "openai_compatible" => "chat_completions",
            "anthropic" => "messages",
            "gemini" => "interactions",
            "vertex" => "generate_content",
            _ => "unknown",
        }
    }
    fn ensure_enabled(&self) -> Result<(), Error> {
        #[cfg(not(feature = "bedrock"))]
        if self.config.kind == "bedrock" {
            return Err(Error::unsupported("bedrock feature is disabled"));
        }
        #[cfg(not(feature = "vertex"))]
        if self.config.kind == "vertex" {
            return Err(Error::unsupported("vertex feature is disabled"));
        }
        Ok(())
    }
    pub fn identity(&self) -> &str {
        match self.config.kind.as_str() {
            "openai_compatible" => &self.name,
            kind => kind,
        }
    }
    pub fn check_metadata(&self, request: &ChatRequest) -> Result<(), Error> {
        for m in request
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .flat_map(ContentBlock::metadata)
            .chain(&request.options.provider_options)
        {
            if m.provider != self.identity() || m.format != self.format() {
                return Err(Error::unsupported("metadata provider/API mismatch"));
            }
        }
        Ok(())
    }
    pub fn metadata(&self, data: Value) -> ProviderMetadata {
        ProviderMetadata {
            provider: self.identity().into(),
            format: self.format().into(),
            data,
        }
    }
    pub fn native_options(
        &self,
        request: &ChatRequest,
        body: &mut Value,
        reserved: &[&str],
    ) -> Result<(), Error> {
        self.check_metadata(request)?;
        let body = body.as_object_mut().ok_or_else(Error::protocol)?;
        for options in &request.options.provider_options {
            let map = options.data.as_object().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    "provider options must be an object",
                )
            })?;
            for (k, v) in map {
                let nested_reserved: Option<&[&str]> = match k.as_str() {
                    "generation_config" => Some(&[
                        "temperature",
                        "top_p",
                        "max_output_tokens",
                        "thinking_level",
                    ]),
                    "generationConfig" => Some(&[
                        "temperature",
                        "topP",
                        "maxOutputTokens",
                        "responseMimeType",
                        "responseJsonSchema",
                        "thinkingConfig",
                    ]),
                    "output_config" => Some(&["format", "effort"]),
                    "reasoning" => Some(&["effort"]),
                    "text" => Some(&["format"]),
                    _ => None,
                };
                if let Some(protected) = nested_reserved.filter(|_| reserved.contains(&k.as_str()))
                {
                    let fields = v.as_object().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "native option container must be an object",
                        )
                    })?;
                    let target = body
                        .entry(k.clone())
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                        .ok_or_else(Error::protocol)?;
                    for (field, value) in fields {
                        if protected.contains(&field.as_str()) || target.contains_key(field) {
                            return Err(Error::new(
                                ErrorKind::InvalidRequest,
                                "native option overrides a reserved field",
                            ));
                        }
                        target.insert(field.clone(), value.clone());
                    }
                    continue;
                }
                if reserved.contains(&k.as_str()) || body.contains_key(k) {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "provider option overrides a reserved field",
                    ));
                }
                body.insert(k.clone(), v.clone());
            }
        }
        Ok(())
    }
    pub async fn post(&self, path: &str, body: Value) -> Result<reqwest::Response, Error> {
        let base = self
            .config
            .base_url
            .as_deref()
            .ok_or_else(crate::config::configuration)?;
        let url = if self.config.kind == "vertex" {
            base.replace(
                "{location}",
                self.config
                    .location
                    .as_deref()
                    .ok_or_else(crate::config::configuration)?,
            )
        } else {
            base.to_owned()
        };
        let mut request = self
            .http
            .post(format!("{}/{path}", url.trim_end_matches('/')))
            .json(&body);
        if self.config.kind == "vertex" {
            #[cfg(feature = "vertex")]
            {
                request = vertex::authenticate(self, request).await?;
            }
            #[cfg(not(feature = "vertex"))]
            {
                return Err(Error::unsupported("vertex feature is disabled"));
            }
        } else if self.config.api_key_env.is_some() || self.key.is_some() {
            let key = self
                .key
                .as_ref()
                .filter(|s| !s.0.is_empty())
                .ok_or_else(|| Error::new(ErrorKind::AuthError, "provider API key is missing"))?;
            let (header, value) = match self.config.kind.as_str() {
                "anthropic" => ("x-api-key", key.0.clone()),
                "gemini" => ("x-goog-api-key", key.0.clone()),
                _ => ("authorization", format!("Bearer {}", key.0)),
            };
            let mut value = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| Error::new(ErrorKind::AuthError, "invalid credential"))?;
            value.set_sensitive(true);
            request = request.header(header, value);
        }
        if self.config.kind == "anthropic" {
            request = request.header(
                "anthropic-version",
                self.config
                    .api_version
                    .as_deref()
                    .ok_or_else(crate::config::configuration)?,
            );
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::transport(e.is_connect() || e.is_timeout() || e.is_body()))?;
        crate::transport::checked(response, &self.settings.retry).await
    }
    async fn response(
        &self,
        path: &str,
        body: Value,
        request: &ChatRequest,
    ) -> Result<ChatResponse, Error> {
        let response = self.post(path, body).await?;
        let request_id = crate::transport::request_id(response.headers());
        let value = crate::transport::json(response, &self.settings.http).await?;
        let mut response = match self.config.kind.as_str() {
            "anthropic" => anthropic::decode(self, request, &value),
            "gemini" => gemini::decode(self, request, &value),
            "vertex" => gemini::decode_vertex(self, request, &value),
            _ => openai::decode(self, request, &value),
        }?;
        response.request_id = request_id;
        response.model = reported_model(&request.model, &value)?;
        response
            .provider_metadata
            .push(self.metadata(envelope(&value)));
        Ok(response)
    }
}
#[async_trait]
impl Provider for HttpProvider {
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
        self.ensure_enabled()?;
        let (path, body) = match self.config.kind.as_str() {
            "anthropic" => ("messages".to_owned(), anthropic::encode(self, r, false)?),
            "gemini" => ("interactions".to_owned(), gemini::encode(self, r, false)?),
            "vertex" => (
                gemini::vertex_path(self, r, false)?,
                gemini::encode_vertex(self, r)?,
            ),
            "openai" | "openai_compatible" => {
                (openai::path(self)?.into(), openai::encode(self, r, false)?)
            }
            _ => return Err(Error::unsupported("provider kind")),
        };
        self.response(&path, body, r).await
    }
    async fn chat_stream(&self, r: &ChatRequest) -> Result<ChatStream, Error> {
        self.ensure_enabled()?;
        let (path, body) = match self.config.kind.as_str() {
            "anthropic" => ("messages".to_owned(), anthropic::encode(self, r, true)?),
            "gemini" => ("interactions".to_owned(), gemini::encode(self, r, true)?),
            "vertex" => (
                gemini::vertex_path(self, r, true)?,
                gemini::encode_vertex(self, r)?,
            ),
            "openai" | "openai_compatible" => {
                (openai::path(self)?.into(), openai::encode(self, r, true)?)
            }
            _ => return Err(Error::unsupported("provider kind")),
        };
        let response = self.post(&path, body).await?;
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Err(Error::protocol());
        }
        let name = self.name.clone();
        let request_id = crate::transport::request_id(response.headers());
        let model = r.model.clone();
        let kind = self.config.kind.clone();
        let format = self.format().to_owned();
        let identity = self.identity().to_owned();
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;
        let source =
            crate::transport::sse_bytes(response, self.settings.http.clone()).eventsource();
        let events = async_stream::stream! {
            futures_util::pin_mut!(source);
            let mut decoder = openai::Decoder::new(kind, format, identity);
            while let Some(event) = source.next().await {
                let event = match event {
                    Ok(e) => e,
                    Err(eventsource_stream::EventStreamError::Transport(error)) => {
                        yield Err(error);
                        return;
                    }
                    Err(_) => {
                        yield Err(Error::protocol());
                        return;
                    }
                };
                let data = if event.data == "[DONE]" {
                    Value::String("[DONE]".into())
                } else {
                    match serde_json::from_str(&event.data) {
                        Ok(v) => v,
                        Err(_) => {
                            yield Err(Error::protocol());
                            return;
                        }
                    }
                };
                match decoder.decode(&event.event, &data) {
                    Ok(events) => {
                        for event in events {
                            let done = matches!(event, crate::ChatEvent::Completed { .. });
                            yield Ok(event);
                            if done {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
        };
        let mut stream = ChatStream::new(model, name, events);
        stream.request_id = request_id;
        Ok(stream)
    }
    async fn embed(&self, r: &EmbeddingRequest) -> Result<EmbeddingResponse, Error> {
        if !matches!(self.config.kind.as_str(), "openai" | "openai_compatible") {
            return Err(Error::unsupported("embeddings"));
        }
        if r.input.is_empty() || r.input.iter().any(String::is_empty) || r.dimensions == Some(0) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "invalid embedding input",
            ));
        }
        let mut body =
            json!({"model":r.model.parts()?.1,"input":r.input,"encoding_format":"float"});
        if let Some(d) = r.dimensions {
            body["dimensions"] = d.into();
        }
        let value =
            crate::transport::json(self.post("embeddings", body).await?, &self.settings.http)
                .await?;
        let mut embeddings = vec![];
        for item in value["data"].as_array().ok_or_else(Error::protocol)? {
            let index = item["index"].as_u64().ok_or_else(Error::protocol)? as usize;
            let vector: Vec<f32> =
                serde_json::from_value(item["embedding"].clone()).map_err(|_| Error::protocol())?;
            if vector.is_empty()
                || vector.iter().any(|n| !n.is_finite())
                || r.dimensions.is_some_and(|d| vector.len() != d as usize)
            {
                return Err(Error::protocol());
            }
            embeddings.push(crate::Embedding { index, vector });
        }
        embeddings.sort_by_key(|e| e.index);
        if embeddings.len() != r.input.len()
            || embeddings.iter().enumerate().any(|(i, e)| i != e.index)
        {
            return Err(Error::protocol());
        }
        Ok(EmbeddingResponse {
            model: r.model.clone(),
            deployment: self.name.clone(),
            embeddings,
            usage: openai::usage(value.get("usage"), false),
        })
    }
}
pub(crate) fn empty_response(
    r: &ChatRequest,
    name: &str,
    content: Vec<ContentBlock>,
    usage: Option<TokenUsage>,
    finish_reason: crate::FinishReason,
) -> ChatResponse {
    ChatResponse {
        model: r.model.clone(),
        deployment: name.into(),
        content,
        usage,
        finish_reason,
        cache_hit: false,
        cost_usd: None,
        request_id: None,
        provider_metadata: vec![],
    }
}
pub(crate) fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str, Error> {
    v[k].as_str().ok_or_else(Error::protocol)
}
/// Preserve native safety/usage/response fields without duplicating generated blocks.
pub(super) fn envelope(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(fields) = value.as_object_mut() {
        for key in ["output", "output_text", "steps", "content"] {
            fields.remove(key);
        }
        for key in ["candidates", "choices"] {
            if let Some(Value::Array(items)) = fields.get_mut(key) {
                for item in items {
                    if let Some(item) = item.as_object_mut() {
                        for key in ["content", "message", "delta"] {
                            item.remove(key);
                        }
                    }
                }
            }
        }
    }
    value
}
pub(crate) fn reported_model(
    requested: &crate::ModelId,
    value: &Value,
) -> Result<crate::ModelId, Error> {
    if let Some(model) = value["model"]
        .as_str()
        .or_else(|| value["modelVersion"].as_str())
    {
        crate::ModelId::parse(format!("{}/{model}", requested.parts()?.0))
            .map_err(|_| Error::protocol())
    } else {
        Ok(requested.clone())
    }
}
pub(crate) fn native(block: &ContentBlock, provider: &HttpProvider) -> Option<Value> {
    let mut result = None;
    for m in block
        .metadata()
        .iter()
        .filter(|m| m.provider == provider.identity() && m.format == provider.format())
    {
        match (&mut result, &m.data) {
            (Some(Value::Object(target)), Value::Object(source)) => target.extend(source.clone()),
            (target, data) => *target = Some(data.clone()),
        }
    }
    result
}
pub(crate) fn text_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

pub(crate) fn builtin(
    name: String,
    config: ProviderConfig,
    settings: Arc<Config>,
    http: reqwest::Client,
    key: Option<String>,
) -> Arc<dyn Provider> {
    #[cfg(feature = "bedrock")]
    if config.kind == "bedrock" {
        return Arc::new(bedrock::BedrockProvider::new(name, config, settings));
    }
    Arc::new(HttpProvider::new(name, config, settings, http, key))
}
