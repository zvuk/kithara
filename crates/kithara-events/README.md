<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-events.svg)](https://crates.io/crates/kithara-events)
[![docs.rs](https://docs.rs/kithara-events/badge.svg)](https://docs.rs/kithara-events)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-events

Unified event bus for the kithara audio pipeline. Provides a clone-able `EventBus` backed by `tokio::sync::broadcast`, hierarchical `BusScope`, and a feature-gated `Event` enum that aggregates events from every subsystem.

## Usage

```rust
use kithara_events::{Event, EventBus, FileEvent};

let bus = EventBus::new(64);
let mut rx = bus.subscribe();

bus.publish(FileEvent::EndOfStream);

while let Ok(event) = rx.recv().await {
    match event {
        Event::File(e) => handle_file(e),
        Event::Hls(e) => handle_hls(e),
        Event::Audio(e) => handle_audio(e),
        _ => {}
    }
}
```

## Key Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>EventBus</code></td><td>Clone-able broadcast publisher; <code>publish()</code> works from both async and blocking contexts</td></tr>
<tr><td><code>BusScope</code></td><td>Hierarchical scope used to attribute events to player/track/peer subtrees</td></tr>
<tr><td><code>EventReceiver</code></td><td>Subscriber handle returned by <code>EventBus::subscribe()</code></td></tr>
<tr><td><code>Event</code></td><td>Top-level enum — variants are feature-gated</td></tr>
<tr><td><code>SeekEpoch</code></td><td>Monotonic seek-generation tag carried across subsystems</td></tr>
</table>

## Features

The default feature set exposes the full event surface. See CONTEXT.md for the
per-feature table and the optional HTTP/TLS forwarding features.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
