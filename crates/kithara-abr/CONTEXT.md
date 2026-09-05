# kithara-abr — Context

Detailed contracts and invariants for the kithara-abr crate; the README is the overview.

## Decision flow

`AbrController::record_bandwidth` → `Estimator::push_sample` → ABR `run_tick(peer_id, now)`:

1. Resolve the peer entry and upgrade its `Weak<dyn Abr>`; either lookup failing aborts the tick silently.
1. Pull `peer.variants()` and `peer.progress()`; `buffer_ahead = download_head_playback_time - reader_playback_time`.
1. Publish `VariantsRegistered` once per peer, then the throttled `BandwidthEstimate` / `BufferAhead` events.
1. `AbrState::decide(&AbrView, now)`.
1. `Stay { AlreadyOptimal }` → `retract_throughput_pending(current)`; `Stay { MinInterval }` → record the eligibility deadline on the peer; any other `Stay` → publish `DecisionSkipped`; a switch → `request_target(target, reason)` + `peer.wake()`.

The tick never publishes a variant change. Publication is a separate, boundary-driven step — see the pending protocol below.

## Decision logic

`decision::evaluate` = `decide()` plus an anti-oscillation post-gate.

`decide()`, in order:

1. Locked → `Stay { Locked }`.
1. `AbrMode::Manual(idx)` → `Stay { ManualOverride }` when `idx == current`, else `Manual { from, to }`.
1. No throughput estimate → `Stay { NoEstimate }`.
1. Candidates = variants whose bandwidth fits `max_bandwidth_bps`, sorted ascending. While escaping, `current` is removed. Empty → `Stay { AlreadyOptimal }`.
1. `adjusted = estimate_bps / throughput_safety_factor` (default 1.5). Candidate = highest-bandwidth variant at or under `adjusted`, else the lowest variant.
1. Escaping → `UpSwitch { EscapeStalled }`, bypassing the buffer and hysteresis gates: their premise (staying grows the buffer) is false for a variant that delivers nothing.
1. Cap set, `current_bw > cap`, candidate differs → `DownSwitch { DownSwitch }`.
1. `candidate_bw > current_bw` → up-switch when buffer (if known) ≥ `min_buffer_for_up_switch` (10 s) **and** `adjusted ≥ candidate_bw × up_hysteresis_ratio` (1.3); otherwise `Stay { BufferTooLowForUpSwitch }`.
1. `candidate_bw < current_bw` → `UrgentDownSwitch` when buffer ≤ `urgent_downswitch_buffer` (5 s), else `DownSwitch` when `adjusted ≤ current_bw × down_hysteresis_ratio` (0.8), else fall through.
1. Otherwise `Stay { AlreadyOptimal }`.

`min_switch_interval` (default 30 s) is applied **after** `decide()`, never before: urgency must be known before it is judged. Rescues (`EscapeStalled`, `UrgentDownSwitch`) and `Manual` mode are never held; anything else that would switch too soon becomes `Stay { MinInterval }`. Before the first switch the interval is measured from state construction, so one fast first segment cannot flip the variant under a listener who has barely started.

`Stay { MinInterval }` is a deferred decision, not a final verdict. The controller records one deadline for that peer at the exact remaining interval, so a completed download cannot leave the desired switch waiting forever for an unrelated future sample. The existing downloader loop polls the earliest ABR deadline inside its cancellation-safe `Registry::tick`; expiry requests the same canonical `tick`, never a cached target. Tick requests coalesce to one dirty bit per peer, while every bandwidth sample still reaches the estimator before the bit is set. No ABR task, peer timer, or ambient runtime lookup exists. Dropping the last `AbrHandle` removes the peer and its deadline.

`decide()` avoids a heterogeneous guard cascade (parallel computes, then one tuple-match) and `run_tick()` uses a single Option-resolver instead of a homogeneous let-else cascade — see `crates/kithara-devtools/src/idioms/checks/guard_cascade.rs` for why, and what NOT to do as a workaround.

## Pending protocol and publication authority

