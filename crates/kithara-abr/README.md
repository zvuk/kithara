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

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
