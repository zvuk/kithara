<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-broadcast

Live HLS packaging core. It frames AAC-LC access units as ADTS, rotates segments on the media clock, and keeps a sliding playlist window whose snapshot carries the rendered media playlist and the segments a client can still fetch. Segments live in memory as `bytes::Bytes`.

## Usage

```rust
use std::time::Duration;
use kithara_broadcast::{LiveWindow, Segmenter};

let mut segmenter = Segmenter::new(48_000, 2, 48_000, Segmenter::TARGET)?;
let mut window = LiveWindow::new(LiveWindow::WINDOW, LiveWindow::GRACE, 48_000)?;

for unit in encoder.push(&samples)? {
    if let Some(segment) = segmenter.push(&unit)? {
        window.push(segment);
    }
}

let snapshot = window.snapshot();
```

## Key types

- `Segmenter` — ADTS framing plus segment rotation on the media clock.
- `Segment` — one closed segment: sequence number, bytes, duration, discontinuity flag.
- `LiveWindow` — sole owner of the playlist window, its retention, and the playlist text.
- `PlaylistSnapshot` — value view of the stream: playlist text, fetchable segments, end-of-stream flag.

Takes access units from `kithara-encode`. Speaks no HTTP and owns no threads.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
