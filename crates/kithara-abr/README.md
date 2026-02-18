<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-abr.svg)](https://crates.io/crates/kithara-abr)
[![Downloads](https://img.shields.io/crates/d/kithara-abr.svg)](https://crates.io/crates/kithara-abr)
[![docs.rs](https://docs.rs/kithara-abr/badge.svg)](https://docs.rs/kithara-abr)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

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
