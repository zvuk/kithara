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
    time::{Duration, Instant},
};

use tracing::{Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// One recorded probe event.
#[derive(Clone)]
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

/// Probe-event keys auto-injected by the `#[kithara::probe]` macro
/// (counter sequencing and call-site location). Pulled out of the
/// custom `Debug` impl so the dump skips the noise and shows only
/// what the probe site actually carried.
mod debug_keys {
    pub(super) const NUMERIC_SERVICE: &[&str] = &["seq", "thread_id", "thread_seq", "caller_line"];
    pub(super) const STRING_SERVICE: &[&str] = &["probe", "caller_file", "caller_fn"];
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "trace dump intentionally omits `at` (timestamp) and `target` \
              (always `<crate>_probe`) — probe events are read by humans \
              scanning per-thread sequences, not deserialized; including \
              these fields buries the signal."
)]
impl std::fmt::Debug for ProbeEvent {
    /// Print only the fields that are actually present. Probes carry
    /// different argument shapes (one fires with `variant`+`segment_index`,
    /// another with `request_id`+`bytes_transferred`); a derived `Debug`
    /// would dump the full `HashMap` payload — printing every probe with
    /// every possible field name regardless of whether the probe carried
    /// it makes traces unreadable. This impl shows just the keys that the
    /// probe set, in the order:
    ///
    /// 1. header — `probe`, `thread_id`, `thread_seq`, `seq`;
    /// 2. user numeric fields (sorted alphabetically);
    /// 3. `caller_fn` if the probe was declared with `caller` and the
    ///    backtrace resolved (empty placeholders are suppressed);
    /// 4. user string fields (sorted alphabetically).
    ///
    /// Service keys (`caller_file`, `caller_line`) are not part of the
    /// dump — they are noisy and rarely useful when reading per-thread
    /// sequences. If you need them, read `event.fields` / `event.string_fields`
    /// directly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("ProbeEvent");
        if let Some(name) = self.probe_name() {
            s.field("probe", &name);
        }
        if let Some(thread_id) = self.thread_id() {
            s.field("thread_id", &thread_id);
        }
        if let Some(thread_seq) = self.thread_seq() {
            s.field("thread_seq", &thread_seq);
        }
        if let Some(seq) = self.seq() {
            s.field("seq", &seq);
        }
        let mut numeric_keys: Vec<&String> = self
            .fields
            .keys()
            .filter(|k| !debug_keys::NUMERIC_SERVICE.contains(&k.as_str()))
            .collect();
        numeric_keys.sort();
        for key in numeric_keys {
            if let Some(value) = self.fields.get(key) {
                s.field(key, value);
            }
        }
        if let Some(caller_fn) = self.caller_fn() {
            s.field("caller_fn", &caller_fn);
        }
        let mut string_keys: Vec<&String> = self
            .string_fields
            .keys()
            .filter(|k| !debug_keys::STRING_SERVICE.contains(&k.as_str()))
            .collect();
        string_keys.sort();
        for key in string_keys {
            if let Some(value) = self.string_fields.get(key) {
                s.field(key, value);
            }
        }
        s.finish()
    }
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

    /// Read a string field, e.g. `caller_file` set automatically by the
    /// `#[kithara::probe]` expansion.
    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.string_fields.get(key).map(String::as_str)
    }

    /// Demangled symbol name of the function that called this probe.
    ///
    /// Resolved at firing time via
    /// `kithara_test_utils::probes::caller_fn_above` (backtrace +
    /// rustc demangling, with the probe machinery and the
    /// probe-attributed frame itself filtered out). Available only on
    /// non-wasm targets — empty string on wasm32.
    ///
    /// Tests should prefer this over `caller_file()` for call-site
    /// assertions: the symbol name survives file moves and renames,
    /// `caller_file` does not. Returns `None` when the macro could
    /// not resolve the caller (older expansion, debug info stripped,
    /// or an empty placeholder string was recorded).
    #[must_use]
    pub fn caller_fn(&self) -> Option<&str> {
        let value = self.str("caller_fn")?;
        if value.is_empty() { None } else { Some(value) }
    }

    /// File path of the call site that triggered this probe.
    ///
    /// Populated by `#[kithara::probe]` via `Location::caller()` paired
    /// with the macro-injected `#[track_caller]` attribute, so the
    /// recorded file is the caller's, not the probe-attributed
    /// function's own definition. Returns `None` when the probe was
    /// emitted by an older expansion or by the `probe_return` path
    /// (which uses the derived `Probe::record_probe` trait method
    /// without a caller hook).
    #[must_use]
    pub fn caller_file(&self) -> Option<&str> {
        self.str("caller_file")
    }

    /// Line of the call site that triggered this probe. Pairs with
    /// [`Self::caller_file`].
    #[must_use]
    pub fn caller_line(&self) -> Option<u64> {
        self.u64("caller_line")
    }

    /// Process-wide monotonic sequence number for this probe firing.
    ///
    /// Increments via `kithara_test_utils::probes::next_probe_seq()`
    /// each time `#[kithara::probe]` emits a tracing event. Tests can
    /// sort by `seq` to obtain a deterministic ordering even when two
    /// firings share the same `Instant`.
    #[must_use]
    pub fn seq(&self) -> Option<u64> {
        self.u64("seq")
    }

    /// Hashed identifier of the OS thread that fired this probe.
    ///
    /// Pair with [`Self::thread_seq`] to reconstruct each thread's own
    /// call order. Two probes with the same `thread_id` come from the
    /// same OS thread (modulo a 64-bit hash collision); filtering by
    /// `thread_id` gives a single thread's slice of the global trace.
    #[must_use]
    pub fn thread_id(&self) -> Option<u64> {
        self.u64("thread_id")
    }

    /// Per-thread monotonic sequence number for this probe firing.
    ///
    /// Independent of [`Self::seq`]: thread A's first probe and thread
    /// B's first probe both have `thread_seq=0`, but distinct
    /// `thread_id`s. Use this to assert "the i-th probe on thread T
    /// was X with args (...)" without the global ordering noise from
    /// unrelated threads.
    #[must_use]
    pub fn thread_seq(&self) -> Option<u64> {
        self.u64("thread_seq")
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
/// Cross-test isolation relies on the **install-id task-local** that
/// `#[kithara::test]` enters before the test body runs: every probe
/// firing stamps the owning `install_id` into its tracing event, and
/// this recorder filters by that id. Tasks spawned via `tokio::spawn`
/// inside the test inherit the task-local automatically; orphan
/// tasks from a just-finished test (downloader on-complete, audio
/// worker draining its last buffer) carry the *previous* test's id
/// and fall out of the new recorder's snapshot — even though they
/// emit after the new test's `install()`.
///
/// The global log is intentionally not drained: under stress runs
/// with multiple recorder lifetimes, draining could wipe an
/// actively-recorded sibling's events. The id filter alone is
/// sufficient. The timestamp filter (`start_at`) handles the small
/// window between scope entry and recorder construction.
#[must_use]
pub fn install() -> Recorder {
    Recorder {
        log: shared_log(),
        start_at: Instant::now(),
        install_id: crate::probes::current_install_id(),
    }
}

/// Per-test snapshot handle.
#[derive(Clone)]
pub struct Recorder {
    start_at: Instant,
    install_id: u64,
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
    ///
    /// Filters by `install_id` first (excludes orphan-task events from
    /// prior tests that outlive their `Drop`) and `start_at` second
    /// (excludes anything emitted before the recorder was anchored —
    /// possible if the install-id counter was bumped before drain
    /// completed).
    #[must_use]
    pub fn snapshot(&self) -> Vec<ProbeEvent> {
        self.log
            .lock()
            .expect("probe log poisoned")
            .iter()
            .filter(|e| e.u64("install_id") == Some(self.install_id) && e.at >= self.start_at)
            .cloned()
            .collect()
    }

    /// Wall-clock instant the recorder began capturing.
    #[must_use]
    pub fn start_at(&self) -> Instant {
        self.start_at
    }

    /// Block until a probe event matches `predicate` or `budget` elapses.
    ///
    /// Tests should drive their progress through this, not through
    /// time-based polling loops on `Audio::read` / `Stream::len()`.
    /// Each `#[kithara::probe]` site emits a `seq`-stamped event the
    /// moment the production code reaches it; `wait_for_probe` makes
    /// that arrival the test's clock instead of wall time.
    ///
    /// Returns the first event recorded by this recorder for which
    /// `predicate` returns `true`. Includes events that arrived *before*
    /// this call (so a probe that fires before the first poll isn't lost
    /// to a race). Returns `None` only when `budget` elapsed without a
    /// match.
    ///
    /// `budget` should be a real budget — fail the test if it elapses
    /// rather than masking a hang. Polls every 5 ms inside the budget.
    pub fn wait_for_probe<F>(&self, predicate: F, budget: Duration) -> Option<ProbeEvent>
    where
        F: Fn(&ProbeEvent) -> bool,
    {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(evt) = self.snapshot().into_iter().find(|e| predicate(e)) {
                return Some(evt);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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
        let formatted = format!("{value:?}");
        if let Ok(parsed) = formatted.parse::<u64>() {
            self.numeric.insert(field.name().to_string(), parsed);
        } else if let Ok(parsed) = formatted.parse::<i64>() {
            if parsed >= 0 {
                self.numeric
                    .insert(field.name().to_string(), u64::try_from(parsed).unwrap_or(0));
            } else {
                self.strings.insert(field.name().to_string(), formatted);
            }
        } else {
            self.strings.insert(field.name().to_string(), formatted);
        }
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
