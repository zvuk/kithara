# kithara-play Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Port zvqengine audio layer into kithara-play, providing a DJ-ready player engine with Firewheel audio graph, EQ, crossfade, and iOS-compatible API.

**Architecture:** Trait-based API (kept for mocking) with concrete implementations. Engine wraps Firewheel audio graph with per-slot chain (PlayerNode → VolPanNode → SumNode → EQ(EffectBridgeNode) → GraphOut). EQ is a master effect on the Firewheel graph (like zvqengine), implemented as `AudioEffect` from kithara-audio and wrapped via `EffectBridgeNode` adapter. Per-track custom effects can be injected via `AudioConfig::with_effect()`. Player manages items array, crossfade, and state via atomics. Item is the former Resource (moved from kithara facade).

**Tech Stack:** Firewheel (audio graph), rubato (resampling, already in kithara-audio), biquad (EQ filters), tokio (async), kanal (realtime-safe channels), portable-atomic (AtomicF64)

**Key design principle — effects architecture:**
- `AudioEffect` trait (kithara-audio) is the SINGLE interface for all effects (EQ, filters, etc.)
- **EQ is a master effect** — runs on the Firewheel graph via `EffectBridgeNode`, like zvqengine's EqNode
- Per-track custom effects: injected into `Audio<S>` worker thread effects chain (after resampler)
- Master effects (EQ + others): wrapped via `EffectBridgeNode` adapter into Firewheel AudioNode
- Firewheel graph: PlayerNode → VolPanNode → SumNode → EQ(EffectBridgeNode) → GraphOut

**Design doc:** `docs/plans/2026-02-17-kithara-play-implementation-design.md`

**Reference codebase:** `/Users/litvinenko-pv/code/zvqengine/crates/core/` — port audio code from here

---

## Phase 1: Foundation

### Task 1: Update workspace dependencies

**Files:**
- Modify: `/Users/litvinenko-pv/code/kithara/Cargo.toml` (workspace dependencies)
- Modify: `crates/kithara-play/Cargo.toml`

**Step 1: Add firewheel and biquad to workspace dependencies**

In root `Cargo.toml` `[workspace.dependencies]` section, add:

```toml
firewheel = { git = "https://github.com/BillyDM/Firewheel.git", features = ["default", "pool", "all_nodes", "musical_transport", "scheduled_events"] }
biquad = "0.4"
```

Check zvqengine's `Cargo.toml` for exact firewheel git ref:
- File: `/Users/litvinenko-pv/code/zvqengine/Cargo.toml` line 31

**Step 2: Update kithara-play Cargo.toml**

Replace `crates/kithara-play/Cargo.toml` with:

```toml
[package]
name = "kithara-play"
version = "0.1.0"
edition.workspace = true
authors.workspace = true
license.workspace = true
description = "Player engine with Firewheel audio graph, EQ, crossfade, and DJ-ready architecture"

[lints]
workspace = true

[dependencies]
# Kithara sub-crates
kithara-audio = { workspace = true }
kithara-decode = { workspace = true }
kithara-events = { workspace = true }
kithara-stream = { workspace = true }
kithara-file = { workspace = true, optional = true }
kithara-hls = { workspace = true, optional = true }
kithara-abr = { workspace = true, optional = true }
kithara-assets = { workspace = true, optional = true }
kithara-net = { workspace = true, optional = true }
kithara-bufpool = { workspace = true }

# Audio engine
firewheel = { workspace = true }
biquad = { workspace = true }

# Async & channels
tokio = { workspace = true }
tokio-util = { workspace = true }
kanal = { workspace = true }

# Utilities
thiserror = { workspace = true }
tracing = { workspace = true }
url = { workspace = true }
portable-atomic = { workspace = true }
smallvec = { workspace = true }

# Testing
unimock = { workspace = true, optional = true }

[features]
default = ["file", "hls"]
file = ["dep:kithara-file", "dep:kithara-assets", "dep:kithara-net"]
hls = ["dep:kithara-hls", "dep:kithara-abr", "dep:kithara-assets", "dep:kithara-net"]
test-utils = ["unimock"]

[dev-dependencies]
unimock = { workspace = true }
tokio = { workspace = true, features = ["full", "test-util"] }
```

**Step 3: Verify it compiles**

Run: `cargo check -p kithara-play`
Expected: compiles with no errors (existing trait code still works)

**Step 4: Commit**

```bash
git add Cargo.toml crates/kithara-play/Cargo.toml
git commit -m "build: add firewheel and biquad deps to kithara-play"
```

---

### Task 2: Restructure kithara-play modules

**Files:**
- Modify: `crates/kithara-play/src/lib.rs`
- Create: `crates/kithara-play/src/traits/mod.rs`
- Move: existing trait files → `traits/` directory
- Create: `crates/kithara-play/src/core/mod.rs` (empty, for implementations)

