//! Probe event capture helper for integration tests.
//!
//! Records every `tracing::event!` whose target ends with `_probe` —
//! by convention all `#[kithara::probe]` expansions emit to a target
//! named `<crate_name>_probe` (e.g. `kithara_stream_probe`,
//! `kithara_hls_probe`). The recorded events are kept in a process-wide
//! `Vec` so a test can snapshot the full sequence and assert on it.
//!
//! # Why a tracing layer (and not just `EventBus`)
//!
//! `kithara_events::EventBus` is a `tokio::sync::broadcast` — under
//! load, lagged subscribers drop events. Probes fire at the decision
//! site itself and the tracing Layer records every emission without a
//! bounded channel — defense in depth.
//!
//! # Why a process-wide subscriber
//!
//! `tracing::subscriber::set_default` is thread-local. Probes fire on
//! tokio runtime worker threads spawned by `Downloader::run`, which
//! don't inherit a per-test default subscriber. We therefore install a
//! single global subscriber and filter captured events per-test by
//! start timestamp.
//!
//! Because the `kithara::test` macro initialises a global tracing
//! subscriber via `setup_tracing_with_filter`, the probe layer must be
//! attached **inside that init path** — not as a separate
//! `set_global_default` call (which would fail with `SetGlobalDefault`).
//! [`fixtures::init_tracing`](crate::fixtures::init_tracing) installs
//! the probe layer alongside the fmt layer.
//!
//! Tests that capture probes should be `#[serial]` (via `serial_test`)
//! so concurrent runs do not pollute the shared recorder.
//!
//! # Activation
//!
//! Probe sites compile to no-ops unless `kithara-stream/usdt-probes`
//! and/or `kithara-hls/usdt-probes` are enabled in the test build.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use tracing::{Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// One recorded probe event.
#[derive(Clone, Debug)]
pub struct ProbeEvent {
    /// Numeric / boolean fields, keyed by name.
    pub fields: HashMap<String, u64>,
    /// Field values that arrived as strings (e.g. `probe = "enqueued"`).
    pub string_fields: HashMap<String, String>,
    /// Wall-clock timestamp of the probe firing.
    pub at: Instant,
    /// Target string of the captured tracing event (e.g.
    /// `"kithara_stream_probe"`, `"kithara_hls_probe"`).
    pub target: String,
}

impl ProbeEvent {
    /// The `probe` discriminator field (e.g. `"enqueued"`,
    /// `"fetch_cmd_emitted"`).
    #[must_use]
    pub fn probe_name(&self) -> Option<&str> {
        self.string_fields.get("probe").map(String::as_str)
    }

    /// Read a `u64` field (`segment_index`, `request_id`, …).
    #[must_use]
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.fields.get(key).copied()
    }
}

type SharedLog = Arc<Mutex<Vec<ProbeEvent>>>;

static GLOBAL_LOG: OnceLock<SharedLog> = OnceLock::new();

/// Lazily allocate (or fetch) the process-wide probe log. Called by
/// [`fixtures::init_tracing`](crate::fixtures::init_tracing) when it
/// constructs the global subscriber, and by [`install`] when a test
/// requests a recorder. Never sets a global subscriber by itself.
pub(crate) fn shared_log() -> SharedLog {
    GLOBAL_LOG
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

/// Build a `tracing_subscriber` `Layer` that captures probe events
/// into the shared log. Composed into the global subscriber by
/// [`fixtures::init_tracing`](crate::fixtures::init_tracing).
pub(crate) fn probe_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    ProbeLayer { log: shared_log() }
}

/// Snapshot handle anchored at the moment of installation.
///
/// `snapshot()` returns events captured at or after `start_at`,
/// filtering out events from earlier tests that landed in the shared
/// log before this recorder was created.
#[must_use]
pub fn install() -> Recorder {
    Recorder {
        log: shared_log(),
        start_at: Instant::now(),
    }
}

/// Per-test snapshot handle.
#[derive(Clone)]
pub struct Recorder {
    start_at: Instant,
    log: SharedLog,
}

impl Recorder {
    /// Filter snapshot by `probe = ...` discriminator.
    #[must_use]
    pub fn events_with_probe(&self, name: &str) -> Vec<ProbeEvent> {
        self.snapshot()
            .into_iter()
            .filter(|e| e.probe_name() == Some(name))
            .collect()
    }

    /// All events recorded since this `Recorder` was created.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ProbeEvent> {
        self.log
            .lock()
            .expect("probe log poisoned")
            .iter()
            .filter(|e| e.at >= self.start_at)
            .cloned()
            .collect()
    }

    /// Wall-clock instant the recorder began capturing.
    #[must_use]
    pub fn start_at(&self) -> Instant {
        self.start_at
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
    numeric: HashMap<String, u64>,
    strings: HashMap<String, String>,
}

impl Visit for ProbeVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.numeric
            .insert(field.name().to_string(), u64::from(value));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.strings
            .insert(field.name().to_string(), format!("{value:?}"));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if value >= 0 {
            self.numeric
                .insert(field.name().to_string(), u64::try_from(value).unwrap_or(0));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.strings
            .insert(field.name().to_string(), value.to_string());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.numeric.insert(field.name().to_string(), value);
    }
}
