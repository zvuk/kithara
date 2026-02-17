<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-events.svg)](https://crates.io/crates/kithara-events)
[![Downloads](https://img.shields.io/crates/d/kithara-events.svg)](https://crates.io/crates/kithara-events)
[![docs.rs](https://docs.rs/kithara-events/badge.svg)](https://docs.rs/kithara-events)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-events

Unified event bus for the kithara audio pipeline. Provides a clone-able `EventBus` backed by `tokio::sync::broadcast` and a hierarchical `Event` enum covering HLS, file, and audio pipeline events.

## Usage

```rust
use kithara_events::{EventBus, Event};

let bus = EventBus::new(64);

// Subscribe
let mut rx = bus.subscribe();

// Publish from any component
bus.publish(FileEvent::EndOfStream);

// Receive unified events
while let Ok(event) = rx.recv().await {
    match event {
        Event::File(e) => handle_file(e),
        Event::Hls(e) => handle_hls(e),
        Event::Audio(e) => handle_audio(e),
    }
}
```

## Key Types

| Type | Role |
|------|------|
| `EventBus` | Clone-able broadcast publisher; `publish()` works from both async and blocking contexts |
| `Event` | Top-level enum: `File(FileEvent)`, `Hls(HlsEvent)`, `Audio(AudioEvent)` |
| `FileEvent` | Download progress, completion, errors, end-of-stream |
| `HlsEvent` | Variant discovery, ABR switches, segment lifecycle, throughput samples |
| `AudioEvent` | Format detection, format changes, seek completion, end-of-stream |

## Features

| Feature | Default | Enables |
|---------|---------|---------|
| `hls` | yes | `HlsEvent` variant, depends on `kithara-abr` |
| `audio` | yes | `AudioEvent` variant, depends on `kithara-decode` |

## Integration

Used by `kithara-audio`, `kithara-file`, `kithara-hls`, and the `kithara` facade. All pipeline components publish to a shared `EventBus`; consumers subscribe for a unified `Event` stream via `tokio::sync::broadcast`.
