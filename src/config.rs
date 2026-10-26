use crate::{Capabilities, Error, ErrorKind, Price, Route};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub max_response_bytes: usize,
    pub max_sse_event_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    pub max_attempts: usize,
    pub total_timeout_ms: u64,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub statuses: Vec<u16>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouterConfig {
    pub failure_threshold: u32,
    pub cooldown_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_ms: u64,
    pub capacity: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub listen: std::net::SocketAddr,
    pub database_path: std::path::PathBuf,
    pub api_key_env: String,
    pub max_request_bytes: usize,
    pub max_in_flight: usize,
    /// Maximum and default number of history or summary rows returned per call.
    pub history_page_size: usize,
    pub sqlite_busy_timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub api: Option<String>,
    pub api_version: Option<String>,
    pub default_max_output_tokens: Option<u32>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub region: Option<String>,
    pub oauth_scope: Option<String>,
}
impl ProviderConfig {
    pub fn openai_compatible(base_url: impl Into<String>) -> Self {
        Self {
            kind: "openai_compatible".into(),
            base_url: Some(base_url.into()),
            api: Some("chat_completions".into()),
            api_key_env: None,
            api_version: None,
            default_max_output_tokens: None,
            project: None,
            location: None,
            region: None,
            oauth_scope: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub http: HttpConfig,
    pub retry: RetryConfig,
    pub router: RouterConfig,
    pub cache: CacheConfig,
    #[serde(default)]
    pub gateway: Option<GatewayConfig>,
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capabilities>,
    #[serde(default)]
    pub prices: BTreeMap<String, Price>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub routes: BTreeMap<String, Route>,
}
impl Default for Config {
    fn default() -> Self {
        let mut value: Self = toml::from_str(include_str!("../config/defaults.toml"))
            .expect("bundled defaults must be valid");
        #[derive(Deserialize)]
        struct Models<T> {
            models: BTreeMap<String, T>,
        }
        value.capabilities =
            toml::from_str::<Models<Capabilities>>(include_str!("../config/capabilities.toml"))
                .expect("bundled capabilities must be valid")
                .models;
        value.prices = toml::from_str::<Models<Price>>(include_str!("../config/pricing.toml"))
            .expect("bundled prices must be valid")
            .models;
        value
    }
}
impl Config {
    /// Merge a partial TOML configuration over bundled defaults.
    pub fn from_toml(input: &str) -> Result<Self, Error> {
        fn merge(base: &mut toml::Value, overlay: toml::Value) {
            match (base, overlay) {
                (toml::Value::Table(base), toml::Value::Table(overlay)) => {
                    for (k, v) in overlay {
                        if let Some(old) = base.get_mut(&k) {
                            merge(old, v)
                        } else {
                            base.insert(k, v);
                        }
                    }
                }
                (base, overlay) => *base = overlay,
            }
        }
        let mut base = toml::Value::try_from(Self::default()).map_err(|_| configuration())?;
        merge(
            &mut base,
            toml::from_str(input).map_err(|_| configuration())?,
        );
        let config: Self = base.try_into().map_err(|_| configuration())?;
        config.validate()?;
        Ok(config)
    }
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_toml(&std::fs::read_to_string(path).map_err(|_| configuration())?)
    }
    pub fn from_env() -> Result<Self, Error> {
        let mut config = match std::env::var("LLM_UNIFY_CONFIG") {
            Ok(path) => Self::from_file(path)?,
            Err(std::env::VarError::NotPresent) => Self::default(),
            Err(_) => return Err(configuration()),
        };
        for (name, p) in &mut config.providers {
            let prefix = format!("LLM_UNIFY_{}", name.to_ascii_uppercase().replace('-', "_"));
            for (suffix, field) in [
                ("BASE_URL", &mut p.base_url),
                ("API", &mut p.api),
                ("API_VERSION", &mut p.api_version),
                ("PROJECT", &mut p.project),
                ("LOCATION", &mut p.location),
                ("REGION", &mut p.region),
            ] {
                if let Ok(v) = std::env::var(format!("{prefix}_{suffix}")) {
                    *field = Some(v)
                }
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(gateway) = &self.gateway
            && (gateway.database_path.as_os_str().is_empty()
                || gateway.database_path == Path::new(":memory:")
                || gateway.api_key_env.is_empty()
                || gateway.max_request_bytes == 0
                || gateway.max_in_flight == 0
                || gateway.max_in_flight > tokio::sync::Semaphore::MAX_PERMITS
                || gateway.history_page_size == 0
                || i64::try_from(gateway.history_page_size)
                    .ok()
                    .is_none_or(|n| n == i64::MAX)
                || gateway.sqlite_busy_timeout_ms == 0
                || gateway.sqlite_busy_timeout_ms > i32::MAX as u64)
        {
            return Err(configuration());
        }
        if self.retry.max_attempts == 0
            || self.retry.total_timeout_ms == 0
            || self.retry.base_delay_ms > self.retry.max_delay_ms
            || self.http.request_timeout_ms == 0
            || self.http.stream_idle_timeout_ms == 0
            || self.http.max_response_bytes == 0
            || self.http.max_sse_event_bytes == 0
            || self.router.failure_threshold == 0
            || self.router.cooldown_ms == 0
            || self.cache.capacity == 0
            || self.cache.ttl_ms == 0
        {
            return Err(configuration());
        }
        for (name, p) in &self.providers {
            if name.is_empty()
                || name == "alias"
                || name.contains('/')
                || name.chars().any(char::is_whitespace)
            {
                return Err(configuration());
            }
            if ![
                "openai",
                "openai_compatible",
                "anthropic",
                "gemini",
                "bedrock",
                "vertex",
            ]
            .contains(&p.kind.as_str())
                || p.default_max_output_tokens == Some(0)
            {
                return Err(configuration());
            }
            if p.kind == "openai"
                && p.api
                    .as_deref()
                    .is_some_and(|v| !matches!(v, "responses" | "chat_completions"))
                || p.kind == "openai_compatible"
                    && p.api.as_deref().is_some_and(|v| v != "chat_completions")
            {
                return Err(configuration());
            }
            if let Some(url) = &p.base_url {
                let url = url.replace("{location}", p.location.as_deref().unwrap_or("location"));
                let url = reqwest::Url::parse(&url).map_err(|_| configuration())?;
                if !matches!(url.scheme(), "http" | "https")
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(configuration());
                }
            }
        }
        for p in self.prices.values() {
            if !p.input_per_million.is_finite()
                || !p.output_per_million.is_finite()
                || p.input_per_million < 0.0
                || p.output_per_million < 0.0
                || p.effective_date.is_empty()
                || p.source.is_empty()
            {
                return Err(configuration());
            }
        }
        for model in self.capabilities.keys().chain(self.prices.keys()) {
            crate::ModelId::parse(model).map_err(|_| configuration())?;
        }
        for route in self.routes.values() {
            route.validate()?;
        }
        Ok(())
    }
}
pub(crate) fn configuration() -> Error {
    Error::new(ErrorKind::Configuration, "invalid SDK configuration")
}
