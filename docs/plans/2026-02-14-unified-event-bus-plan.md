# Unified Event Bus — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace three layers of event forwarding (Stream → Audio → Resource) with a single `EventBus` that all components publish to directly.

**Architecture:** New `kithara-events` crate holds `EventBus`, `Event` enum, and sub-enums (`HlsEvent`, `FileEvent`, `AudioEvent`). Each component receives `EventBus` via config and calls `bus.publish()` directly. No forwarding tasks, no wrapper enums.

**Tech Stack:** `tokio::sync::broadcast` (already in deps), feature-gated deps on `kithara-abr` and `kithara-decode`.

**Design doc:** `docs/plans/2026-02-14-unified-event-bus-design.md`

---

### Task 1: Create `kithara-events` crate

**Files:**
- Create: `crates/kithara-events/Cargo.toml`
- Create: `crates/kithara-events/src/lib.rs`
- Create: `crates/kithara-events/src/bus.rs`
- Create: `crates/kithara-events/src/event.rs`
- Create: `crates/kithara-events/src/hls.rs`
- Create: `crates/kithara-events/src/file.rs`
- Create: `crates/kithara-events/src/audio.rs`
- Modify: `Cargo.toml` (root) — add to workspace members + workspace.dependencies

**Step 1: Create directory and Cargo.toml**

```toml
# crates/kithara-events/Cargo.toml
[package]
name = "kithara-events"
version = "0.1.0"
edition.workspace = true
authors.workspace = true
license.workspace = true
description = "Unified event bus for kithara audio pipeline"

[lints]
workspace = true

[dependencies]
tokio = { workspace = true }

kithara-abr = { workspace = true, optional = true }
kithara-decode = { workspace = true, optional = true }

[features]
default = ["hls", "audio"]
hls = ["dep:kithara-abr"]
audio = ["dep:kithara-decode"]

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

**Step 2: Add to workspace**

In root `Cargo.toml`:
- Add `"crates/kithara-events"` to `[workspace] members` list (after `kithara-drm`)
- Add `kithara-events = { path = "crates/kithara-events", version = "0.1.0" }` to `[workspace.dependencies]`

**Step 3: Write `bus.rs`**

```rust
#![forbid(unsafe_code)]

use tokio::sync::broadcast;

use crate::Event;

/// Unified event bus for the kithara audio pipeline.
///
/// All components receive a cloned `EventBus` and publish events directly.
/// Subscribers receive all events from all components.
///
/// `publish()` is a sync call — works from both async tasks and blocking threads.
/// If there are no subscribers, events are silently dropped.
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Publish an event to all subscribers.
    ///
    /// Accepts any type that converts `Into<Event>`, so you can pass
    /// sub-enum values directly: `bus.publish(HlsEvent::EndOfStream)`.
    ///
    /// This is a sync call (no `.await`). Safe to call from blocking threads.
    pub fn publish(&self, event: impl Into<Event>) {
        let _ = self.tx.send(event.into());
    }

    /// Subscribe to all future events.
    ///
    /// Each subscriber gets an independent receiver. Slow subscribers
    /// receive `RecvError::Lagged(n)` instead of blocking producers.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
```

**Step 4: Write `file.rs`**

Move from `crates/kithara-file/src/events.rs` verbatim:

```rust
#![forbid(unsafe_code)]

