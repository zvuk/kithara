<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-stream.svg)](https://crates.io/crates/kithara-stream)
[![docs.rs](https://docs.rs/kithara-stream/badge.svg)](https://docs.rs/kithara-stream)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-stream

Bridges async producers (network) to sync consumers (decoders). Exposes:

- the sync `Source` trait that decoders read through;
- the `Stream<T>` wrapper that gives `Source` a `Read + Seek` shape;
- the pull-driven `Downloader` (struct) and `Peer` (trait) — the workspace's unified HTTP transport;
- the canonical media vocabulary: `AudioCodec`, `ContainerFormat`, `MediaInfo`, used by every crate that talks about codecs or containers.

## Role

Architectural waist for bytes entering the decoder: protocol crates implement
`StreamType` and `dl::Peer`, while decoder/audio crates consume `Stream<T>`.
`AudioCodec`, `ContainerFormat`, and `MediaInfo` are defined here and re-exported
elsewhere.

## Key Entry Points

- `Source` — sync random-access surface for decoders (`wait_range`, `read_at`, `position`, `len`, `media_info`, `byte_map`, `variant_control`, `abr_handle`).
- `Stream<T>` — `Read + Seek` wrapper around any `T: StreamType`.
- `StreamType` — marker for protocol types (`File`, `Hls`) with associated `Config` and `Events`.
- `dl::Downloader` / `dl::Peer` / `dl::PeerHandle` / `dl::FetchCmd` — shared HTTP pool and pull-driven per-track transport.
- `AudioCodec` / `ContainerFormat` / `MediaInfo` — canonical media vocabulary, single source of truth.

## Usage

```rust
use kithara_stream::{Stream, StreamType};
use kithara_file::File;

// `File` and `Hls` implement `StreamType`.
let stream = Stream::<File>::new(config).await?;
// `stream` implements `Read + Seek` via the underlying `Source`.
```

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
