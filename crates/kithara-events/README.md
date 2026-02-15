# kithara-events

Unified event bus for the kithara audio pipeline.

## Public Contract

- **`EventBus`** — Clone-able event bus backed by `tokio::sync::broadcast`. All components publish to a shared bus; subscribers receive all events.
- **`Event`** — Hierarchical enum: `Hls(HlsEvent)`, `File(FileEvent)`, `Audio(AudioEvent)`.
- **`HlsEvent`** — HLS stream events (variant switches, segment lifecycle, throughput). Requires `hls` feature.
- **`FileEvent`** — File stream events (download progress, errors).
- **`AudioEvent`** — Audio pipeline events (format detection, seek, EOF). Requires `audio` feature.

## Features

| Feature | Default | Enables |
|---------|---------|---------|
| `hls` | yes | `HlsEvent`, depends on `kithara-abr` |
| `audio` | yes | `AudioEvent`, depends on `kithara-decode` |