/// Events emitted by file streams.
#[derive(Debug, Clone, PartialEq)]
pub enum FileEvent {
    /// Bytes written to local storage by the downloader.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed successfully.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Bytes consumed by the playback reader.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// General error.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
```

**Step 5: Write `hls.rs`**

Move from `crates/kithara-hls/src/events.rs`:

```rust
#![forbid(unsafe_code)]

use std::time::Duration;

use kithara_abr::{AbrReason, VariantInfo};

/// Events emitted during HLS playback.
#[derive(Clone, Debug)]
pub enum HlsEvent {
    /// Master playlist loaded and variants discovered.
    VariantsDiscovered {
        variants: Vec<VariantInfo>,
        initial_variant: usize,
    },
    /// Variant (quality level) changed.
    VariantApplied {
        from_variant: usize,
        to_variant: usize,
        reason: AbrReason,
    },
    /// Segment download started.
    SegmentStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },
    /// Segment download completed.
    SegmentComplete {
        variant: usize,
        segment_index: usize,
        bytes_transferred: u64,
        duration: Duration,
    },
    /// Throughput measurement.
    ThroughputSample { bytes_per_second: f64 },
    /// Cumulative download progress.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed successfully.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Playback progress update.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// Error occurred.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}
```

**Step 6: Write `audio.rs`**

Move from `crates/kithara-audio/src/events.rs` (only `AudioEvent`, NOT `AudioPipelineEvent`):

```rust
#![forbid(unsafe_code)]

use std::time::Duration;

use kithara_decode::PcmSpec;

/// Events from the audio pipeline.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// Audio format detected.
    FormatDetected { spec: PcmSpec },
    /// Audio format changed (ABR switch).
    FormatChanged { old: PcmSpec, new: PcmSpec },
    /// Seek completed.
    SeekComplete { position: Duration },
    /// Decoding finished (EOF).
    EndOfStream,
}
```

**Step 7: Write `event.rs`**

```rust
#![forbid(unsafe_code)]

use crate::FileEvent;

#[cfg(feature = "audio")]
use crate::AudioEvent;
#[cfg(feature = "hls")]
use crate::HlsEvent;

/// Unified event for the full audio pipeline.
///
/// Hierarchical: each subsystem has its own variant with a sub-enum.
#[derive(Clone, Debug)]
pub enum Event {
    /// HLS stream event.
    #[cfg(feature = "hls")]
    Hls(HlsEvent),
    /// File stream event.
    File(FileEvent),
    /// Audio pipeline event.
    #[cfg(feature = "audio")]
    Audio(AudioEvent),
}

#[cfg(feature = "hls")]
impl From<HlsEvent> for Event {
    fn from(e: HlsEvent) -> Self {
        Self::Hls(e)
    }
}

impl From<FileEvent> for Event {
    fn from(e: FileEvent) -> Self {
        Self::File(e)
    }
}

#[cfg(feature = "audio")]
impl From<AudioEvent> for Event {
    fn from(e: AudioEvent) -> Self {
        Self::Audio(e)
    }
}
```

**Step 8: Write `lib.rs`**

```rust
#![forbid(unsafe_code)]

//! Unified event bus for the kithara audio pipeline.

mod bus;
mod event;
mod file;

#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "hls")]
mod hls;

pub use bus::EventBus;
pub use event::Event;
pub use file::FileEvent;