**Step 1: Create traits/ directory and move trait files**

Move all current trait files into `src/traits/`:
- `asset.rs` → `traits/asset.rs`
- `engine.rs` → `traits/engine.rs`
- `item.rs` → `traits/item.rs`
- `mixer.rs` → `traits/mixer.rs`
- `player.rs` → `traits/player.rs`
- `queue.rs` → `traits/queue.rs`
- `session.rs` → `traits/session.rs`
- `dj/` → `traits/dj/` (entire directory)

Create `traits/mod.rs` that re-exports everything the old lib.rs exported.

**Step 2: Create core/ directory for implementations**

Create `src/core/mod.rs` — initially empty, will hold concrete implementations.

**Step 3: Update lib.rs**

```rust
#![allow(clippy::missing_errors_doc, clippy::ignored_unit_patterns)]

pub mod traits;
pub mod core;

mod error;
mod events;
mod metadata;
mod time;
mod types;

// Re-export traits (backwards compatible)
pub use traits::asset::Asset;
pub use traits::engine::Engine;
pub use traits::item::PlayerItem;
pub use traits::mixer::Mixer;
pub use traits::player::Player;
pub use traits::queue::QueuePlayer;
pub use traits::session::{
    AudioSession, PortDescription, PortType, RouteDescription, SessionCategory, SessionMode,
    SessionOptions,
};

// DJ traits
pub use traits::dj::bpm::{BeatGrid, BpmAnalyzer, BpmInfo, BpmSync};
pub use traits::dj::crossfade::{CrossfadeConfig, CrossfadeController, CrossfadeCurve};
pub use traits::dj::effects::{DjEffect, DjEffectKind};
pub use traits::dj::eq::{EqConfig, Equalizer};

// Types
pub use error::PlayError;
pub use events::{
    DjEvent, EngineEvent, InterruptionKind, ItemEvent, PlayerEvent, RouteChangeReason,
    SessionEvent,
};
pub use metadata::{Artwork, Metadata};
pub use time::MediaTime;
pub use types::{
    ActionAtItemEnd, EqBand, ItemStatus, ObserverId, PlayerStatus, SlotId, TimeControlStatus,
    TimeRange, WaitingReason,
};

// Mock exports (test-utils feature)
#[cfg(any(test, feature = "test-utils"))]
pub use traits::asset::AssetMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::dj::bpm::{BpmAnalyzerMock, BpmSyncMock};
#[cfg(any(test, feature = "test-utils"))]
pub use traits::dj::crossfade::CrossfadeControllerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::dj::effects::DjEffectMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::dj::eq::EqualizerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::engine::EngineMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::mixer::MixerMock;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::session::AudioSessionMock;
```

**Step 4: Fix all `crate::` references inside moved trait files**

Each moved file references `crate::error::PlayError`, `crate::types::SlotId`, etc. These paths remain valid because `error.rs`, `types.rs` etc. stay at crate root. The only changes needed are for inter-trait references (e.g., `crate::item::PlayerItem` → `crate::traits::item::PlayerItem`).

Check player.rs line 13: `crate::item::PlayerItem` → `crate::traits::item::PlayerItem`
Check engine.rs line 3: `crate::dj::crossfade::CrossfadeConfig` → `crate::traits::dj::crossfade::CrossfadeConfig`

**Step 5: Verify it compiles**

Run: `cargo check -p kithara-play`
Expected: compiles, all existing tests pass

Run: `cargo test -p kithara-play`
Expected: all trait mock tests pass

**Step 6: Commit**

```bash
git add crates/kithara-play/
git commit -m "refactor: move kithara-play traits into traits/ module"
```

---

## Phase 2: Move Resource → Item

### Task 3: Move Resource and config from kithara to kithara-play

**Files:**
- Move: `crates/kithara/src/resource.rs` → `crates/kithara-play/src/core/item.rs`
- Move: `crates/kithara/src/config.rs` → `crates/kithara-play/src/core/config.rs`
- Move: `crates/kithara/src/source_type.rs` → `crates/kithara-play/src/core/source_type.rs`
- Modify: `crates/kithara/src/lib.rs` — replace with re-exports from kithara-play
- Modify: `crates/kithara/Cargo.toml` — add kithara-play dependency
- Modify: `crates/kithara-play/src/core/mod.rs`
- Modify: `crates/kithara-play/src/lib.rs`

**Step 1: Copy resource.rs, config.rs, source_type.rs to kithara-play**

Copy the three files into `crates/kithara-play/src/core/`.

Rename `Resource` → `Item` in the copied `item.rs`.
Keep `ResourceConfig` and `ResourceSrc` names for backwards compatibility (they describe config, not the playing item).

Update `core/mod.rs`:
```rust
mod config;
mod item;
mod source_type;

pub use config::{ResourceConfig, ResourceSrc};
pub use item::Item;
pub use source_type::SourceType;
```

