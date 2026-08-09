<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-broadcast

Live HLS origin. It encodes a PCM feed to AAC-LC, frames the access units as ADTS behind the RFC 8216 §3.4 timestamp tag, rotates segments on the media clock, keeps a sliding playlist window, and serves master playlist, media playlist, and segments over HTTP. Segments live in memory as `bytes::Bytes`.

## Usage

```rust
use kithara_broadcast::{Broadcast, BroadcastConfig};

let config = BroadcastConfig::builder().sample_rate(48_000).channels(2).build();
let handle = Broadcast::start(config, feed, Some(parent))?;

println!("on air at {}", handle.url());
handle.stop();
```

The packaging core is usable on its own — `Segmenter` and `LiveWindow` take the same config and own no threads:

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

- `BroadcastConfig` — the audio, the segments, and the address the origin binds.
- `Broadcast` / `BroadcastHandle` — the live service: URL, status, and the graceful end of the broadcast.
- `LivePcmFeed` — the PCM seam: non-blocking interleaved f32 plus the gap behind it, and the close that draws the line under the feed.
- `RingFeed` — that seam over a `ringbuf` consumer and the producer's drop counter.
- `Segmenter` — ADTS framing plus segment rotation on the media clock.
- `Segment` — one closed segment: sequence number, bytes, duration, discontinuity flag.
- `LiveWindow` — sole owner of the playlist window, its retention, and the playlist text.
- `PlaylistSnapshot` — value view of the stream: playlist text, fetchable segments, end-of-stream flag.

Takes access units from `kithara-encode`.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