#[cfg(feature = "audio")]
pub use audio::AudioEvent;
#[cfg(feature = "hls")]
pub use hls::HlsEvent;
```

**Step 9: Write tests in `bus.rs`**

Append to `bus.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileEvent;

    #[test]
    fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.publish(FileEvent::EndOfStream);
    }

    #[tokio::test]
    async fn publish_and_subscribe() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(FileEvent::DownloadComplete { total_bytes: 42 });
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            Event::File(FileEvent::DownloadComplete { total_bytes: 42 })
        ));
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        bus.publish(FileEvent::EndOfStream);
        assert!(matches!(rx1.recv().await.unwrap(), Event::File(FileEvent::EndOfStream)));
        assert!(matches!(rx2.recv().await.unwrap(), Event::File(FileEvent::EndOfStream)));
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_error() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for i in 0..10 {
            bus.publish(FileEvent::DownloadProgress { offset: i, total: None });
        }
        let result = rx.recv().await;
        assert!(matches!(result, Err(broadcast::error::RecvError::Lagged(_))));
    }

    #[test]
    fn clone_shares_channel() {
        let bus1 = EventBus::new(16);
        let bus2 = bus1.clone();
        let mut rx = bus1.subscribe();
        bus2.publish(FileEvent::EndOfStream);
        assert!(rx.try_recv().is_ok());
    }
}
```

**Step 10: Verify**

Run: `cargo test -p kithara-events`
Expected: All tests pass.

Run: `cargo clippy -p kithara-events -- -D warnings`
Expected: No warnings.

**Step 11: Commit**

```bash
git add crates/kithara-events/ Cargo.toml
git commit -m "feat(events): add kithara-events crate with EventBus and unified Event enum"
```

---

### Task 2: Migrate kithara-hls to use EventBus

This task, Task 3, and Task 4 form an atomic unit — they must all compile together.

**Files:**
- Modify: `crates/kithara-hls/Cargo.toml` — add kithara-events dep
- Delete: `crates/kithara-hls/src/events.rs`
- Modify: `crates/kithara-hls/src/lib.rs` — re-export from kithara-events instead
- Modify: `crates/kithara-hls/src/config.rs` — replace `events_tx` with `bus: Option<EventBus>`
- Modify: `crates/kithara-hls/src/inner.rs` — use EventBus, remove `ensure_events` impl
- Modify: `crates/kithara-hls/src/downloader.rs` — replace `events_tx` with `EventBus`, remove `emit_event()`
- Modify: `crates/kithara-hls/src/source.rs` — update `build_pair()` to pass EventBus

**Step 1: Add dep to Cargo.toml**

Add `kithara-events = { workspace = true }` to `[dependencies]` in `crates/kithara-hls/Cargo.toml`.

**Step 2: Delete `crates/kithara-hls/src/events.rs`**

**Step 3: Update `lib.rs`**

- Remove `pub mod events;`
- Change `pub use events::HlsEvent;` to `pub use kithara_events::HlsEvent;`
- Add `pub use kithara_events::EventBus;`

**Step 4: Update `config.rs`**

- Remove `use tokio::sync::broadcast;`
- Add `use kithara_events::EventBus;`
- Replace field `events_tx: Option<broadcast::Sender<crate::HlsEvent>>` → `pub bus: Option<EventBus>` (line 91)
- Replace field `events_channel_capacity: usize` → `pub event_channel_capacity: usize` (line 89)
- Update `Default` impl: `events_tx: None` → `bus: None`, `events_channel_capacity: 32` → `event_channel_capacity: 32`
- Update `new()`: same changes
- Replace `with_events()` signature: `broadcast::Sender<crate::HlsEvent>` → `EventBus`
- Replace `with_events()` body: `self.events_tx = Some(events_tx)` → `self.bus = Some(bus)`
- Replace `with_events_channel_capacity()` → `with_event_channel_capacity()`

**Step 5: Update `inner.rs`**

- Remove `use tokio::sync::broadcast;`
- Remove `use crate::events::HlsEvent;` → `use kithara_events::{EventBus, HlsEvent};`
- Remove `type Event = HlsEvent;` from `impl StreamType for Hls` (line 33)
- Remove entire `ensure_events()` method (lines 38-47)
- Replace events_tx creation (lines 120-123):
  ```rust
  // Old:
  let events_tx = match config.events_tx {
      Some(ref tx) => tx.clone(),
      None => broadcast::channel(config.events_channel_capacity.max(1)).0,
  };
  // New:
  let bus = config.bus.clone()
      .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));
  ```
- Replace `events_tx.send(HlsEvent::VariantsDiscovered { .. })` (line 127) with `bus.publish(HlsEvent::VariantsDiscovered { .. })`
- Pass `bus` instead of `Some(events_tx)` to `build_pair()`

**Step 6: Update `downloader.rs`**

- Remove `use tokio::sync::broadcast;`
- Remove `use crate::events::HlsEvent;`
- Add `use kithara_events::{EventBus, HlsEvent};`
- Replace field `events_tx: Option<broadcast::Sender<HlsEvent>>` → `bus: EventBus` (line 113)
- Remove `emit_event()` method entirely (lines 123-127)
- Replace all `self.emit_event(HlsEvent::X { .. })` → `self.bus.publish(HlsEvent::X { .. })` throughout the file (lines 321, 398, 570, 636, 674, 696, 705, 721, 754)

**Step 7: Update `source.rs`**

Find `build_pair()` function. Update its signature and body to accept `EventBus` instead of `Option<broadcast::Sender<HlsEvent>>`, and pass `bus` to `HlsDownloader` struct creation.

**Step 8: Verify (this won't compile alone — needs Task 3 + 4)**

This step is deferred to after Task 4.

---

### Task 3: Migrate kithara-file to use EventBus

**Files:**
- Modify: `crates/kithara-file/Cargo.toml` — add kithara-events dep
- Delete: `crates/kithara-file/src/events.rs`
- Modify: `crates/kithara-file/src/lib.rs` — re-export from kithara-events
- Modify: `crates/kithara-file/src/config.rs` — replace events_tx with bus
- Modify: `crates/kithara-file/src/inner.rs` — use EventBus, remove ensure_events
- Modify: `crates/kithara-file/src/session.rs` — replace events_tx with EventBus

**Step 1: Add dep**

Add `kithara-events = { workspace = true }` to `[dependencies]` in `crates/kithara-file/Cargo.toml`.

**Step 2: Delete `crates/kithara-file/src/events.rs`**

**Step 3: Update `lib.rs`**

- Remove `pub mod events;`
- Change `pub use events::FileEvent;` to `pub use kithara_events::FileEvent;`
- Add `pub use kithara_events::EventBus;`

**Step 4: Update `config.rs`**

Same pattern as HLS:
- Replace `events_tx: Option<broadcast::Sender<FileEvent>>` → `pub bus: Option<EventBus>`
- Replace `events_channel_capacity` → `event_channel_capacity`
- Update `with_events()` to take `EventBus`
- Update Default + new()

**Step 5: Update `inner.rs`**

- Remove `type Event = FileEvent;` from `impl StreamType for File`
- Remove `ensure_events()` implementation
- Replace `events_tx` creation with `let bus = config.bus.clone().unwrap_or_else(|| EventBus::new(config.event_channel_capacity));`
- Replace all `events.send(FileEvent::X)` / `events_tx.send(FileEvent::X)` with `bus.publish(FileEvent::X)`

**Step 6: Update `session.rs`**

- In `FileStreamState`: replace `events_tx: broadcast::Sender<FileEvent>` → `bus: EventBus`
- In `FileSource`: replace `events_tx: broadcast::Sender<FileEvent>` → `bus: EventBus`
- Replace all `self.events_tx.send(FileEvent::X { .. })` → `self.bus.publish(FileEvent::X { .. })`

---

### Task 4: Clean up kithara-stream (remove Event from StreamType)

**Files:**
- Modify: `crates/kithara-stream/src/stream.rs`
- Modify: `crates/kithara-stream/src/lib.rs` (if StreamType is re-exported — verify)

**Step 1: Remove from StreamType trait** (`stream.rs:28-72`)

- Remove `type Event: Clone + Send + 'static;` (line 39)
- Remove `fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<Self::Event>;` (line 62)
- Remove `use tokio::sync::broadcast;` (line 20) if no other usage remains

