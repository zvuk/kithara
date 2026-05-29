//! Probe event capture helper for integration tests.
//!
//! Records every `tracing::event!` whose target ends with `_probe` into a
//! process-wide `Vec` so a test can snapshot the sequence and assert on it.
//! See the crate `README.md` "Probe capture" for the subscriber and
//! activation contract.

mod event;
mod layer;
mod recorder;

pub use event::ProbeEvent;
pub use layer::probe_layer;
pub use recorder::{Recorder, install};
