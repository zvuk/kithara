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

mod event;
mod layer;
mod recorder;

pub use event::ProbeEvent;
pub use layer::probe_layer;
pub use recorder::{Recorder, install};
