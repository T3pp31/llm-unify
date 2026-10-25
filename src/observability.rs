use crate::{ErrorKind, TokenUsage};
use serde::{Deserialize, Serialize};
use std::time::Duration;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub effective_date: String,
    pub source: String,
}
impl Price {
    pub(crate) fn cost(&self, usage: &TokenUsage) -> f64 {
        (usage.input_tokens as f64 * self.input_per_million
            + usage.output_tokens as f64 * self.output_per_million)
            / 1_000_000.0
    }
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub model: String,
    pub deployment: String,
    pub elapsed: Duration,
    pub attempts: usize,
    pub cache_hit: bool,
    pub usage: Option<TokenUsage>,
    pub error: Option<ErrorKind>,
}
pub trait Observer: Send + Sync {
    fn observe(&self, event: &Observation);
}
pub(crate) fn emit(observer: Option<&dyn Observer>, event: &Observation) {
    if let Some(observer) = observer {
        observer.observe(event)
    }
    #[cfg(feature = "opentelemetry")]
    {
        use opentelemetry::{
            KeyValue,
            trace::{Span, Tracer},
        };
        let attributes = vec![
            KeyValue::new("gen_ai.request.model", event.model.clone()),
            KeyValue::new("server.address", event.deployment.clone()),
            KeyValue::new("llm_unify.attempts", event.attempts as i64),
            KeyValue::new("llm_unify.cache_hit", event.cache_hit),
        ];
        let tracer = opentelemetry::global::tracer("llm-unify");
        let now = std::time::SystemTime::now();
        let mut span = tracer
            .span_builder("gen_ai.chat")
            .with_start_time(now.checked_sub(event.elapsed).unwrap_or(now))
            .start(&tracer);
        span.set_attributes(attributes.clone());
        if let Some(kind) = event.error {
            span.set_attribute(KeyValue::new("error.type", format!("{kind:?}")));
        }
        let meter = opentelemetry::global::meter("llm-unify");
        meter
            .f64_histogram("gen_ai.client.operation.duration")
            .with_unit("s")
            .build()
            .record(event.elapsed.as_secs_f64(), &attributes);
        if let Some(usage) = &event.usage {
            let counter = meter.u64_counter("gen_ai.client.token.usage").build();
            let mut a = attributes.clone();
            a.push(KeyValue::new("gen_ai.token.type", "input"));
            counter.add(usage.input_tokens, &a);
            a.pop();
            a.push(KeyValue::new("gen_ai.token.type", "output"));
            counter.add(usage.output_tokens, &a);
        }
        span.end();
    }
}