`AbrState` separates intent from publication.

- `request_target(target, reason)` records intent. Replace-pending, latest wins; a repeat request for the target already queued is a full no-op, ticket and reason included — the ticket names the exact transition a consumer has already built its incoming session and decoder against, so re-minting one would cancel that work on every tick and the switch could never finish. Under `Manual(idx)` a request for any other target is refused; the mode is read under the slot lock, pairing with the store-mode-then-clear-slot order in `set_mode`. The write happens regardless of lock state.
- `AbrTicket` identifies one accepted request. `pending_claim(current)` reports `Absent`, `Locked(claim)`, or `Ready(claim)`; a pending that already equals `current` reports `Absent`.
- `commit_pending(claim, now)` publishes only when the slot still carries the same ticket **and** rebuilds the same decision, and never while locked. `abort_pending(ticket)` drops only the matching request; a stale ticket leaves a newer intent untouched.
- `commit_pending` and `apply_decision` are the only writers of `current_variant`. Production HLS commits through the publisher at a segment boundary (`kithara-hls` `stream/transition.rs`); `apply_decision` is the direct publish path.
- `AbrPublisher` (minted by `AbrState::publisher()`) is the publication capability the state's owner keeps. Consumers receive `AbrHandle`, which observes and controls but cannot publish.
- `lock()` / `unlock()` are reentrant and gate publication only — the intent survives so it resumes on unlock. `invalidate_pending()` is the destructive counterpart, called on a semantic seek boundary: it drops throughput-driven intents (`UpSwitch`, `DownSwitch`, `UrgentDownSwitch`, `EscapeStalled`) and preserves `ManualOverride` / `Initial`, which a position jump does not invalidate.
- `retract_throughput_pending(current)` runs from the tick's `Stay { AlreadyOptimal }` arm — the one verdict that re-affirms `current` on live evidence. It drops a throughput-driven, non-rescue pending aimed elsewhere, so an urgent down-switch latched on the initial seed cannot outlive the estimate that justified it.
- `set_mode` clears the pending slot, except when a manual pin restates the target already queued.
- `selected_variant_for_seek()` returns the pending target when one exists, even while locked, so a seek replacement opens the variant the user is switching to.

### Escape

`mark_escape()` flags the active variant as non-delivering; the HLS stall detector sets it when the reader is parked at a clean boundary on a segment whose in-flight fetch crossed the downloader soft timeout. The caller must follow with `AbrHandle::reevaluate()`: the flag is set under the HLS state lock and the tick re-locks that state through `peer.progress()`, so the tick must fire outside the lock. Publishing a new variant clears escape **before** storing it, so a concurrent tick that observes the new `current_variant` also observes the cleared flag and cannot immediately re-escape off a variant that has not had a chance to deliver.

## Throughput estimation

Dual-track EWMA — fast (2 s half-life) and slow (10 s half-life); estimate = `min(fast, slow)`, conservative by construction.

- Samples below 16 000 bytes are dropped as noise, fetch duration is clamped to ≥ 0.5 ms, and a zero fetch duration drops the sample in `record_bandwidth` before it reaches the estimator.
- `BandwidthSource::Cache` never feeds the EWMAs: it pins the fallback estimate to 100 Mbps so a cache hit is not mistaken for network throughput.
- With no EWMA weight yet, the estimate falls back to the seed / cache value; `None` only when there is neither.

### Initial seed

`AbrSettings::initial_throughput_bps` (default `Some(2_000_000)`) is applied to the estimator at controller construction so the first tick can pick a sensible variant before a real sample lands. ≈2 Mbps covers Wi-Fi and most 4G; constrained networks down-switch after the first real sample. It is a transient prior — real EWMA weight replaces it through the `min(fast, slow)` consensus. Set it to `None` for the cold-start path: `decide()` returns `NoEstimate` and the peer stays on its initial variant until samples accumulate.

