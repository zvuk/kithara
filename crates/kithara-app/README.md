<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-app

Workspace application crate (`publish = false`) that wires demo binaries around shared engine/UI crates.

## Binary

Single binary `kithara` — the desktop DJ studio.

## Run

```bash
cargo run -p kithara-app -- <TRACK_URL_1> <TRACK_URL_2>
```

If no tracks are provided, the app loads built-in defaults that include MP3, HLS,
and DRM-HLS examples.

## Features

- `gui` — desktop GUI player (iced).
- `lib-only` — build as a plain library (used by integration tests).
- `beat-nn` — NN beat/downbeat detection.
- `stretch-signalsmith` / `stretch-bungee` / `stretch-all` — time-stretch backends.
- `client-reqwest` / `client-wreq` — HTTP backend forwarding.
- `tls-rustls` / `tls-native` — TLS backend forwarding.

Defaults: `gui` + `beat-nn` + `stretch-signalsmith`.

## Architecture

```mermaid
flowchart LR
    cli["kithara binary"] --> gui["kithara-app::gui"]
    gui --> core["kithara::PlayerImpl"]
```

## Track Analysis Cache

DJ Studio memoizes whole-track waveform and beat/BPM analysis in memory and on
disk. Runtime freshness is guarded by `TrackId`; cross-session cache identity is
owned by `AnalysisKey`. See CONTEXT.md for the key spaces, disk-tier lifecycle,
and `ANALYSIS_BYTES_VERSION` invalidation contract.

## Integration

- Depends on `kithara` with `file` + `hls` features.
- The GUI frontend is gated by the `gui` Cargo feature.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