**Step 2: Remove from Stream struct** (`stream.rs:79-103`)

- Remove `events_rx: Option<broadcast::Receiver<T::Event>>` field (line 82)
- Remove from `Stream::new()`: `let events_rx = T::ensure_events(&mut config);` (line 88) and `events_rx: Some(events_rx)` (line 93)
- Remove `take_events_rx()` method entirely (lines 97-103)

**Step 3: Verify all crates compile**

Run: `cargo check --workspace`
Expected: Compiles successfully. This confirms Tasks 2-4 are consistent.

Run: `cargo test -p kithara-events`
Expected: All tests pass.

**Step 4: Commit Tasks 2-4 together**

```bash
git add -A
git commit -m "refactor(events): migrate hls/file to EventBus, remove Event from StreamType"
```

---

### Task 5: Migrate kithara-audio to use EventBus

**Files:**
- Modify: `crates/kithara-audio/Cargo.toml` — add kithara-events dep
- Delete: `crates/kithara-audio/src/events.rs`
- Modify: `crates/kithara-audio/src/lib.rs` — remove AudioPipelineEvent, re-export from kithara-events
- Modify: `crates/kithara-audio/src/pipeline/config.rs` — replace events_tx with bus
- Modify: `crates/kithara-audio/src/pipeline/audio.rs` — remove forwarding task, simplify emit
- Modify: `crates/kithara-audio/src/pipeline/stream_source.rs` — update emit signature

**Step 1: Add dep**

Add `kithara-events = { workspace = true }` to `[dependencies]` in `crates/kithara-audio/Cargo.toml`.

**Step 2: Delete `crates/kithara-audio/src/events.rs`**

**Step 3: Update `lib.rs`**

- Remove `mod events;`
- Remove `pub use events::{AudioEvent, AudioPipelineEvent};`
- Add `pub use kithara_events::{AudioEvent, EventBus};`
- Keep `pub use kithara_events::Event;` if consumers need it

**Step 4: Update `config.rs`**

