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

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>EventBus</code></td><td>Clone-able broadcast publisher; <code>publish()</code> works from both async and blocking contexts</td></tr>
<tr><td><code>Event</code></td><td>Top-level enum: <code>File(FileEvent)</code>, <code>Hls(HlsEvent)</code>, <code>Audio(AudioEvent)</code></td></tr>
<tr><td><code>FileEvent</code></td><td>Download progress, completion, errors, end-of-stream</td></tr>
<tr><td><code>HlsEvent</code></td><td>Variant discovery, ABR switches, segment lifecycle, throughput samples</td></tr>
<tr><td><code>AudioEvent</code></td><td>Format detection, format changes, seek completion, end-of-stream</td></tr>
</table>

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>hls</code></td><td>yes</td><td><code>HlsEvent</code> variant, depends on <code>kithara-abr</code></td></tr>
<tr><td><code>audio</code></td><td>yes</td><td><code>AudioEvent</code> variant, depends on <code>kithara-decode</code></td></tr>
</table>

## Integration

Used by `kithara-audio`, `kithara-file`, `kithara-hls`, and the `kithara` facade. All pipeline components publish to a shared `EventBus`; consumers subscribe for a unified `Event` stream via `tokio::sync::broadcast`.
