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

All variants of `Event` and all subsystem sub-enums are feature-gated. The default set turns everything on so consumers get the full event surface; disable defaults and pick à-la-carte for smaller builds.

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>abr</code></td><td>yes</td><td><code>AbrEvent</code>, <code>AbrMode</code>, <code>VariantInfo</code>, …</td></tr>
<tr><td><code>app</code></td><td>yes</td><td><code>AppEvent</code></td></tr>
<tr><td><code>audio</code></td><td>yes</td><td><code>AudioEvent</code>, <code>AudioFormat</code>, <code>SeekLifecycleStage</code></td></tr>
<tr><td><code>downloader</code></td><td>yes</td><td><code>DownloaderEvent</code>, <code>CancelReason</code>, <code>RequestId</code> (pulls <code>kithara-net</code>)</td></tr>
<tr><td><code>file</code></td><td>yes</td><td><code>FileEvent</code>, <code>FileError</code></td></tr>
<tr><td><code>hls</code></td><td>yes</td><td><code>HlsEvent</code>, <code>HlsError</code> (implies <code>abr</code>)</td></tr>
<tr><td><code>player</code></td><td>yes</td><td><code>PlayerEvent</code>, <code>EngineEvent</code>, <code>ItemEvent</code>, <code>SessionEvent</code>, <code>DjEvent</code>, <code>MediaTime</code>, …</td></tr>
<tr><td><code>queue</code></td><td>yes</td><td><code>QueueEvent</code>, <code>TrackId</code>, <code>TrackStatus</code></td></tr>
</table>

## Trait Bridges

- `{Downloader,Hls,File,Audio,Player,Engine,Item,Session,Dj,App,Queue,Abr}Event` → `Event` (`From`) — lift subsystem events into the top-level enum
- `TrackId` ↔ `u64` (`From` both ways) — track-id newtype conversions
- `AbrMode` ↔ `usize` (`From` both ways) — variant-index encoding
- `Duration` → `MediaTime` (`From`) / `&MediaTime` → `Duration` (`TryFrom`) — playback-time bridge, rejects invalid/indefinite
- `FileError` / `HlsError` / `AudioFormat` / `TrackId` (`Display`) — human-readable rendering

## Integration

Used by `kithara-audio`, `kithara-file`, `kithara-hls`, `kithara-abr`, `kithara-play`, `kithara-queue`, `kithara-app`, and the `kithara` facade. Each subsystem publishes to a shared `EventBus`; consumers subscribe for a unified `Event` stream via `tokio::sync::broadcast`.