- Remove `use kithara_audio::AudioPipelineEvent` (or equivalent import)
- Add `use kithara_events::EventBus;`
- Replace field `events_tx: Option<broadcast::Sender<AudioPipelineEvent<T::Event>>>` → `pub(super) bus: Option<EventBus>`
- Remove `event_channel_capacity` field (EventBus capacity is set at creation)
- Update `with_events()` to take `EventBus`:
  ```rust
  pub fn with_events(mut self, bus: EventBus) -> Self {
      self.bus = Some(bus);
      self
  }
  ```

**Step 5: Update `audio.rs` — the big change**

In `Audio` struct definition, replace:
- `audio_events_tx: broadcast::Sender<AudioEvent>` → `bus: EventBus`
- Remove `unified_events: Option<Box<dyn std::any::Any + Send + Sync>>`

In `Audio::<Stream<T>>::new()` (lines 369-652):

- Remove `event_channel_capacity` from destructuring (line 376)
- Replace unified channel creation (lines 393-396):
  ```rust
  // Old:
  let event_capacity = event_channel_capacity.max(1);
  let unified_tx = events_tx.unwrap_or_else(|| broadcast::channel(event_capacity).0);
  // New:
  let bus = config.bus.unwrap_or_else(|| EventBus::new(64));
  ```
- Pass bus to stream config before Stream::new():
  ```rust
  stream_config.bus = Some(bus.clone());
  ```
- **Delete entire forwarding task** (lines 404-426) — the 23-line tokio::spawn block
- Remove `(audio_events_tx, _) = broadcast::channel(event_capacity)` (line 503)
- Simplify emit closure (lines 521-527):
  ```rust
  // Old:
  let emit_raw_tx = audio_events_tx.clone();
  let emit_unified_tx = unified_tx.clone();
  let emit = Box::new(move |event: AudioEvent| {
      let _ = emit_raw_tx.send(event.clone());
      let _ = emit_unified_tx.send(AudioPipelineEvent::Audio(event));
  });
  // New:
  let emit_bus = bus.clone();
  let emit = Box::new(move |event: AudioEvent| {
      emit_bus.publish(event);
  });
  ```
- Update struct construction (lines 630-651):
  - Replace `audio_events_tx` → `bus`
  - Remove `unified_events: Some(Box::new(unified_tx))`

- **Remove `events()` method** (lines 660-666) — type-erased downcast is no longer needed

- Update `decode_events()` in PcmReader impl: return `bus.subscribe()` instead of `audio_events_tx.subscribe()`

**Step 6: Update `stream_source.rs`**

The emit closure type `Box<dyn Fn(AudioEvent) + Send>` stays the same. No changes needed here — the closure is already opaque. The only change is in audio.rs where the closure is created.

**Step 7: Verify**

Run: `cargo check --workspace`
Run: `cargo test -p kithara-audio`
Expected: Compiles and tests pass.

**Step 8: Commit**

```bash
git add -A
git commit -m "refactor(audio): replace forwarding task and AudioPipelineEvent with EventBus"
```

---

### Task 6: Clean up kithara facade

**Files:**
- Modify: `crates/kithara/Cargo.toml` — add kithara-events dep
- Delete or gut: `crates/kithara/src/events.rs` — remove ResourceEvent and all From impls
- Modify: `crates/kithara/src/resource.rs` — remove spawn_typed_forward, use EventBus directly
- Modify: `crates/kithara/src/lib.rs` — update re-exports
- Modify: `crates/kithara/src/config.rs` — update ResourceConfig if it references events

**Step 1: Add dep**

Add `kithara-events = { workspace = true }` to `[dependencies]` in `crates/kithara/Cargo.toml`.

**Step 2: Replace `events.rs`**

Delete entire contents of `crates/kithara/src/events.rs` (ResourceEvent + ~110 lines of From impls). Replace with a simple re-export:

```rust
#![forbid(unsafe_code)]

//! Re-export unified events from kithara-events.

pub use kithara_events::{Event, EventBus};

#[cfg(feature = "audio")]
pub use kithara_events::AudioEvent;
#[cfg(feature = "file")]
pub use kithara_events::FileEvent;
#[cfg(feature = "hls")]
pub use kithara_events::HlsEvent;
```

**Step 3: Update `resource.rs`**