Fix imports in the copied files — they referenced `crate::config`, `crate::source_type` which now live in the same `core` module.

**Step 2: Update lib.rs to export Item, ResourceConfig**

Add to `crates/kithara-play/src/lib.rs`:
```rust
// Core implementations
pub use core::config::{ResourceConfig, ResourceSrc};
pub use core::item::Item;
pub use core::source_type::SourceType;
```

**Step 3: Update kithara facade to re-export from kithara-play**

In `crates/kithara/Cargo.toml` add:
```toml
kithara-play = { workspace = true }
```

In `crates/kithara/src/lib.rs`, remove:
```rust
mod config;
mod resource;
mod source_type;
pub use config::{ResourceConfig, ResourceSrc};
pub use resource::Resource;
pub use source_type::SourceType;
```

Replace with:
```rust
pub use kithara_play::{Item, ResourceConfig, ResourceSrc, SourceType};

/// Backwards-compatible alias for `Item`.
pub type Resource = kithara_play::Item;
```

Also update the prelude:
```rust
pub mod prelude {
    // ... existing exports ...
    pub use crate::{Event, EventBus, Item, Resource, ResourceConfig};
}
```

**Step 4: Verify compilation across workspace**

Run: `cargo check --workspace`
Expected: compiles (Resource is now a type alias for Item)

