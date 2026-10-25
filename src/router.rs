use crate::{Error, ErrorKind, ModelId, RouterConfig};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};
use tokio::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub model: String,
    pub provider: String,
    pub weight: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub deployments: Vec<Deployment>,
    #[serde(default)]
    pub fallbacks: Vec<String>,
}
impl Route {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.deployments.is_empty()
            || self.deployments.iter().any(|d| {
                d.weight == 0 || d.provider.is_empty() || ModelId::parse(&d.model).is_err()
            })
            || self
                .deployments
                .iter()
                .map(|d| u64::from(d.weight))
                .sum::<u64>()
                > i64::MAX as u64
        {
            return Err(crate::config::configuration());
        }
        Ok(())
    }
}
#[derive(Default)]
struct Circuit {
    failures: u32,
    open_at: Option<Instant>,
    probing: bool,
}
#[derive(Default)]
struct State {
    weights: BTreeMap<String, Vec<i64>>,
    circuits: BTreeMap<String, Circuit>,
}
pub(crate) struct Router {
    state: Mutex<State>,
    config: RouterConfig,
}
impl Router {
    pub fn new(config: RouterConfig) -> Self {
        Self {
            state: Mutex::new(State::default()),
            config,
        }
    }
    /// Smooth weighted round robin; exclude open circuits before accounting weight.
    pub fn order(&self, name: &str, route: &Route) -> Result<Vec<Deployment>, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| crate::config::configuration())?;
        let available: Vec<usize> = route
            .deployments
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let c = state.circuits.entry(key(d)).or_default();
                if c.open_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(self.config.cooldown_ms))
                    || c.probing
                {
                    None
                } else {
                    Some(i)
                }
            })
            .collect();
        if available.is_empty() {
            return Err(Error::new(
                ErrorKind::Routing,
                "all deployments are temporarily unavailable",
            ));
        }
        let weights = state
            .weights
            .entry(name.to_owned())
            .or_insert_with(|| vec![0; route.deployments.len()]);
        let mut total = 0;
        let mut selected = available[0];
        for &i in &available {
            let weight = i64::from(route.deployments[i].weight);
            total += weight;
            weights[i] += weight;
            if weights[i] > weights[selected] {
                selected = i
            }
        }
        weights[selected] -= total;
        let mut result = vec![route.deployments[selected].clone()];
        result.extend(
            available
                .into_iter()
                .filter(|i| *i != selected)
                .map(|i| route.deployments[i].clone()),
        );
        Ok(result)
    }
    pub fn begin(&self, d: &Deployment) -> Result<bool, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| crate::config::configuration())?;
        let c = state.circuits.entry(key(d)).or_default();
        if let Some(at) = c.open_at {
            if c.probing || at.elapsed() < Duration::from_millis(self.config.cooldown_ms) {
                return Ok(false);
            }
            c.probing = true;
        }
        Ok(true)
    }
    pub fn finish(&self, d: &Deployment, retryable_failure: bool) {
        if let Ok(mut state) = self.state.lock() {
            let c = state.circuits.entry(key(d)).or_default();
            c.probing = false;
            if retryable_failure {
                c.failures = c.failures.saturating_add(1);
                if c.failures >= self.config.failure_threshold {
                    c.open_at = Some(Instant::now())
                }
            } else {
                c.failures = 0;
                c.open_at = None;
            }
        }
    }
    pub fn release(&self, d: &Deployment) {
        if let Ok(mut state) = self.state.lock()
            && let Some(c) = state.circuits.get_mut(&key(d))
        {
            c.probing = false;
        }
    }
}
fn key(d: &Deployment) -> String {
    format!("{}:{}", d.provider, d.model)
}