- Replace `events_tx: broadcast::Sender<ResourceEvent>` → `bus: EventBus` in `Resource` struct (line 37)
- Remove `spawn_typed_forward()` entirely (lines 199-217)
- Remove `spawn_audio_forward()` entirely (lines 182-197)
- Update `from_file()` (lines 84-96):
  ```rust
  pub(crate) async fn from_file(config: AudioConfig<kithara_file::File>) -> DecodeResult<Self> {
      use kithara_stream::Stream;
      let bus = config.bus.clone().unwrap_or_else(|| EventBus::new(64));
      let audio = Audio::<Stream<kithara_file::File>>::new(config).await?;
      Ok(Self { inner: Box::new(audio), bus })
  }
  ```
- Update `from_hls()` (lines 101-114) similarly
- Update `from_reader()` (lines 66-77):
  ```rust
  pub(crate) fn from_reader(reader: impl PcmReader + 'static) -> Self {
      let bus = EventBus::new(64);
      Self { inner: Box::new(reader), bus }
  }
  ```
- Update `subscribe()` (line 117):
  ```rust
  pub fn subscribe(&self) -> broadcast::Receiver<Event> {
      self.bus.subscribe()
  }
  ```

**Step 4: Update `lib.rs` re-exports**

- Remove `pub use events::ResourceEvent;`
- Add `pub use events::{Event, EventBus};` (or re-export from kithara_events directly)
- Add conditional re-exports for `AudioEvent`, `FileEvent`, `HlsEvent`

**Step 5: Update tests in `resource.rs`**

The test `test_resource_subscribe_receives_events` (line 448) needs updating:
- Mock should now publish via EventBus
- Pattern match on `Event::Audio(AudioEvent::FormatDetected { .. })` instead of `ResourceEvent::FormatDetected`

**Step 6: Verify**

Run: `cargo check --workspace`
Run: `cargo test -p kithara`
Expected: Compiles and tests pass.

**Step 7: Commit**

```bash
git add -A
git commit -m "refactor(kithara): remove ResourceEvent, replace with unified EventBus"
```

---

### Task 7: Update examples

**Files:**
- Modify: `crates/kithara-audio/examples/hls_audio.rs`
- Modify: `crates/kithara-audio/examples/file_audio.rs`
- Modify: any other examples (check for `hls_drm_audio.rs`)

**Step 1: Update `hls_audio.rs`**

Replace:
```rust
let (events_tx, mut events_rx) = broadcast::channel(128);
// ...
.with_events(events_tx);
```

With:
```rust
let bus = EventBus::new(128);
let mut events_rx = bus.subscribe();
// ...
.with_events(bus);
```

Update pattern matching from `AudioPipelineEvent::Stream(HlsEvent::X)` to `Event::Hls(HlsEvent::X)`.
Update imports.

**Step 2: Update `file_audio.rs`**

Same pattern as hls_audio.

**Step 3: Check for other examples**

Search for any other example files using the old event types and update them.

**Step 4: Verify**

Run: `cargo check --workspace --examples`
Expected: All examples compile.

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor(examples): update hls_audio and file_audio to use EventBus"
```

---

### Task 8: Update integration tests

**Files:**
- Modify: tests in `tests/` crate that use event types
- Modify: any integration test files referencing `AudioPipelineEvent`, `ResourceEvent`, `ensure_events`, `take_events_rx`

**Step 1: Search for usages**

Search across the workspace for:
- `AudioPipelineEvent`
- `ResourceEvent`
- `ensure_events`
- `take_events_rx`
- `events_tx`
- `with_events_channel_capacity`

Fix each occurrence.

**Step 2: Verify full workspace**

Run: `cargo test --workspace`
Expected: All tests pass.

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

Run: `cargo fmt --all --check`
Expected: No formatting issues.

Run: `bash scripts/ci/lint-style.sh`
Expected: No blocking violations.

**Step 3: Commit**

```bash
git add -A
git commit -m "refactor(tests): update integration tests for unified event bus"
```

---

### Task 9: Clean up dead code and verify

**Step 1: Remove any remaining dead imports**

Run `cargo clippy --workspace -- -D warnings` and fix any unused imports, dead code warnings.

**Step 2: Run full test suite**

```bash
cargo test --workspace
```

**Step 3: Run style checks**

```bash
cargo fmt --all
bash scripts/ci/lint-style.sh
```

**Step 4: Final commit**

```bash
git add -A
git commit -m "chore: clean up dead code after event bus migration"
```