Run: `cargo test -p kithara`
Expected: all existing Resource tests pass (they're now in kithara-play)

**Step 5: Commit**

```bash
git add crates/kithara-play/ crates/kithara/
git commit -m "refactor: move Resource to kithara-play as Item, add re-export alias"
```

---

## Phase 3: Engine (Firewheel Integration)

### Task 4: Implement Engine with Firewheel audio graph

This is the core audio output component. Port from zvqengine's `ZvqEngineInternal`.

**Files:**
- Create: `crates/kithara-play/src/core/engine.rs`
- Create: `crates/kithara-play/src/core/player_node.rs`
- Create: `crates/kithara-play/src/core/player_processor.rs`
- Modify: `crates/kithara-play/src/core/mod.rs`

**Reference files (zvqengine):**
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/engine/internal.rs` — Firewheel graph setup
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/node.rs` — PlayerNode AudioNode
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/processor.rs` — PlayerProcessor

**Step 1: Write test for Engine lifecycle**

Create `crates/kithara-play/src/core/engine.rs` with test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_starts_and_stops() {
        let engine = EngineImpl::new(EngineConfig::default());
        assert!(!engine.is_running());
        engine.start().unwrap();
        assert!(engine.is_running());
        engine.stop().unwrap();
        assert!(!engine.is_running());
    }

    #[test]
    fn engine_allocates_and_releases_slots() {
        let engine = EngineImpl::new(EngineConfig { max_slots: 4, ..Default::default() });
        engine.start().unwrap();
        let slot = engine.allocate_slot().unwrap();
        assert_eq!(engine.slot_count(), 1);
        engine.release_slot(slot).unwrap();
        assert_eq!(engine.slot_count(), 0);
    }
}
```

**Step 2: Implement EngineImpl**

Port Firewheel graph construction from `ZvqEngineInternal::new()` (zvqengine `internal.rs` lines 103-165).

Key structure:
```rust
pub struct EngineConfig {
    pub max_slots: usize,        // default: 4
    pub sample_rate: u32,        // default: 44100
    pub channels: u16,           // default: 2
}

pub struct EngineImpl {
    config: EngineConfig,
    firewheel_ctx: Mutex<Option<FirewheelContext>>,
    slots: Mutex<SlotArena>,
    host_sample_rate: Arc<AtomicU32>,
    events_tx: broadcast::Sender<EngineEvent>,
    running: AtomicBool,
}
```

The Firewheel graph:
```
PlayerNode[slot0] → VolPanNode[slot0] ──┐
                                         ├→ SumNode → EQ(EffectBridgeNode) → GraphOut
PlayerNode[slot1] → VolPanNode[slot1] ──┘
```

EQ is a **master effect** on the Firewheel graph, like zvqengine's EqNode.
The difference: instead of a dedicated EqNode, we use `EffectBridgeNode` that wraps
`EqEffect` (which implements `AudioEffect` from kithara-audio). This makes the adapter
reusable for any future master effect.

Port the graph construction from `ZvqEngineInternal::new()` adapting for slot-based architecture.
Replace zvqengine's `EqNode` with `EffectBridgeNode(EqEffect)` in the graph.

**Step 3: Implement Engine trait for EngineImpl**

Wire all Engine trait methods to the concrete implementation.

**Step 4: Run tests**

Run: `cargo test -p kithara-play core::engine`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/kithara-play/src/core/
git commit -m "feat: implement Engine with Firewheel audio graph"
```

---

### Task 5: Implement PlayerNode and PlayerProcessor

Port the real-time audio rendering from zvqengine.

**Files:**
- Create: `crates/kithara-play/src/core/player_node.rs`
- Create: `crates/kithara-play/src/core/player_processor.rs`
- Create: `crates/kithara-play/src/core/player_track.rs`
- Create: `crates/kithara-play/src/core/player_resource.rs`

**Reference files (zvqengine):**
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/node.rs`
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/processor.rs`
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/track.rs`
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/resource/reader.rs`

**Step 1: Write test for PlayerProcessor rendering**

Test that PlayerProcessor generates audio from a mock resource:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn processor_renders_silence_when_no_track() {
        // Create PlayerProcessor, process() with empty track arena
        // Assert output buffer is all zeros
    }

    #[test]
    fn processor_renders_audio_from_loaded_track() {
        // Create PlayerProcessor, load mock resource
        // process() should fill output with PCM data from resource
    }
}
```

**Step 2: Port PlayerNode (AudioNode impl)**

Port from zvqengine `player/node.rs`. Key adaptations:
- Remove zvqengine-specific `Track` struct, use `Item` instead
- Keep `TrackTransition` enum (FadeIn/FadeOut)
- Keep Diff/Patch mechanism for lock-free updates

**Step 3: Port PlayerProcessor (AudioNodeProcessor impl)**

Port from zvqengine `player/processor.rs`. Key adaptations:
- Replace `PlayerResource` wrapper with direct `Item` (which wraps `Box<dyn PcmReader>`)
- Keep arena management (`thunderdome::Arena`)
- Keep fade logic (MixDSP with SquareRoot curve)
- Keep notification channel (kanal)
- Remove zvqengine-specific promote/demote, replace with items-array semantics

**Step 4: Port PlayerResource wrapper**

Port from zvqengine `resource/reader.rs`. This wraps `Item` with PCM channel buffers for the real-time audio callback. Key method: `read(scratch, range)` that feeds channel data to the processor.

**Step 5: Run tests**

Run: `cargo test -p kithara-play core::player_processor`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/kithara-play/src/core/
git commit -m "feat: port PlayerNode and PlayerProcessor from zvqengine"
```

---

## Phase 4: Effects Architecture

### Task 6: Implement EQ as AudioEffect

EQ implements the existing `AudioEffect` trait from kithara-audio.
In v1, EQ runs on the **master bus** via `EffectBridgeNode` (like zvqengine's EqNode).
The AudioEffect interface makes it reusable per-track in the future (DJ: per-deck EQ).

**Files:**
- Create: `crates/kithara-audio/src/effects/eq.rs` (EQ lives in kithara-audio alongside resampler)
- Create: `crates/kithara-audio/src/effects/mod.rs`
- Modify: `crates/kithara-audio/src/lib.rs` (re-export)

**Reference files (zvqengine):**
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/eq/processor.rs` — biquad filter logic
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/eq/band.rs` — EqBand + log-spaced generation

**Step 1: Write test for EQ as AudioEffect**

```rust
#[cfg(test)]
mod tests {
    use kithara_audio::AudioEffect;

    #[test]
    fn generates_10_log_spaced_bands() {
        let bands = generate_log_spaced_bands(10);
        assert_eq!(bands.len(), 10);
        assert!((bands[0].frequency - 30.0).abs() < 1.0);
        assert!((bands[9].frequency - 18000.0).abs() < 500.0);
        for w in bands.windows(2) {
            assert!(w[1].frequency > w[0].frequency);
        }
    }

    #[test]
    fn eq_processes_pcm_chunk() {
        let bands = generate_log_spaced_bands(10);
        let mut eq = EqEffect::new(bands, 44100, 2);

        // Create a PcmChunk with test data
        let chunk = make_test_chunk(1024, 2, 44100);

        // Process through EQ (all gains at 0dB = passthrough)
        let result = AudioEffect::process(&mut eq, chunk.clone());
        assert!(result.is_some());
        // With 0dB gain, output ≈ input
    }

    #[test]
    fn eq_gain_boost_increases_amplitude() {
        let mut bands = generate_log_spaced_bands(10);
        bands[5].gain_db = 12.0;  // +12dB on band 5
        let mut eq = EqEffect::new(bands, 44100, 2);

        // Process sine wave at band 5's center frequency
        // Assert output amplitude > input amplitude
    }

    #[test]
    fn eq_reset_clears_filter_state() {
        let bands = generate_log_spaced_bands(10);
        let mut eq = EqEffect::new(bands, 44100, 2);

        let chunk = make_test_chunk(1024, 2, 44100);
        AudioEffect::process(&mut eq, chunk);
        AudioEffect::reset(&mut eq);
        // After reset, filter state is clean
    }
}
```

**Step 2: Implement EqBand and log-spaced generation**

Port from zvqengine `eq/band.rs`:
```rust
pub struct EqBandConfig {
    pub frequency: f32,  // Hz
    pub q_factor: f32,
    pub gain_db: f32,    // -24.0 to +6.0
}

pub fn generate_log_spaced_bands(count: usize) -> Vec<EqBandConfig> {
    // Port from zvqengine: 30Hz to 18kHz, logarithmic spacing
    // Q factor scales with band count: 1.4 * sqrt(count/10)
}
```

**Step 3: Implement EqEffect as AudioEffect**

```rust
pub struct EqEffect {
    bands: Vec<EqBandConfig>,
    filters_l: Vec<DirectForm1<f32>>,  // Left channel biquad per band
    filters_r: Vec<DirectForm1<f32>>,  // Right channel biquad per band
    sample_rate: u32,
    channels: usize,
}

impl AudioEffect for EqEffect {
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        // Deinterleave chunk → apply biquad chain per channel → re-interleave
        // Port filter logic from zvqengine eq/processor.rs
    }

    fn flush(&mut self) -> Option<PcmChunk> { None }

    fn reset(&mut self) {
        // Reset all biquad filter states
    }
}

