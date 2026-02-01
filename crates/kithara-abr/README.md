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

## Integration

Used by `kithara-hls` for variant selection. Fully independent of HLS specifics -- can be used with any adaptive streaming protocol.
