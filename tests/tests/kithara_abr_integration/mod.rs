//! Cross-crate integration tests for the ABR contract.
//!
//! These tests exercise `AbrState` / `AbrController` through the same
//! public entry points the stream and HLS peer use, without spinning up a
//! full HLS test server. The goal is fast, deterministic coverage of the
//! hard invariants called out in the ABR projection §12 Tier 4:
//!
//! * Three parallel peers holding independent state — no cross-track
//!   leak (`multi_track_contention`).
//! * `AbrEvent::VariantApplied` fires without a trailing `Incoherence`
//!   when the reader keeps advancing (`variant_switch_coherence`).
//! * Switch during a pending fetch must not leave orphan bytes on the
//!   shared state (`switch_midfetch`).
//! * Canonical bandwidth profiles produce the expected switch sequence
//!   (`bandwidth_scenarios_golden`).
//!
//! The SEEK-NO-SWITCH cases that used to live here moved into
//! `kithara-abr/src/state/tests.rs` so the crate no longer needs to
//! publish a `test_variants_3` helper.

mod bandwidth_scenarios_golden;
mod multi_track_contention;
mod switch_midfetch;
mod variant_switch_coherence;