impl EqEffect {
    /// Update gain for a specific band (called from main thread).
    /// Filter coefficients recalculated on next process() call.
    pub fn set_gain(&mut self, band_index: usize, gain_db: f32) { ... }

    pub fn bands(&self) -> &[EqBandConfig] { &self.bands }
}
```

**Step 4: Run tests**

Run: `cargo test -p kithara-audio effects::eq`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/kithara-audio/src/effects/
git commit -m "feat: implement EQ as AudioEffect (biquad N-band parametric)"
```

---

### Task 6b: Implement EffectBridgeNode (AudioEffect → Firewheel adapter)

Allows any `AudioEffect` to run as a Firewheel `AudioNode` on the master bus.

**Files:**
- Create: `crates/kithara-play/src/core/effect_bridge.rs`

**Step 1: Write test for EffectBridgeNode**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn bridge_passes_audio_through_effect() {
        // Create a simple pass-through AudioEffect
        // Wrap in EffectBridgeNode
        // Verify audio passes through correctly
    }

    #[test]
    fn bridge_applies_eq_to_firewheel_buffers() {
        // Create EqEffect with +6dB on a band
        // Wrap in EffectBridgeNode
        // Process Firewheel-style buffers
        // Verify EQ was applied
    }
}
```

**Step 2: Implement EffectBridgeNode**

```rust
/// Firewheel AudioNode that wraps an AudioEffect.
///
/// Converts between Firewheel's raw buffer format and PcmChunk,
/// delegates processing to the wrapped AudioEffect.
pub struct EffectBridgeNode {
    effect: Option<Box<dyn AudioEffect>>,
}

/// Firewheel AudioNodeProcessor for the bridge.
pub struct EffectBridgeProcessor {
    effect: Box<dyn AudioEffect>,
    spec: PcmSpec,
}

impl AudioNode for EffectBridgeNode {
    // Diff/Patch: can swap effect at runtime
    type Processor = EffectBridgeProcessor;
    // ...
}

