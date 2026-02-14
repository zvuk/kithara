# Unified Event Bus Design

## Problem

Current event architecture uses three layers of broadcast channels with forwarding tasks:

```
Stream (FileEvent/HlsEvent)  → tokio::spawn → Audio (AudioPipelineEvent<E>) → tokio::spawn → Resource (ResourceEvent)
```

Each layer creates its own `broadcast::channel`, spawns a forwarding task, and wraps events in its own type. Adding a new event source requires new forwarding code, new `From` impls, and new wrapper variants.

## Solution

Single `EventBus` backed by `tokio::sync::broadcast`, passed to all components. Each component publishes directly — no forwarding tasks, no wrapping.

## New crate: `kithara-events`

### Dependencies

```toml
[dependencies]
tokio = { workspace = true, features = ["sync"] }

[dependencies.kithara-abr]
path = "../kithara-abr"
optional = true

[dependencies.kithara-decode]
path = "../kithara-decode"
optional = true

[features]
default = ["hls", "audio"]
hls = ["dep:kithara-abr"]
audio = ["dep:kithara-decode"]
```

### Module structure

```
kithara-events/src/
  lib.rs       — pub mod + re-exports only
  bus.rs       — EventBus
  event.rs     — Event enum + From impls
  hls.rs       — HlsEvent
  file.rs      — FileEvent
  audio.rs     — AudioEvent
```

### Dependency graph (no cycles)

```
kithara-abr (0 deps) ──┐
                        ├──▶ kithara-events (EventBus + all enums)
kithara-decode ─────────┘          │
    │                              │
    └── kithara-stream             │  (does NOT depend on kithara-events)
            │                      │
            ├── kithara-hls ───────┤  (depends on kithara-events)
            ├── kithara-file ──────┤
            └── kithara-audio ─────┘
```

## EventBus

```rust
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self;
    pub fn publish(&self, event: impl Into<Event>);
    pub fn subscribe(&self) -> broadcast::Receiver<Event>;
}
```

- `publish()` is a sync call (broadcast::Sender::send is sync — mutex lock, write, return)
- Works from both async tasks and blocking worker threads
- No subscribers = noop (send returns Err which is ignored)
- Clone-able, passed as value to all components

## Event hierarchy

```rust
#[derive(Clone, Debug)]
pub enum Event {
    #[cfg(feature = "hls")]
    Hls(HlsEvent),
    File(FileEvent),
    #[cfg(feature = "audio")]
    Audio(AudioEvent),
}
```

Sub-enums are identical to current types, moved from their original crates:

- `HlsEvent` — from `kithara-hls/src/events.rs` (uses `VariantInfo`, `AbrReason` from kithara-abr)
- `FileEvent` — from `kithara-file/src/events.rs` (self-contained)
- `AudioEvent` — from `kithara-audio/src/events.rs` (uses `PcmSpec` from kithara-decode)

`From` impls for ergonomic publishing: `bus.publish(HlsEvent::SegmentComplete { .. })` auto-wraps into `Event::Hls(...)`.

## What gets removed

### kithara-stream (StreamType trait)
- `type Event` associated type
- `ensure_events()` method
- `events_rx: Option<broadcast::Receiver<T::Event>>` field in `Stream<T>`
- `take_events_rx()` method

### kithara-hls
- `events_tx: Option<broadcast::Sender<HlsEvent>>` from HlsConfig
- `events_channel_capacity` from HlsConfig
- `emit_event()` helper in HlsDownloader
- `crates/kithara-hls/src/events.rs` (type moved to kithara-events)

### kithara-file
- `events_tx: Option<broadcast::Sender<FileEvent>>` from FileConfig
- `events_channel_capacity` from FileConfig
- `crates/kithara-file/src/events.rs` (type moved to kithara-events)

### kithara-audio
- `AudioPipelineEvent<E>` wrapper enum (entire type removed)
- Forwarding task in `Audio::new()` (lines 404-426)
- `unified_events: Box<dyn Any>` type erasure
- `events_tx` / `audio_events_tx` dual channel setup
- `crates/kithara-audio/src/events.rs` (types moved to kithara-events)

### kithara (facade)
- `ResourceEvent` enum (entire type removed)
- `spawn_typed_forward()` in resource.rs
- All `From` impls (~110 lines) in `crates/kithara/src/events.rs`

## Config changes

Each crate's config gains `bus: Option<EventBus>` (optional in config, mandatory at runtime):

```rust
impl<T: StreamType> AudioConfig<T> {
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }
}

// In Audio::new():
let bus = config.bus.unwrap_or_else(|| EventBus::new(config.event_channel_capacity));
// Pass same bus to stream config
stream_config.bus = bus.clone();
```

## Public API change

Before:
```rust
let (events_tx, mut events_rx) = broadcast::channel(128);
let config = AudioConfig::<Hls>::new(hls_config).with_events(events_tx);
let audio = Audio::<Stream<Hls>>::new(config).await?;
// AudioPipelineEvent::Stream(HlsEvent::VariantApplied { .. })
```

After:
```rust
let bus = EventBus::new(128);
let config = AudioConfig::<Hls>::new(hls_config).with_events(bus.clone());
let audio = Audio::<Stream<Hls>>::new(config).await?;
let mut rx = bus.subscribe();
// Event::Hls(HlsEvent::VariantApplied { .. })
```

## Migration order (bottom-up)

Steps 2-4 must be done together (StreamType::Event removal requires hls/file to use EventBus simultaneously).

1. Create `kithara-events` — EventBus, Event, sub-enums, From impls, tests
2. `kithara-hls` — depend on kithara-events, replace events_tx with EventBus, delete events.rs
3. `kithara-file` — same
4. `kithara-stream` — remove type Event, ensure_events(), take_events_rx() from StreamType/Stream
5. `kithara-audio` — remove forwarding task, AudioPipelineEvent, replace with EventBus, delete events.rs
6. `kithara` (facade) — remove ResourceEvent, spawn_typed_forward(), all From impls, events.rs
7. Examples (hls_audio, file_audio) — update to new API

## Testing

- EventBus unit tests: publish/subscribe, multiple subscribers, lagged, no-subscriber noop
- Existing integration tests adapted: replace broadcast::channel with EventBus, update pattern matching
- Stress tests verify events arrive correctly during rapid ABR switches and seeks
