//! Native execution adapter for the pinned LiteLLM source.
//!
//! The configuration validator intentionally rejects settings that this adapter cannot
//! execute. The upstream config deserializer preserves unknown settings; accepting those
//! here would incorrectly imply that budgets, callbacks or retries were active.

use std::{collections::BTreeSet, path::Path, sync::Arc};

use serde::Deserialize;
use serde_json::Value;

pub mod openai;

/// Durable accounting and shared-limit building blocks. HTTP billing integration
/// is not implied by exposing this API.
pub use llm_unify_storage as storage;

pub use litellm_config::Config;
pub use litellm_gateway_auth::Auth;
pub use litellm_gateway_inference::Gateway;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read configuration")]
    Io(#[from] std::io::Error),
    #[error("invalid YAML configuration at {0}; values omitted to protect credentials")]
    Yaml(String),
    #[error("unsupported setting: {0}; it must be implemented before this configuration can run")]
    Unsupported(String),
    #[error(
        "custom Python extensions cannot run in the Rust proxy; migrate this setting to a Rust extension: {0}"
    )]
    PythonExtension(String),
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
    #[error("native execution initialization failed")]
    Native(#[from] litellm_gateway::Error),
    #[error("proxy authentication initialization failed")]
    Authentication(#[from] litellm_gateway_auth::Error),
}

#[derive(Deserialize)]
struct Capabilities {
    root: Vec<String>,
    general_settings: Vec<String>,
    model: Vec<String>,
    litellm_params: Vec<String>,
    python_extension_settings: Vec<String>,
}

fn yaml_error(error: serde_yaml_ng::Error) -> Error {
    let location = error.location().map_or_else(
        || "unknown location".to_owned(),
        |location| format!("line {}, column {}", location.line(), location.column()),
    );
    Error::Yaml(location)
}

fn validate_keys(
    value: &Value,
    allowed: &[String],
    prefix: &str,
    python: &[String],
) -> Result<(), Error> {
    let object = value
        .as_object()
        .ok_or(Error::Invalid("expected a mapping"))?;
    for key in object.keys() {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if python.contains(key) {
            return Err(Error::PythonExtension(path));
        }
        if !allowed.contains(key) {
            return Err(Error::Unsupported(path));
        }
    }
    Ok(())
}

/// Load the supported YAML subset, refusing ignored settings and ambiguous deployments.
pub fn load_config(path: impl AsRef<Path>) -> Result<Config, Error> {
    parse_config(&std::fs::read_to_string(path)?)
}

pub fn parse_config(yaml: &str) -> Result<Config, Error> {
    let raw: Value = serde_yaml_ng::from_str(yaml).map_err(yaml_error)?;
    let capabilities: Capabilities = serde_json::from_str(include_str!(
        "../../../config/parity/native-capabilities.json"
    ))
    .map_err(|_| Error::Invalid("invalid bundled native capability manifest"))?;
    validate_keys(
        &raw,
        &capabilities.root,
        "",
        &capabilities.python_extension_settings,
    )?;
    if let Some(general) = raw.get("general_settings") {
        validate_keys(
            general,
            &capabilities.general_settings,
            "general_settings",
            &capabilities.python_extension_settings,
        )?;
    }
    let models = raw
        .get("model_list")
        .and_then(Value::as_array)
        .filter(|models| !models.is_empty())
        .ok_or(Error::Invalid("model_list must be a nonempty list"))?;
    let mut names = BTreeSet::new();
    for (index, model) in models.iter().enumerate() {
        let prefix = format!("model_list[{index}]");
        validate_keys(
            model,
            &capabilities.model,
            &prefix,
            &capabilities.python_extension_settings,
        )?;
        let name = model
            .get("model_name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or(Error::Invalid("model_name must be nonempty"))?;
        if !names.insert(name) {
            // Upstream currently collects into a HashMap, silently discarding deployments.
            return Err(Error::Invalid(
                "duplicate model_name requires routing support",
            ));
        }
        let params = model
            .get("litellm_params")
            .ok_or(Error::Invalid("litellm_params is required"))?;
        validate_keys(
            params,
            &capabilities.litellm_params,
            &format!("{prefix}.litellm_params"),
            &capabilities.python_extension_settings,
        )?;
        if params
            .get("model")
            .and_then(Value::as_str)
            .is_none_or(|model| model.trim().is_empty())
        {
            return Err(Error::Invalid("deployment model must be nonempty"));
        }
    }
    let config = Config::from_yaml(yaml)
        .map_err(|_| Error::Invalid("configuration field has an invalid type"))?;
    if config
        .general_settings
        .master_key
        .as_ref()
        .is_none_or(|key| key.expose().trim().is_empty())
    {
        return Err(Error::Invalid("general_settings.master_key is required"));
    }
    Ok(config)
}

/// Build only the native inference graph. Never enables the Python host or body logging.
pub async fn build(config: &Config) -> Result<(Arc<Gateway>, Auth), Error> {
    let mut gateway = litellm_gateway::build_inference(config)?;
    // Native routes expect literal deployment values. Resolve the supported YAML
    // references once, through the same source used for proxy authentication.
    let mut resolved = config.clone();
    let resolve = async |name: &str| {
        gateway
            .secrets
            .get_secret_str(name)
            .await
            .map_err(|_| Error::Invalid("deployment environment reference could not be resolved"))?
            .filter(|value| !value.expose().trim().is_empty())
            .ok_or(Error::Invalid(
                "deployment environment reference is missing or empty",
            ))
    };
    for model in &mut resolved.model_list {
        let params = &mut model.litellm_params;
        if let Some(name) = params
            .api_key
            .as_ref()
            .and_then(|key| key.expose().strip_prefix("os.environ/"))
        {
            params.api_key = Some(resolve(name).await?);
        }
        if let Some(name) = params
            .api_base
            .as_deref()
            .and_then(|base| base.strip_prefix("os.environ/"))
        {
            params.api_base = Some(resolve(name).await?.expose().to_owned());
        }
    }
    Arc::get_mut(&mut gateway)
        .ok_or(Error::Invalid(
            "inference initialized with shared ownership",
        ))?
        .models = litellm_gateway_inference::ModelRouter::from_model_list(&resolved.model_list);
    let auth = Auth::from_config(config, gateway.secrets.clone());
    auth.validate().await?;
    Ok((gateway, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(extra: &str) -> String {
        format!(
            "model_list:\n  - model_name: demo\n    litellm_params:\n      model: openai/demo\ngeneral_settings:\n  master_key: test-only-key\n{extra}"
        )
    }

    #[test]
    fn accepts_native_deployment_and_authentication() {
        let config = parse_config(&yaml("")).unwrap();
        assert_eq!(config.model_list[0].model_name, "demo");
    }

    #[tokio::test]
    async fn refuses_missing_or_empty_deployment_environment_references_at_startup() {
        for field in ["api_base", "api_key"] {
            for environment in [
                "",
                "environment_variables:\n  LLM_UNIFY_TEST_UNSET_DEPLOYMENT: ''\n",
            ] {
                let input = yaml(environment).replace(
                    "model: openai/demo",
                    &format!("model: openai/demo\n      {field}: os.environ/LLM_UNIFY_TEST_UNSET_DEPLOYMENT"),
                );
                let config = parse_config(&input).unwrap();
                let Err(error) = build(&config).await else {
                    panic!("an unresolved deployment reference must prevent startup");
                };
                assert!(matches!(error, Error::Invalid(_)));
                assert!(
                    !error
                        .to_string()
                        .contains("LLM_UNIFY_TEST_UNSET_DEPLOYMENT")
                );
            }
        }
    }

    #[test]
    fn refuses_silently_ignored_router_cache_and_limit_settings() {
        for extra in [
            "router_settings:\n  num_retries: 3",
            "litellm_settings:\n  cache: true",
            "unknown_option: true",
        ] {
            assert!(matches!(
                parse_config(&yaml(extra)),
                Err(Error::Unsupported(_))
            ));
        }
        let input = yaml("").replace(
            "model: openai/demo",
            "model: openai/demo\n      max_budget: 1",
        );
        assert!(matches!(parse_config(&input), Err(Error::Unsupported(_))));
    }

    #[test]
    fn refuses_duplicate_deployments_instead_of_discarding_one() {
        let input = yaml("").replace("general_settings:", "  - model_name: demo\n    litellm_params:\n      model: openai/second\ngeneral_settings:");
        assert!(matches!(parse_config(&input), Err(Error::Invalid(_))));
    }

    #[test]
    fn refuses_missing_or_empty_authentication() {
        for input in [
            yaml("").replace("master_key: test-only-key", "master_key: ''"),
            yaml("").replace("  master_key: test-only-key", "  master_key: null"),
        ] {
            assert!(matches!(parse_config(&input), Err(Error::Invalid(_))));
        }
    }

    #[test]
    fn explains_python_extension_migration() {
        let input = yaml("").replace(
            "  master_key:",
            "  custom_auth: my_auth.handler\n  master_key:",
        );
        assert!(matches!(
            parse_config(&input),
            Err(Error::PythonExtension(_))
        ));
    }

    #[test]
    fn errors_do_not_echo_secret_values() {
        let error = parse_config(
            &yaml("").replace("model_name: demo", "model_name: [sensitive-test-value]"),
        )
        .err()
        .unwrap();
        assert!(!error.to_string().contains("sensitive-test-value"));
    }
}