impl AudioNodeProcessor for EffectBridgeProcessor {
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], ...) {
        // 1. Pack input buffers into PcmChunk (interleaved)
        // 2. Call self.effect.process(chunk)
        // 3. Unpack result PcmChunk into output buffers
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p kithara-play core::effect_bridge`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/kithara-play/src/core/effect_bridge.rs
git commit -m "feat: implement EffectBridgeNode (AudioEffect → Firewheel adapter)"
```

---

### Task 6c: Add per-track effects injection to Audio<S>

Currently `create_effects()` in kithara-audio hardcodes `[Resampler]`.
Add API to inject additional per-track effects.

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/config.rs` (AudioConfig)
- Modify: `crates/kithara-audio/src/pipeline/audio.rs` (Audio::new)

**Step 1: Write test for custom effects injection**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn audio_config_accepts_custom_effects() {
        let eq = EqEffect::new(generate_log_spaced_bands(10), 44100, 2);
        let config = AudioConfig::<File>::new(file_config)
            .with_effect(Box::new(eq));
        // Effects chain will be: [Resampler, EqEffect]
    }
}
```

**Step 2: Add effects field to AudioConfig**

In `crates/kithara-audio/src/pipeline/config.rs`:
```rust
pub struct AudioConfig<T: StreamType> {
    // ... existing fields ...
    /// Additional effects to append after resampler in the processing chain.
    pub effects: Vec<Box<dyn AudioEffect>>,
}

impl<T: StreamType> AudioConfig<T> {
    // ... existing methods ...

    /// Add an audio effect to the processing chain (runs after resampler).
    pub fn with_effect(mut self, effect: Box<dyn AudioEffect>) -> Self {
        self.effects.push(effect);
        self
    }
}
```

**Step 3: Update create_effects to include custom effects**

```rust
pub(super) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    quality: ResamplerQuality,
    pool: Option<PcmPool>,
    custom_effects: Vec<Box<dyn AudioEffect>>,  // NEW parameter
) -> Vec<Box<dyn AudioEffect>> {
    let mut chain: Vec<Box<dyn AudioEffect>> = vec![
        Box::new(ResamplerProcessor::new(params)),
    ];
    chain.extend(custom_effects);  // Append user effects after resampler
    chain
}
```

**Step 4: Run tests**

Run: `cargo test -p kithara-audio`
Expected: all existing + new tests pass

**Step 5: Commit**

```bash
git add crates/kithara-audio/
git commit -m "feat: add per-track effects injection to AudioConfig"
```

---

## Phase 5: Crossfade

### Task 7: Implement sequential crossfade

**Files:**
- Create: `crates/kithara-play/src/core/crossfade.rs`
- Create: `crates/kithara-play/src/core/mix_dsp.rs`

**Reference files (zvqengine):**
- `/Users/litvinenko-pv/code/zvqengine/crates/core/src/player/track.rs` — MixDSP, FadeCurve

**Step 1: Write test for MixDSP fade curves**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn square_root_fade_in_is_smooth() {
        let mut dsp = MixDsp::new(FadeCurve::SquareRoot);
        dsp.set_fade_duration(1.0);  // 1 second
        dsp.start_fade_in();
        // Process 44100 samples
        // Assert: starts near 0, ends near 1, monotonically increasing
    }

    #[test]
    fn crossfade_overlaps_correctly() {
        // Two MixDsp instances: one fading out, one fading in
        // Sum of gains at any point should be ~1.0 (equal-power)
    }
}
```

**Step 2: Port MixDsp**

Port from zvqengine `player/track.rs` MixDSP section:
- `FadeCurve::SquareRoot` (default, equal-power)
- `MixDsp { smoother, fade_state, target_gain }`
- `process(buffer, gain)` — applies envelope

**Step 3: Implement CrossfadeImpl**

Wraps two MixDsp instances for sequential crossfade between items:
```rust
pub struct CrossfadeImpl {
    duration: AtomicF32,
    curve: CrossfadeCurve,
    outgoing: MixDsp,
    incoming: MixDsp,
    active: AtomicBool,
    progress: AtomicF32,
}
```

**Step 4: Run tests**

Run: `cargo test -p kithara-play core::crossfade`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/kithara-play/src/core/
git commit -m "feat: implement sequential crossfade with MixDsp"
```

---

## Phase 6: Player

### Task 8: Implement Player with items array

**Files:**
- Create: `crates/kithara-play/src/core/player.rs`
- Modify: `crates/kithara-play/src/core/mod.rs`
- Modify: `crates/kithara-play/src/lib.rs`

**Step 1: Write test for Player items management**

```rust
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn player_insert_and_remove_items() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let item = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 1.0)));

        assert!(player.items().is_empty());
        player.insert(item.clone(), None);
        assert_eq!(player.items().len(), 1);
        player.remove(&item);
        assert!(player.items().is_empty());
    }

    #[tokio::test]
    async fn player_play_sets_rate() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let item = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 1.0)));
        player.insert(item, None);

        assert_eq!(player.rate(), 0.0);  // paused
        player.play();
        assert!((player.rate() - 1.0).abs() < f32::EPSILON);  // default rate
    }

    #[tokio::test]
    async fn player_seek_updates_position() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let item = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 10.0)));
        player.insert(item, None);

        let (tx, rx) = tokio::sync::oneshot::channel();
        player.seek(Duration::from_secs(5), move |success| { let _ = tx.send(success); });
        assert!(rx.await.unwrap());
    }
}
```

**Step 2: Implement PlayerImpl struct**

```rust
pub struct PlayerConfig {
    pub max_slots: usize,
    pub default_rate: f32,
    pub crossfade_duration: f32,
    pub eq_bands: usize,
}

pub struct PlayerImpl {
    engine: Arc<EngineImpl>,
    items: Mutex<Vec<Arc<Item>>>,
    current_index: AtomicUsize,
    // Atomics for polling
    default_rate: AtomicF32,
    current_time: AtomicF64,
    rate: AtomicF32,
    volume: AtomicF32,
    pan: AtomicF32,
    crossfade_duration: AtomicF32,
    // Errors
    error: Mutex<Option<PlayError>>,
    // Config
    quality: Mutex<Option<StreamQuality>>,
    authorization: Mutex<Option<String>>,
    // Events
    events_tx: broadcast::Sender<PlayerEvent>,
}
```

**Step 3: Implement Player public API**

