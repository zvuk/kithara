<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-broadcast

Live HLS packaging core. It frames AAC-LC access units as ADTS behind the RFC 8216 §3.4 timestamp tag, rotates segments on the media clock, and keeps a sliding playlist window whose snapshot carries the rendered media playlist and the segments a client can still fetch. Segments live in memory as `bytes::Bytes`.

## Usage

```rust
use kithara_broadcast::{BroadcastConfig, LiveWindow, Segmenter};

let config = BroadcastConfig::builder().build();
let mut segmenter = Segmenter::new(&config)?;
let mut window = LiveWindow::new(&config)?;

for unit in encoder.push(&samples)? {
    if let Some(segment) = segmenter.push(&unit)? {
        window.push(segment);
    }
}

let snapshot = window.snapshot();
```

## Key types

- `BroadcastConfig` — the audio and the segments the packager cuts.
- `Segmenter` — ADTS framing plus segment rotation on the media clock.
- `Segment` — one closed segment: sequence number, bytes, duration, discontinuity flag.
- `LiveWindow` — sole owner of the playlist window, its retention, and the playlist text.
- `PlaylistSnapshot` — value view of the stream: playlist text, fetchable segments, end-of-stream flag.

Takes access units from `kithara-encode`. Speaks no HTTP and owns no threads.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
