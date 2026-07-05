# kithara-abr — Context

Detailed contracts and invariants for the kithara-abr crate; the README is the overview.

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
│   ├── core.rs      — AbrState struct + accessors + commands (apply_decision,
│   │                  set_mode / lock / unlock / …)
│   ├── decision.rs  — pure `evaluate(state, view, now) -> AbrDecision`
│   ├── view.rs      — AbrView<'a> (decision inputs)
│   ├── error.rs     — AbrError
│   └── tests.rs     — decision/lifecycle tests
├── types.rs         — re-exports event vocabulary plus crate-owned controller/state types
└── lib.rs           — module declarations + public re-exports
```

## Architecture invariants

- **`AbrState::current_variant_index()` has two legitimate writers**, both go through
  [`AbrState::apply_decision`](src/state/core.rs):
  1. `controller::tick` when the auto-mode FSM picks a new variant;
  2. `kithara-hls` scheduler when the user manually selects a variant — HLS
     holds `Arc<AbrState>` and applies a `Manual` decision so the layout
     switch and the ABR state stay in sync.
  `kithara-hls`'s `HlsCoord` reads the variant via `Arc<AbrState>` (no
  cloneable `Arc<AtomicUsize>` handle is exposed) — see `redundant_accessors`
  in `crates/kithara-devtools/src/arch/checks` for the rationale.

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
             ├─ if changed: AbrState::apply_decision()
             │              + bus.publish(AbrEvent::VariantApplied)
             │              + schedule_incoherence_watch (5 s deadline)
             └─ else: bus.publish(AbrEvent::DecisionSkipped)
```

The decision function avoids a heterogeneous guard-cascade for branch
prediction reasons — see `crates/kithara-devtools/src/idioms/checks/guard_cascade.rs` for
why and what NOT to do as a workaround.

## Benchmarking

Criterion microbenchmarks for ABR estimator/decision hot paths live in `kithara-integration-tests`:

```bash
cargo bench -p kithara-integration-tests --bench abr_estimator
```