Methods matching iOS `AudioPlayerProtocol`:
- `new(config)` — creates Engine internally, starts Firewheel graph
- `items()` — returns current items list
- `insert(item, after)` — adds item, triggers preload if 2nd item
- `remove(item)` — removes item
- `remove_all_items()` — clears all
- `play()` — sets rate to default_rate, starts audio
- `pause()` — sets rate to 0
- `seek(to, completion)` — delegates to current item
- `current_time()` — reads AtomicF64
- `rate()` — reads AtomicF32
- `set_default_rate(rate)` — stores, adjusts host_sample_rate for resampler
- `current_item()` — items[current_index]
- `set_eq_gain(band, db)` — delegates to Engine's master EQ (EffectBridgeNode)
- `eq_bands()` — returns master EQ band definitions
- `set_crossfade_duration(secs)` — stores in atomic
- `set_volume(vol)` / `volume()` — delegates to Engine VolumePan
- `set_pan(pan)` — delegates to Engine VolumePan
- `set_quality(quality)` — for ABR selection
- `set_authorization(token)` — for HLS keys
- `subscribe()` — broadcast receiver

**Step 4: Implement update loop**

Port from zvqengine `ZvqEngineInternal` update thread:
- 15ms polling interval
- Read notifications from PlayerProcessor
- Update atomics (position, duration)
- Emit broadcast events (TrackChanged, TrackEnded, AboutToEnd)
- Auto-advance to next item on track end
- Trigger crossfade when transitioning

**Step 5: Run tests**

Run: `cargo test -p kithara-play core::player`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/kithara-play/src/core/
git commit -m "feat: implement Player with items array and event loop"
```

---

### Task 9: Wire Player → Engine → Firewheel pipeline

**Files:**
- Modify: `crates/kithara-play/src/core/player.rs`
- Modify: `crates/kithara-play/src/core/engine.rs`
- Modify: `crates/kithara-play/src/core/mod.rs`
- Modify: `crates/kithara-play/src/lib.rs`

**Step 1: Write integration test**

```rust
#[cfg(test)]
mod integration_tests {
    #[tokio::test]
    async fn player_plays_mock_audio() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let item = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 2.0)));
        player.insert(item, None);
        player.play();

        // Wait for some audio to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Position should have advanced
        assert!(player.current_time() > 0.0);
        assert!((player.rate() - 1.0).abs() < f32::EPSILON);

        player.pause();
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }
}
```

**Step 2: Connect Player.play() to Engine**

When `play()` is called:
1. Get current Item from items array
2. If not loaded, trigger load
3. Wrap Item's PcmReader in PlayerResource (with PCM buffers)
4. Send to Engine's PlayerNode via Diff/Patch
5. Engine starts rendering audio through Firewheel graph

**Step 3: Connect Engine events back to Player**

Engine's PlayerProcessor sends notifications:
- `TrackPlaybackStarted` → Player emits `PlayerEvent::StatusChanged`
- `TrackAboutToEnd` → Player preloads next item, queues crossfade
- `TrackEnded` → Player advances to next item

**Step 4: Export public API from lib.rs**

```rust
// Concrete implementations
pub use core::player::{PlayerConfig, PlayerImpl};
pub use core::engine::{EngineConfig, EngineImpl};
pub use core::item::Item;
pub use core::config::{ResourceConfig, ResourceSrc};
pub use core::source_type::SourceType;
```

**Step 5: Run tests**

Run: `cargo test -p kithara-play`
Expected: all tests pass

**Step 6: Commit**

```bash
git add crates/kithara-play/
git commit -m "feat: wire Player → Engine → Firewheel pipeline"
```

---

## Phase 7: Integration

### Task 10: Update kithara facade

**Files:**
- Modify: `crates/kithara/src/lib.rs`
- Modify: `crates/kithara/Cargo.toml`
- Modify: `crates/kithara/src/prelude.rs` (if exists)

**Step 1: Add kithara-play re-exports to facade**

```rust
// In crates/kithara/src/lib.rs
pub mod play {
    pub use kithara_play::*;
}

// Top-level convenience re-exports
pub use kithara_play::{Item, PlayerImpl, ResourceConfig, ResourceSrc, SourceType};

