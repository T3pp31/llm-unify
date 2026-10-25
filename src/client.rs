use crate::router::Router;
use crate::{
    Cache, ChatEvent, ChatRequest, ChatResponse, ChatStream, Config, Deployment, EmbeddingRequest,
    EmbeddingResponse, Error, ErrorKind, MemoryCache, ModelId, Observation, Observer, Provider,
    Route,
};
use futures_util::{StreamExt, stream};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}
struct Inner {
    config: Arc<Config>,
    providers: BTreeMap<String, Arc<dyn Provider>>,
    router: Arc<Router>,
    cache: Option<Arc<dyn Cache>>,
    cache_namespace: [u8; 32],
    observer: Option<Arc<dyn Observer>>,
}
#[derive(Default)]
pub struct ClientBuilder {
    config: Config,
    keys: BTreeMap<String, String>,
    providers: BTreeMap<String, Arc<dyn Provider>>,
    cache: Option<Arc<dyn Cache>>,
    observer: Option<Arc<dyn Observer>>,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field(
                "providers",
                &self.inner.providers.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBuilder").finish_non_exhaustive()
    }
}
impl ClientBuilder {
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
    pub fn api_key(mut self, provider: impl Into<String>, key: impl Into<String>) -> Self {
        self.keys.insert(provider.into(), key.into());
        self
    }
    pub fn provider(mut self, name: impl Into<String>, provider: Arc<dyn Provider>) -> Self {
        self.providers.insert(name.into(), provider);
        self
    }
    pub fn cache(mut self, cache: Arc<dyn Cache>) -> Self {
        self.cache = Some(cache);
        self
    }
    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }
    pub fn build(mut self) -> Result<Client, Error> {
        self.config.validate()?;
        for name in self.providers.keys() {
            if name.is_empty()
                || name == "alias"
                || name.contains('/')
                || name.chars().any(char::is_whitespace)
            {
                return Err(crate::config::configuration());
            }
        }
        for name in self.config.aliases.keys() {
            if self.config.routes.contains_key(name) {
                return Err(crate::config::configuration());
            }
        }
        validate_aliases(&self.config, &self.providers)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(self.config.http.request_timeout_ms))
            .build()
            .map_err(|_| crate::config::configuration())?;
        let config = Arc::new(self.config);
        for (name, p) in &config.providers {
            if !self.providers.contains_key(name) {
                let key = self.keys.remove(name).or_else(|| {
                    p.api_key_env
                        .as_ref()
                        .and_then(|name| std::env::var(name).ok())
                });
                self.providers.insert(
                    name.clone(),
                    crate::providers::builtin(
                        name.clone(),
                        p.clone(),
                        config.clone(),
                        http.clone(),
                        key,
                    ),
                );
            }
        }
        let cache = match self.cache {
            Some(cache) => Some(cache),
            None if config.cache.enabled => Some(Arc::new(MemoryCache::new(
                config.cache.capacity,
                Duration::from_millis(config.cache.ttl_ms),
            )?) as Arc<dyn Cache>),
            None => None,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                router: Arc::new(Router::new(config.router.clone())),
                config,
                providers: self.providers,
                cache,
                cache_namespace: rand::random(),
                observer: self.observer,
            }),
        })
    }
}
impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }
    pub fn from_env() -> Result<Self, Error> {
        Self::builder().config(Config::from_env()?).build()
    }
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, Error> {
        match self.execute(request, false).await? {
            Output::Response(r) => Ok(r),
            Output::Stream(_) => unreachable!(),
        }
    }
    pub async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, Error> {
        match self.execute(request, true).await? {
            Output::Stream(s) => Ok(s),
            Output::Response(_) => unreachable!(),
        }
    }
    fn candidates(&self, model: &ModelId) -> Result<Vec<Deployment>, Error> {
        let mut visited = BTreeSet::new();
        let mut out = vec![];
        self.resolve(model.as_str(), &mut visited, &mut out)?;
        if out.is_empty() {
            return Err(Error::new(ErrorKind::Routing, "no available deployment"));
        }
        Ok(out)
    }
    fn resolve(
        &self,
        model: &str,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<Deployment>,
    ) -> Result<(), Error> {
        if !visited.insert(model.into()) {
            return Err(crate::config::configuration());
        }
        let id = ModelId::parse(model)?;
        let (provider, name) = id.parts()?;
        if provider == "alias" {
            if let Some(target) = self.inner.config.aliases.get(name) {
                self.resolve(target, visited, out)?
            } else if let Some(route) = self.inner.config.routes.get(name) {
                if let Ok(deployments) = self.inner.router.order(name, route) {
                    out.extend(deployments);
                }
                for fallback in &route.fallbacks {
                    self.resolve(fallback, visited, out)?
                }
            } else {
                return Err(Error::new(ErrorKind::InvalidRequest, "unknown model alias"));
            }
        } else {
            out.push(Deployment {
                model: model.into(),
                provider: provider.into(),
                weight: 1,
            });
        }
        visited.remove(model);
        Ok(())
    }
    fn prepare(
        &self,
        request: &ChatRequest,
        d: &Deployment,
        stream: bool,
    ) -> Result<(ChatRequest, Arc<dyn Provider>), Error> {
        let provider = self
            .inner
            .providers
            .get(&d.provider)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "unknown provider"))?;
        let mut request = request.clone();
        request.model = ModelId::parse(&d.model)?;
        let caps = self
            .inner
            .config
            .capabilities
            .get(&d.model)
            .cloned()
            .unwrap_or_else(|| {
                provider.capabilities(request.model.parts().map(|(_, m)| m).unwrap_or_default())
            });
        caps.validate(&request, stream)?;
        // Metadata cannot be forwarded through a custom adapter to another provider.
        {
            let config = self.inner.config.providers.get(&d.provider);
            let identity = config
                .map(|config| {
                    if config.kind == "openai_compatible" {
                        d.provider.as_str()
                    } else {
                        config.kind.as_str()
                    }
                })
                .unwrap_or(&d.provider);
            let format = config.map(|c| match c.kind.as_str() {
                "openai" => c.api.as_deref().unwrap_or("responses"),
                "openai_compatible" => "chat_completions",
                "anthropic" => "messages",
                "gemini" => "interactions",
                "bedrock" => "converse",
                "vertex" => "generate_content",
                _ => "unknown",
            });
            for meta in request
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .flat_map(crate::ContentBlock::metadata)
                .chain(&request.options.provider_options)
            {
                if meta.provider != identity || format.is_some_and(|format| meta.format != format) {
                    return Err(Error::unsupported("metadata provider mismatch"));
                }
            }
        }
        Ok((request, provider))
    }
    fn cache_key(&self, request: &ChatRequest, d: &Deployment) -> Result<String, Error> {
        let mut hash = Sha256::new();
        hash.update(self.inner.cache_namespace);
        hash.update(d.provider.as_bytes());
        hash.update(
            serde_json::to_vec(request).map_err(|_| {
                Error::new(ErrorKind::InvalidRequest, "request cannot be serialized")
            })?,
        );
        Ok(format!("{:x}", hash.finalize()))
    }
    fn observe(
        &self,
        d: &Deployment,
        start: Instant,
        attempts: usize,
        response: Option<&ChatResponse>,
        error: Option<ErrorKind>,
    ) {
        crate::observability::emit(
            self.inner.observer.as_deref(),
            &Observation {
                model: d.model.clone(),
                deployment: d.provider.clone(),
                elapsed: start.elapsed(),
                attempts,
                cache_hit: response.is_some_and(|r| r.cache_hit),
                usage: response.and_then(|r| r.usage.clone()),
                error,
            },
        );
    }
    async fn execute(&self, request: ChatRequest, streaming: bool) -> Result<Output, Error> {
        crate::capabilities::validate_request(&request)?;
        let candidates = self.candidates(&request.model)?;
        let start = Instant::now();
        let deadline = start + Duration::from_millis(self.inner.config.retry.total_timeout_ms);
        let mut attempts = 0;
        let mut position = 0;
        let mut last = Error::new(ErrorKind::Routing, "no compatible available deployment");
        while attempts < self.inner.config.retry.max_attempts {
            if Instant::now() >= deadline {
                return Err(Error::transport(false));
            }
            let d = &candidates[position.min(candidates.len() - 1)];
            let (request, provider) = match self.prepare(&request, d, streaming) {
                Ok(v) => v,
                Err(error) => {
                    last = error;
                    position += 1;
                    if position >= candidates.len() {
                        return Err(last);
                    }
                    continue;
                }
            };
            let key = if !streaming && self.inner.cache.is_some() {
                Some(self.cache_key(&request, d)?)
            } else {
                None
            };
            if let (Some(cache), Some(key)) = (&self.inner.cache, &key)
                && let Ok(Ok(Some(mut response))) =
                    tokio::time::timeout_at(deadline, cache.get(key)).await
            {
                response.cache_hit = true;
                self.observe(d, start, attempts, Some(&response), None);
                return Ok(Output::Response(response));
            }
            if !self.inner.router.begin(d)? {
                position += 1;
                if position >= candidates.len() {
                    return Err(last);
                }
                continue;
            }
            let lease = Lease {
                router: self.inner.router.clone(),
                deployment: d.clone(),
                finished: false,
            };
            attempts += 1;
            let result = tokio::time::timeout_at(deadline, async {
                if streaming {
                    let mut events = provider.chat_stream(&request).await?;
                    match events.next().await {
                        Some(Ok(first)) => Ok(Output::Stream(
                            self.observe_stream(events, first, lease, start, deadline, attempts),
                        )),
                        Some(Err(error)) => {
                            lease.finish(error.retryable);
                            Err(error)
                        }
                        None => {
                            lease.finish(true);
                            Err(Error::transport(true))
                        }
                    }
                } else {
                    match provider.chat(&request).await {
                        Ok(mut response) => {
                            response.deployment = d.provider.clone();
                            response.cache_hit = false;
                            if let Some(usage) = &response.usage {
                                response.cost_usd = self
                                    .inner
                                    .config
                                    .prices
                                    .get(response.model.as_str())
                                    .or_else(|| self.inner.config.prices.get(&d.model))
                                    .map(|p| p.cost(usage));
                            }
                            lease.finish(false);
                            Ok(Output::Response(response))
                        }
                        Err(error) => {
                            lease.finish(error.retryable);
                            Err(error)
                        }
                    }
                }
            })
            .await
            .unwrap_or_else(|_| Err(Error::transport(false)));
            match result {
                Ok(Output::Response(response)) => {
                    if let (Some(cache), Some(key)) = (&self.inner.cache, key) {
                        let _ =
                            tokio::time::timeout_at(deadline, cache.insert(key, response.clone()))
                                .await;
                    }
                    self.observe(d, start, attempts, Some(&response), None);
                    return Ok(Output::Response(response));
                }
                Ok(output) => return Ok(output),
                Err(error) => {
                    self.observe(d, start, attempts, None, Some(error.kind));
                    if !error.retryable || attempts >= self.inner.config.retry.max_attempts {
                        return Err(error);
                    }
                    let delay = self.backoff(&error, attempts);
                    last = error;
                    self.wait(delay, deadline).await?;
                    if position + 1 < candidates.len() {
                        position += 1;
                    }
                }
            }
        }
        Err(last)
    }
    fn observe_stream(
        &self,
        events: ChatStream,
        first: ChatEvent,
        lease: Lease,
        start: Instant,
        deadline: Instant,
        attempts: usize,
    ) -> ChatStream {
        let inner = self.inner.clone();
        let deployment = lease.deployment.clone();
        let model = ModelId::parse(&deployment.model).expect("validated model");
        let name = deployment.provider.clone();
        let source = stream::once(async move { Ok(first) }).chain(events);
        let observed = async_stream::stream! {
            futures_util::pin_mut!(source);
            let mut lease = Some(lease);
            let mut usage = None;
            let mut bytes = 0usize;
            loop {
                let result = tokio::time::timeout_at(deadline, source.next()).await;
                let mut item = match result {
                    Ok(Some(item)) => item,
                    Ok(None) => return,
                    Err(_) => Err(Error::transport(false)),
                };
                if let Ok(event) = &item {
                    bytes = bytes.saturating_add(event.payload_len());
                    if bytes > inner.config.http.max_response_bytes {
                        item = Err(Error::protocol());
                    }
                }
                let (done, error) = match &item {
                    Ok(ChatEvent::Usage(u)) => {
                        usage = Some(u.clone());
                        (false, None)
                    }
                    Ok(ChatEvent::Completed { .. }) => (true, None),
                    Err(e) => (true, Some(e.kind)),
                    _ => (false, None),
                };
                if done {
                    if let Some(lease) = lease.take() {
                        lease.finish(error.is_some());
                    }
                    crate::observability::emit(
                        inner.observer.as_deref(),
                        &Observation {
                            model: deployment.model.clone(),
                            deployment: deployment.provider.clone(),
                            elapsed: start.elapsed(),
                            attempts,
                            cache_hit: false,
                            usage: usage.clone(),
                            error,
                        },
                    );
                }
                yield item;
                if done {
                    return;
                }
            }
        };
        ChatStream::new(model, name, observed)
    }
    fn backoff(&self, error: &Error, attempts: usize) -> Duration {
        if let Some(delay) = error.retry_after {
            return delay;
        }
        let config = &self.inner.config.retry;
        let ceiling = config
            .base_delay_ms
            .saturating_mul(
                1u64.checked_shl((attempts - 1).min(63) as u32)
                    .unwrap_or(u64::MAX),
            )
            .min(config.max_delay_ms);
        Duration::from_millis(if ceiling == 0 {
            0
        } else {
            rand::random_range(0..=ceiling)
        })
    }
    async fn wait(&self, delay: Duration, deadline: Instant) -> Result<(), Error> {
        if delay >= deadline.saturating_duration_since(Instant::now()) {
            return Err(Error::transport(false));
        }
        tokio::time::sleep(delay).await;
        Ok(())
    }
    pub async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, Error> {
        if request.input.is_empty()
            || request.input.iter().any(String::is_empty)
            || request.dimensions == Some(0)
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "invalid embedding input",
            ));
        }
        let candidates = self.candidates(&request.model)?;
        let deadline =
            Instant::now() + Duration::from_millis(self.inner.config.retry.total_timeout_ms);
        let mut position = 0;
        let mut attempts = 0;
        while attempts < self.inner.config.retry.max_attempts {
            let d = &candidates[position.min(candidates.len() - 1)];
            let provider = self
                .inner
                .providers
                .get(&d.provider)
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "unknown provider"))?;
            if !self.inner.router.begin(d)? {
                position += 1;
                if position >= candidates.len() {
                    return Err(Error::new(ErrorKind::Routing, "no available deployment"));
                }
                continue;
            }
            let lease = Lease {
                router: self.inner.router.clone(),
                deployment: d.clone(),
                finished: false,
            };
            let mut request = request.clone();
            request.model = ModelId::parse(&d.model)?;
            attempts += 1;
            let result = tokio::time::timeout_at(deadline, provider.embed(&request))
                .await
                .unwrap_or_else(|_| Err(Error::transport(false)));
            match result {
                Ok(mut response) => {
                    lease.finish(false);
                    response.model = request.model;
                    response.deployment = d.provider.clone();
                    return Ok(response);
                }
                Err(error) => {
                    lease.finish(error.retryable);
                    if !error.retryable || attempts >= self.inner.config.retry.max_attempts {
                        return Err(error);
                    }
                    self.wait(self.backoff(&error, attempts), deadline).await?;
                    if position + 1 < candidates.len() {
                        position += 1;
                    }
                }
            }
        }
        Err(Error::new(ErrorKind::Routing, "attempt budget exhausted"))
    }
}
enum Output {
    Response(ChatResponse),
    Stream(ChatStream),
}
struct Lease {
    router: Arc<Router>,
    deployment: Deployment,
    finished: bool,
}
impl Lease {
    fn finish(mut self, failed: bool) {
        self.router.finish(&self.deployment, failed);
        self.finished = true;
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished {
            self.router.release(&self.deployment)
        }
    }
}

