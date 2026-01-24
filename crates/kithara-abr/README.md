# kithara-abr

Protocol-agnostic Adaptive Bitrate (ABR) streaming algorithm.

## Overview

`kithara-abr` provides a throughput-based ABR controller for adaptive streaming applications. It's designed to be completely independent of any specific streaming protocol (HLS, DASH, etc.) through trait abstraction.

## Features

- **Protocol-agnostic**: Works with any streaming protocol via `VariantSource` trait
- **Auto and Manual modes**: Supports both automatic adaptation and fixed quality selection
- **Throughput-based decisions**: Uses network throughput estimation for quality switching
- **Buffer-aware**: Considers buffer level to prevent rebuffering
- **Configurable**: Extensive configuration options for tuning switching behavior
- **Zero dependencies**: Minimal external dependencies (only uses std library)

## Architecture

The ABR controller makes decisions based on:
- Network throughput (bytes/second)
- Buffer level (seconds of content)
- Variant bitrates (bits/second)
- Switching hysteresis to prevent oscillation

## Usage

```rust
use kithara_abr::{AbrController, AbrOptions, AbrMode, VariantSource};
use std::time::Instant;

// Define your variants (e.g., from HLS master playlist)
struct MyVariants {
    bandwidths: Vec<u64>,
}

impl VariantSource for MyVariants {
    fn variant_count(&self) -> usize {
        self.bandwidths.len()
    }

    fn variant_bandwidth(&self, index: usize) -> Option<u64> {
        self.bandwidths.get(index).copied()
    }
}

// Create ABR controller in Auto mode
let opts = AbrOptions {
    mode: AbrMode::Auto(Some(0)), // Start with lowest quality
    ..Default::default()
};
let mut controller = AbrController::new(opts);

// Push throughput samples as segments download
controller.push_throughput_sample(sample);

// Get ABR decision
let variants = MyVariants {
    bandwidths: vec![500_000, 1_000_000, 2_000_000],
};
let decision = controller.decide(&variants, Instant::now());

// Apply the decision
if decision.changed {
    println!("Switch to variant {} (reason: {:?})",
        decision.target_variant_index, decision.reason);
}
```

## Manual Control

```rust
// Switch to manual mode with specific variant
controller.set_manual_variant(1);

// Switch back to auto mode
controller.set_auto_mode(Some(1)); // Start from variant 1

// Check current mode
if controller.is_auto() {
    println!("ABR is in automatic mode");
}
```

## Configuration

The `AbrOptions` struct provides fine-grained control over ABR behavior:

- `mode`: Auto or Manual mode
- `min_buffer_for_up_switch_secs`: Minimum buffer level required for quality upgrade
- `down_switch_buffer_secs`: Buffer level that triggers quality downgrade
- `throughput_safety_factor`: Safety margin for throughput estimation (e.g., 1.5 = use 66% of estimated)
- `up_hysteresis_ratio`: Throughput must exceed target by this factor to upgrade
- `down_hysteresis_ratio`: Throughput must fall below target by this factor to downgrade
- `min_switch_interval`: Minimum time between quality switches
- `sample_window`: Time window for throughput estimation

## Integration with Streaming Protocols

### HLS Example

```rust
use kithara_abr::VariantSource;
use hls_m3u8::MasterPlaylist;

impl VariantSource for MasterPlaylist {
    fn variant_count(&self) -> usize {
        self.variants.len()
    }

    fn variant_bandwidth(&self, index: usize) -> Option<u64> {
        self.variants
            .iter()
            .find(|v| v.id == index)
            .and_then(|v| v.bandwidth)
    }
}
```

### DASH Example

```rust
impl VariantSource for DashMpd {
    fn variant_count(&self) -> usize {
        self.representations.len()
    }

    fn variant_bandwidth(&self, index: usize) -> Option<u64> {
        self.representations.get(index)
            .and_then(|r| r.bandwidth)
    }
}
```

## Design Philosophy

- **Keep it simple**: The algorithm uses straightforward throughput and buffer-based heuristics
- **Avoid oscillation**: Hysteresis and minimum switch intervals prevent rapid quality changes
- **Conservative estimates**: Safety factors ensure we don't overestimate network capacity
- **Protocol independence**: No assumptions about HLS, DASH, or any specific protocol

## License

Dual-licensed under MIT or Apache-2.0.