/// Backwards-compatible alias.
pub type Resource = kithara_play::Item;
```

**Step 2: Update prelude**

```rust
pub mod prelude {
    // ... existing ...
    pub use crate::{Event, EventBus, Item, Resource, ResourceConfig};
    pub use kithara_play::{PlayerConfig, PlayerImpl};
}
```

**Step 3: Verify workspace compiles**

Run: `cargo check --workspace`
Run: `cargo test --workspace`
Expected: all pass

**Step 4: Commit**

```bash
git add crates/kithara/
git commit -m "feat: re-export kithara-play types from facade"
```

---

### Task 11: Integration tests with real audio

**Files:**
- Create: `tests/kithara_play/mod.rs` (or add to existing tests/ crate)

**Step 1: Write integration test with file playback**

```rust
#[tokio::test]
async fn play_local_wav_file() {
    let player = PlayerImpl::new(PlayerConfig::default());

    // Use test fixture WAV file
    let config = ResourceConfig::new("/path/to/test.wav").unwrap();
    let item = Arc::new(Item::new(config).await.unwrap());

    player.insert(item, None);
    player.play();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(player.current_time() > 0.0);

    player.pause();
}
```

**Step 2: Write integration test for crossfade**

```rust
#[tokio::test]
async fn crossfade_between_two_items() {
    let player = PlayerImpl::new(PlayerConfig {
        crossfade_duration: 0.5,
        ..Default::default()
    });

    let item1 = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 2.0)));
    let item2 = Arc::new(Item::from_reader(MockPcmReader::new(mock_spec(), 2.0)));

    player.insert(item1, None);
    player.insert(item2.clone(), None);  // after item1
    player.play();

    // Subscribe to events
    let mut events = player.subscribe();

    // Wait for first track to end and crossfade to trigger
    // ...
}
```

**Step 3: Write integration test for EQ**

```rust
#[tokio::test]
async fn eq_gain_changes_apply() {
    let player = PlayerImpl::new(PlayerConfig::default());
    let bands = player.eq_bands();
    assert!(!bands.is_empty());

    // Set gain on first band
    player.set_eq_gain(0, 6.0);
    assert!((player.eq_bands()[0].gain_db - 6.0).abs() < f32::EPSILON);
}
```

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: all pass

**Step 5: Commit**

```bash
git add tests/
git commit -m "test: add kithara-play integration tests"
```

---

## Phase 8: Lint and cleanup

### Task 12: Final lint pass

**Step 1: Format**

Run: `cargo fmt --all`

**Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`
Fix any issues.

**Step 3: Style lint**

Run: `bash scripts/ci/lint-style.sh`
Fix any issues.

**Step 4: Full test suite**

Run: `cargo test --workspace`
Expected: all pass

**Step 5: Commit**

```bash
git add -A
git commit -m "chore: lint cleanup for kithara-play implementation"
```

---

## Key Porting Reference

When porting from zvqengine, use these file mappings:

| zvqengine source | kithara target | Key changes |
|---|---|---|
| `engine/internal.rs` (Firewheel graph) | `kithara-play/core/engine.rs` | Slot-based, no EQ node in graph |
| `player/node.rs` (AudioNode) | `kithara-play/core/player_node.rs` | Items-array semantics |
| `player/processor.rs` (AudioNodeProcessor) | `kithara-play/core/player_processor.rs` | Replace Track with Item |
| `player/track.rs` (PlayerTrack + MixDSP) | `kithara-play/core/player_track.rs` + `mix_dsp.rs` | Keep fade logic |
| `resource/reader.rs` (PlayerResource) | `kithara-play/core/player_resource.rs` | Wrap Item instead of kithara::Resource |
| `eq/processor.rs` (EqProcessor) | `kithara-audio/effects/eq.rs` | **AudioEffect impl**, not Firewheel node |
| `eq/band.rs` (EqBand) | `kithara-audio/effects/eq.rs` | Keep log-spaced generation |
| `eq/node.rs` (EqNode) | `kithara-play/core/effect_bridge.rs` | **Replaced by generic EffectBridgeNode** |
| `engine/engine.rs` (public API) | `kithara-play/core/player.rs` | items-array, iOS API |
| `engine/listener.rs` (callbacks) | events.rs (broadcast) | Replace callbacks with broadcast |

### Effects flow diagram

```
v1: Master EQ (via Firewheel EffectBridgeNode — like zvqengine)
  Decoder → [Resampler] → PcmChunk → channel → Firewheel PlayerNode → VolPan → Sum
    → EffectBridgeNode(EqEffect) → GraphOut

Per-track custom effects (via AudioConfig::with_effect):
  Decoder → [Resampler, ...custom effects...] → PcmChunk → channel → Firewheel

EqEffect implements AudioEffect trait — same code works at both levels.
```

## Notes for implementor

1. **No `unwrap()`/`expect()` in production code** — use `PlayError` or `tracing::warn!`
2. **Use `tracing` for logging** — never `println!` or `dbg!`
3. **No buffer allocations in hot paths** — use `kithara_bufpool::SharedPool`
4. **Run `cargo fmt --all` before each commit**
5. **Firewheel's Diff/Patch is lock-free** — main thread writes Diff, audio thread reads Patch
6. **`kanal` channels are realtime-safe** — use bounded channels for audio ↔ main thread
7. **Speed control = resampling ratio** — `set_default_rate(2.0)` → set `host_sample_rate = source_rate / 2`
