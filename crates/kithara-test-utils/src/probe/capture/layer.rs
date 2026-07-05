use std::collections::HashMap;

use kithara_platform::time::Instant;
use tracing::{Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

use super::{event::ProbeEvent, recorder::SharedLog};

/// Build a `tracing_subscriber` `Layer` that captures probe events
/// into the shared log. Composed into the global subscriber by
/// [`crate::test::init_tracing`].
#[must_use]
pub fn probe_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    ProbeLayer {
        log: super::recorder::shared_log(),
    }
}

struct ProbeLayer {
    log: SharedLog,
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for ProbeLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let target = event.metadata().target();
        if !target.ends_with("_probe") {
            return;
        }
        let mut visitor = ProbeVisitor::default();
        event.record(&mut visitor);
        let evt = ProbeEvent {
            target: target.to_string(),
            at: Instant::now(),
            fields: visitor.numeric,
            string_fields: visitor.strings,
        };
        if let Ok(mut log) = self.log.lock() {
            log.push(evt);
        }
    }
}

#[derive(Default)]
struct ProbeVisitor {
    numeric: HashMap<&'static str, u64>,
    strings: HashMap<&'static str, String>,
}

impl Visit for ProbeVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.numeric.insert(field.name(), u64::from(value));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{value:?}");
        if let Ok(parsed) = formatted.parse::<u64>() {
            self.numeric.insert(field.name(), parsed);
        } else if let Ok(parsed) = formatted.parse::<i64>() {
            if parsed >= 0 {
                self.numeric
                    .insert(field.name(), u64::try_from(parsed).unwrap_or(0));
            } else {
                self.strings.insert(field.name(), formatted);
            }
        } else {
            self.strings.insert(field.name(), formatted);
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if value >= 0 {
            self.numeric
                .insert(field.name(), u64::try_from(value).unwrap_or(0));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.strings.insert(field.name(), value.to_string());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.numeric.insert(field.name(), value);
    }
}
