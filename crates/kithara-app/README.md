<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-app

The desktop DJ demo built on the engine: two decks with waveforms, BPM and
beat sync, a mixer with EQ and crossfader, tempo with key lock, and a track
library. It also builds for the browser (WebGPU).

## Usage

```bash
cargo run -p kithara-app --release -- <FILE_OR_URL> [<FILE_OR_URL> ...]
cargo run -p kithara-app --release -- --help
```

Without arguments the app plays the playlist from its configuration.

### Configuration

The configuration is `app.yaml`, embedded at build time. A `kithara.yaml`
beside the executable, or a file passed with `--config`, is laid over it:
mappings merge key by key, other values replace what they cover.
`--dump-config` prints the result. Values written as `$KITHARA_...` are read
from the environment or from `.env` in the workspace root at build time. A
DRM provider whose values are missing is turned off with a warning; any other
missing value stops startup.

### Browser

```bash
just platform wasm build --package kithara-app
```

Builds `dist/` for a WebGPU browser. Serve it with cross-origin isolation
(`Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Embedder-Policy: require-corp`); `dist/_headers` sets both on
hosts that read it. The browser build lays `app.web.yaml` over `app.yaml`: no
DRM providers, no catalogue account, and a playlist of cross-origin streams.

## Features

- `gui` (default) — the iced desktop UI; `masonry` adds the retained host
  (`--host retained`).
- `analysis-waveform` (default), `beat-nn-small` (default), `beat-nn-full`,
  `beat-nn-full-int8`, `beat-dsp` — waveform and beat detection.
- `stretch-signalsmith` (default), `stretch-bungee`, `stretch-identity`,
  `stretch-all` — time-stretch engines.
- `zvuk` — Zvuk catalogue: search, liked tracks, playlists; `keystore` keeps
  its token in the OS keychain.
- `broadcast` — serve the master output as a live HLS stream.
- `client-reqwest` / `client-wreq`, `tls-rustls` / `tls-native` — HTTP and TLS.

## Integration

The app only composes and sends commands; playback, mixing, and analysis are
engine features. It caches waveform and beat analysis in memory and on disk
through the shared `AssetStore`. See
[crate contracts](https://github.com/zvuk/kithara/wiki/kithara-app) for the
cache keys and invalidation rules.
