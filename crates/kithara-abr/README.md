<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

# kithara-abr

Protocol-agnostic adaptive bitrate (ABR) algorithm. Provides `AbrController` with auto (throughput-based) and manual modes, buffer-aware switching decisions, and configurable hysteresis to prevent oscillation.

## Usage

```rust
use kithara_abr::{AbrController, AbrOptions, AbrMode, Variant};

let opts = AbrOptions {
    mode: AbrMode::Auto(Some(0)),
    variants: vec![
        Variant { variant_index: 0, bandwidth_bps: 500_000 },
        Variant { variant_index: 1, bandwidth_bps: 1_000_000 },
    ],
    ..Default::default()
};
let controller = AbrController::new(opts);
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

| Type | Role |
|------|------|
| `AbrController<E>` | Main controller with Auto/Manual modes |
| `AbrOptions` | Full configuration: hysteresis ratios, buffer thresholds, mode, variants |
| `AbrDecision` | Decision result: target variant index, reason, changed flag |
| `AbrReason` | Why the decision was made (UpSwitch, DownSwitch, MinInterval, etc.) |
| `Variant` | Variant descriptor: index + bandwidth in bps |
| `ThroughputSample` | Measurement: bytes, duration, timestamp, source |
| `Estimator` (trait) | Pluggable estimation strategy |

## Integration

Used by `kithara-hls` for variant selection. Fully independent of HLS specifics -- can be used with any adaptive streaming protocol.
