<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/kithara-abr.svg)](https://crates.io/crates/kithara-abr)
[![docs.rs](https://docs.rs/kithara-abr/badge.svg)](https://docs.rs/kithara-abr)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-abr

Protocol-agnostic adaptive bitrate (ABR) algorithm. Provides `AbrController` with auto (throughput-based) and manual modes, buffer-aware switching decisions, and configurable hysteresis to prevent oscillation.

## Usage

```rust
use kithara_abr::{AbrController, AbrOptions, AbrMode, Variant};
use kithara_platform::time::Instant;

let opts = AbrOptions {
    mode: AbrMode::Auto(Some(0)),
    variants: vec![
        Variant { variant_index: 0, bandwidth_bps: 500_000 },
        Variant { variant_index: 1, bandwidth_bps: 1_000_000 },
    ],
    ..Default::default()
};
let controller = AbrController::new(opts);
let decision = controller.decide(Instant::now());
assert_eq!(decision.target_variant_index, 0);
```

## Decision Logic

1. If in Manual mode → return current variant (`ManualOverride`).
2. If less than `min_switch_interval` (default 30 s) since last switch → `MinInterval`.
3. Estimate throughput via dual-track EWMA.
4. Select highest-bandwidth variant not exceeding `estimate / safety_factor`.
5. **Up-switch**: requires buffer ≥ `min_buffer_for_up_switch_secs` (10 s) AND throughput ≥ `candidate_bw × up_hysteresis_ratio` (1.3×).
6. **Down-switch**: triggered by buffer ≤ `down_switch_buffer_secs` (5 s) OR throughput ≤ `current_bw × down_hysteresis_ratio` (0.8×).

## Throughput Estimation

Uses dual-track Exponentially Weighted Moving Average (EWMA):

- **Fast** (2 s half-life): responds quickly to changes.
- **Slow** (10 s half-life): provides stability.
- Final estimate = `min(fast, slow)` — conservative approach.

Samples below 16,000 bytes are filtered as noise. Cache hits set `initial_bps = 100 Mbps` to allow immediate quality upgrades.

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>AbrController&lt;E&gt;</code></td><td>Main controller with Auto/Manual modes</td></tr>
<tr><td><code>AbrOptions</code></td><td>Full configuration: hysteresis ratios, buffer thresholds, mode, variants</td></tr>
<tr><td><code>AbrDecision</code></td><td>Decision result: target variant index, reason, changed flag</td></tr>
<tr><td><code>AbrReason</code></td><td>Why the decision was made (UpSwitch, DownSwitch, MinInterval, etc.)</td></tr>
<tr><td><code>Variant</code></td><td>Variant descriptor: index + bandwidth in bps</td></tr>
<tr><td><code>ThroughputSample</code></td><td>Measurement: bytes, duration, timestamp, source</td></tr>
<tr><td><code>Estimator</code> (trait)</td><td>Pluggable estimation strategy</td></tr>
</table>

## Integration

Used by `kithara-hls` for variant selection. Fully independent of HLS specifics -- can be used with any adaptive streaming protocol.

## Module layout

```
src/
├── abr.rs          — Abr trait (per-peer ABR capability surface)
├── controller/
│   ├── core.rs     — AbrController struct + AbrSettings + AbrPeerId
│   │                 + lifecycle (new, with_estimator, register, unregister)
│   │                 + peer-state callbacks (on_locked / on_unlocked /
│   │                   on_mode_changed / on_max_bandwidth_cap_changed)
│   ├── tick.rs     — record_bandwidth + tick (decision orchestration)
│   ├── throttle.rs — EventThrottleCache + emit_throttled
│   ├── incoherence.rs — schedule_incoherence_watch + check_incoherence
│   └── peer.rs     — internal PeerEntry struct
├── estimator.rs    — ThroughputEstimator + Estimator trait + private Ewma helper
├── handle.rs       — AbrHandle (Drop-driven unregister; safe external API)
├── state/
│   ├── core.rs     — AbrState struct + accessors + commands (apply,
│   │                 set_mode / set_variants / lock / unlock / …)
│   ├── decision.rs — pure `evaluate(state, view, now) -> AbrDecision`
│   │                 (parallel compute + single tuple-match)
│   ├── view.rs     — AbrView<'a> (decision inputs)
│   └── error.rs    — AbrError
├── types.rs        — re-exports of cross-crate vocabulary owned by kithara-events
├── internal.rs     — feature-gated test/internal exports
└── lib.rs          — module declarations + public re-exports
```

## Architecture invariants

- **`AbrState::current_variant` has two legitimate writers**, both go through
  [`AbrState::apply`](src/state/core.rs):
  1. `controller::tick` when the auto-mode FSM picks a new variant;
  2. `kithara-hls` scheduler when the user manually selects a variant — HLS
     holds `Arc<AbrState>` and applies a `Manual` decision so the layout
     switch and the ABR state stay in sync.
  `kithara-hls`'s `HlsCoord` reads the variant via `Arc<AbrState>` (no
  cloneable `Arc<AtomicUsize>` handle is exposed) — see `redundant_accessors`
  in `xtask/src/arch/checks` for the rationale.
- **`AbrState::set_variant_for_test`** is a `#[cfg(any(test, feature = "internal"))]`
  escape hatch for HLS integration tests that want to reproduce a midstream
  variant switch without simulating a full ABR tick.

## Decision flow

```
Downloader → AbrController::record_bandwidth(peer_id, bytes, dur, source)
           → ThroughputEstimator::push_sample()
           → AbrController::tick(peer_id, now)
             ├─ peer.variants() / peer.progress()        (pull from peer)
             ├─ AbrView { estimate_bps, buffer, … }
             ├─ state::decision::evaluate(state, view, now)
             │  ├─ Phase 1: parallel compute (5 independent let-bindings)
             │  ├─ Phase 2: single tuple-match → AbrDecision
             │  └─ Phase 3: bandwidth-aware up_switch / down_switch
             ├─ if changed: AbrState::apply()
             │              + bus.publish(AbrEvent::VariantApplied)
             │              + schedule_incoherence_watch (5 s deadline)
             └─ else: bus.publish(AbrEvent::DecisionSkipped)
```

The decision function avoids a heterogeneous guard-cascade for branch
prediction reasons — see `xtask/src/idioms/checks/guard_cascade.rs` for
why and what NOT to do as a workaround.

## Benchmarking

Run Criterion microbenchmarks for ABR estimator/decision hot paths:

```bash
cargo bench -p kithara-abr --bench abr_estimator
```