fn validate_aliases(
    config: &Config,
    custom: &BTreeMap<String, Arc<dyn Provider>>,
) -> Result<(), Error> {
    fn visit(
        model: &str,
        config: &Config,
        custom: &BTreeMap<String, Arc<dyn Provider>>,
        path: &mut BTreeSet<String>,
    ) -> Result<(), Error> {
        if !path.insert(model.into()) {
            return Err(crate::config::configuration());
        }
        let id = ModelId::parse(model)?;
        let (provider, name) = id.parts()?;
        if provider == "alias" {
            if let Some(target) = config.aliases.get(name) {
                visit(target, config, custom, path)?
            } else if let Some(Route {
                deployments,
                fallbacks,
            }) = config.routes.get(name)
            {
                for d in deployments {
                    let id = ModelId::parse(&d.model)?;
                    if id.parts()?.0 == "alias"
                        || !config.providers.contains_key(&d.provider)
                            && !custom.contains_key(&d.provider)
                    {
                        return Err(crate::config::configuration());
                    }
                }
                for f in fallbacks {
                    visit(f, config, custom, path)?
                }
            } else {
                return Err(crate::config::configuration());
            }
        } else if !config.providers.contains_key(provider) && !custom.contains_key(provider) {
            return Err(crate::config::configuration());
        }
        path.remove(model);
        Ok(())
    }
    for name in config.aliases.keys().chain(config.routes.keys()) {
        visit(
            &format!("alias/{name}"),
            config,
            custom,
            &mut BTreeSet::new(),
        )?;
    }
    Ok(())
}
