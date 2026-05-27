<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-abr.svg)](https://crates.io/crates/kithara-abr)
[![docs.rs](https://docs.rs/kithara-abr/badge.svg)](https://docs.rs/kithara-abr)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-abr

Protocol-agnostic adaptive bitrate (ABR) for HLS and any other streaming source. `AbrController` offers auto (throughput-based) and manual modes, buffer-aware decisions, and configurable hysteresis to prevent oscillation.

## Usage

```rust
use std::sync::Arc;

use kithara_abr::{Abr, AbrController, AbrHandle, AbrSettings};

// Owned at the consumer-crate top (Queue / App / FFI player / PlayerImpl).
let controller: Arc<AbrController> = AbrController::new(AbrSettings::default());

// Each peer (HLS variant, file source) implements `Abr` and registers itself.
let handle: AbrHandle = controller.register(&(peer as Arc<dyn Abr>));

// Throughput samples flow back from the downloader.
controller.record_bandwidth(peer_id, bytes, duration, source);
```

`AbrController::new` returns `Arc<Self>`. The controller is registered with multiple peers via `register(peer)`; dropping the returned `AbrHandle` unregisters the peer automatically.

ABR decisions are pull-driven — they fire from the peer's scheduler on each fetch, not on a separate timer.

## Decision Logic

1. If in Manual mode → return current variant (`ManualOverride`).
2. If less than `min_switch_interval` (default 30 s) since last switch → `MinInterval`.
3. Estimate throughput via dual-track EWMA (seeded at construction from `initial_throughput_bps` if set).
4. Select highest-bandwidth variant not exceeding `estimate / throughput_safety_factor` (default 1.5).
5. **Up-switch**: requires buffer ≥ `min_buffer_for_up_switch` (10 s) AND throughput ≥ `candidate_bw × up_hysteresis_ratio` (1.3×).
6. **Down-switch**: triggered by buffer ≤ `urgent_downswitch_buffer` (5 s) OR throughput ≤ `current_bw × down_hysteresis_ratio` (0.8×).

## Initial Bandwidth Seed

`AbrSettings::initial_throughput_bps` lets the controller pick a sensible variant on the **first tick**, before any real bandwidth samples land. Default: `Some(2_000_000)` (≈2 Mbps) — covers Wi-Fi and most 4G; constrained networks (3G/EDGE) down-switch after the first real sample.

Without the seed (`None`), the historical cold-start path applies: `decide()` returns `AbrReason::NoEstimate` and the player stays on the initial variant until the first sample is recorded. This is the explicit opt-out for tests that want to reproduce the pre-refactor behaviour:

```rust
let settings = AbrSettings { initial_throughput_bps: None, ..AbrSettings::default() };
```

The seed is treated as a transient prior — real EWMA samples replace it via the `min(fast, slow)` consensus once they accumulate weight.

## Throughput Estimation

Uses dual-track Exponentially Weighted Moving Average (EWMA):

- **Fast** (2 s half-life): responds quickly to changes.
- **Slow** (10 s half-life): provides stability.
- Final estimate = `min(fast, slow)` — conservative approach.

Samples below 16,000 bytes are filtered as noise.

## Key Types

<table>
<tr><th>Type</th><th>Kind</th><th>Role</th></tr>
<tr><td><code>AbrController</code></td><td>struct (Arc)</td><td>Main controller; owns the estimator and per-peer registry; emits ABR events</td></tr>
<tr><td><code>Abr</code></td><td>trait</td><td>Per-peer ABR capability surface (variants, progress, current variant, locks)</td></tr>
<tr><td><code>AbrHandle</code></td><td>struct</td><td>Drop-driven unregister handle returned by <code>AbrController::register</code></td></tr>
<tr><td><code>AbrState</code> / <code>AbrView</code></td><td>structs</td><td>Owned state of a peer's ABR context and the view passed to <code>evaluate()</code></td></tr>
<tr><td><code>AbrSettings</code></td><td>struct</td><td>Configuration: hysteresis ratios, buffer thresholds, mode, initial-throughput seed, min-switch interval (defined in <code>kithara-events</code>, re-exported here)</td></tr>
<tr><td><code>AbrMode</code></td><td>enum</td><td><code>Auto(Option&lt;variant_index&gt;)</code> or <code>Manual(variant_index)</code></td></tr>
<tr><td><code>VariantInfo</code></td><td>struct</td><td>Variant descriptor — <code>bandwidth_bps</code>, codec, container, name, …</td></tr>
<tr><td><code>AbrDecision</code></td><td>struct</td><td>Decision result — target variant index, reason, changed flag</td></tr>
<tr><td><code>AbrReason</code></td><td>enum</td><td>Why the decision fired (UpSwitch, DownSwitch, MinInterval, NoEstimate, ManualOverride, …)</td></tr>
<tr><td><code>BandwidthSource</code></td><td>enum</td><td>Whether a sample came from a primary fetch or an on-demand range</td></tr>
<tr><td><code>AbrProgressSnapshot</code> / <code>VariantDuration</code> / <code>AbrPeerId</code></td><td>structs</td><td>Cross-crate vocabulary owned by <code>kithara-events</code></td></tr>
<tr><td><code>Estimator</code> / <code>ThroughputEstimator</code></td><td>trait / struct</td><td>Pluggable throughput estimation strategy (default: dual-track EWMA)</td></tr>
</table>

## Integration

Used by `kithara-hls` for variant selection. Fully independent of HLS specifics -- can be used with any adaptive streaming protocol.

## Module layout

```
src/
├── abr.rs           — Abr trait (per-peer ABR capability surface)
├── controller/
│   ├── core.rs      — AbrController struct + lifecycle (new, with_estimator,
│   │                  register, unregister) + peer-state callbacks
│   ├── tick.rs      — record_bandwidth + tick (decision orchestration)
│   ├── throttle.rs  — EventThrottleCache + emit_throttled
│   ├── incoherence.rs — schedule_incoherence_watch + check_incoherence
│   └── peer.rs      — internal PeerEntry struct
├── estimator.rs     — ThroughputEstimator + Estimator trait + private Ewma helper
├── handle.rs        — AbrHandle (Drop-driven unregister; safe external API)
├── state/
│   ├── core.rs      — AbrState struct + accessors + commands (apply,
│   │                  set_mode / set_variants / lock / unlock / …)
│   ├── decision.rs  — pure `evaluate(state, view, now) -> AbrDecision`
│   ├── view.rs      — AbrView<'a> (decision inputs)
│   ├── error.rs     — AbrError
│   └── tests.rs     — decision/lifecycle tests
├── types.rs         — re-exports of cross-crate vocabulary owned by kithara-events
└── lib.rs           — module declarations + public re-exports
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

Criterion microbenchmarks for ABR estimator/decision hot paths live in `kithara-integration-tests`:

```bash
cargo bench -p kithara-integration-tests --bench abr_estimator
```