`AbrSettingsPatch` is the second entry point: a configuration document types into it and `apply` writes past the builder, so a document can compose a setting the builder would have refused. One thing it cannot express — `initial_throughput_bps` and `max_bandwidth_bps` are already `Option<u64>`, so a document sets a value but cannot blank one: `initial_throughput_bps: null` reads as "leave it alone", not as the cold-start path above. The cold start stays a builder-only choice.

`AbrSettings` is the facade configuration for the controller: it carries both algorithm parameters and injected resources such as the optional parent `CancelToken`. It is `#[non_exhaustive]` and built with `AbrSettings::builder()…build()` (`Default` goes through the builder); `initial_throughput_bps(Some(value))` sets the seed and `initial_throughput_bps(None)` explicitly disables it.

## Ownership

- Variants live on the peer (`Abr::variants()`), never in `AbrState`; they reach the decision through `AbrView`. `AbrState` owns only runtime control: current index, mode, lock count, escape flag, bandwidth cap, last-switch timestamp, pending slot.
- `Abr::cancel()` is mandatory and returns the protocol track/source token. The controller observes this token but never cancels it; HLS returns its stream token and File returns its source token.
- `AbrHandle::set_mode` validates `Manual(idx)` against the peer's live variant list and returns `AbrError::VariantOutOfBounds`; `AbrState::set_mode` does not validate.
- `AbrHandle` is the consumer surface; dropping the last clone unregisters the peer. The track-scoped `EventBus` lives on the handle (`with_bus`), so peers stay free of event-bus plumbing.
- `AbrController` owns a child scope of the optional parent carried by `AbrSettings`. Each registration derives one controller-owned child and OR-combines it with `Abr::cancel()` through `CancelGroup`; controller/parent cancellation stops every registration, protocol cancellation stops that track, and sibling registrations remain live.
- `Downloader::run` is the only async driver for ABR scheduling. `AbrController` stores coalesced tick requests, deadlines, and the downloader task's current waker; it never spawns a worker of its own.
- `AbrController::register(peer)` discovers protocol cancellation from the peer. Dropping the last `AbrHandle` cancels only the controller-owned registration child, so unregister never cancels the protocol track or the controller scope.
- `AbrHandle::notify_exact_commit` publishes `AbrEvent::VariantApplied` after a promotion and does nothing else. Its caller is the audio worker, which is not a runtime thread, so this path must not schedule async work.
- `kithara-hls` reads the variant through `AbrHandle` / `Arc<AbrState>`; no cloneable `Arc<AtomicUsize>` handle is exposed — see `redundant_accessors` in `crates/kithara-devtools/src/arch/checks` for the rationale.

## Event throttling

Per peer: `ThroughputSample` at most every `throughput_sample_min_interval` (default 200 ms); `BandwidthEstimate` when `bandwidth_emit_min_interval` has elapsed **or** the relative change reaches `bandwidth_emit_min_delta_ratio`; `BufferAhead` on any `Some`/`None` transition, otherwise when both the interval and the absolute-delta thresholds are met.

## Module layout

- `abr.rs` — `Abr` trait, the per-peer capability surface; `cancel()` is mandatory while optional ABR capabilities are defaulted so simple peers opt out.
- `controller/` — `core.rs` (`AbrController`, `AbrSettings`, `AbrPeerId`, registration, peer-state callbacks), `driver.rs` (coalesced tick readiness and deadlines driven by the downloader), `tick.rs`, `throttle.rs`, `peer.rs` (`PeerEntry`).
- `estimator.rs` — `Estimator` trait, `ThroughputEstimator`, private `Ewma`.
- `handle.rs` — `AbrHandle` (Drop-driven unregister).
- `state/` — `core.rs` (`AbrState`), `decision.rs` (pure `evaluate`), `view.rs`, `pending.rs` (`AbrTicket`, `PendingAbrClaim`, `PendingAbrDecision`), `publisher.rs` (`AbrPublisher`), `error.rs`, `tests.rs`.

## Benchmarking

Criterion microbenchmarks for the estimator/decision hot paths live in `kithara-integration-tests`:

```bash
cargo bench -p kithara-integration-tests --bench abr_estimator
```
