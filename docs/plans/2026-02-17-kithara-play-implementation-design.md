# kithara-play Implementation Design

**Date:** 2026-02-17
**Status:** Approved
**Goal:** Port zvqengine audio layer into kithara-play, providing a minimal iOS-ready player with DJ-ready architecture.

## Context

- **zvqengine** has a working audio engine built on Firewheel (audio graph) + kithara (decode/stream)
- **kithara-play** currently contains only trait definitions (AVPlayer-inspired API + DJ subsystem)
- iOS team needs a minimal API (`AudioPlayerProtocol` / `AudioPlayerItemProtocol`) to replace AVPlayer
- DJ features (BPM, beat sync, effects) are NOT implemented in v1 but architecture must support them

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Audio engine | Firewheel (keep from zvqengine) | Already working, supports all platforms |
| Playback model | items-array (iOS style) | Matches iOS API, auto-advance |
| Reactive state | Atomics (hot path) + broadcast (events) | Efficient for UI progress bars |
| Speed control | Existing resampler (`Arc<AtomicU32>` host_sample_rate) | Already built, dynamic ratio |
| Trait preservation | Keep all traits for mock testing | DJ traits stay as future contracts |
| EQ | AudioEffect in kithara-audio, on master bus via EffectBridgeNode | Like zvqengine: master EQ, not per-track |
| Effects | Per-track (AudioEffect chain) + master (EffectBridgeNode) | Unified trait for both levels |
| Crossfade | Sequential (v1) + parallel slot (future DJ) | Two code paths, shared CrossfadeConfig |
| Resource → Item | Move from kithara facade to kithara-play | kithara facade becomes re-exports only |

## Architecture

### Crate structure

```
kithara-play/
├── src/
│   ├── lib.rs              — public API re-exports
│   ├── traits/             — trait definitions (kept for mocking)
│   │   ├── player.rs       — Player trait
│   │   ├── item.rs         — PlayerItem trait
│   │   ├── engine.rs       — Engine trait
│   │   ├── crossfade.rs    — CrossfadeController trait
│   │   ├── mixer.rs        — Mixer trait (future DJ)
│   │   └── dj/             — BpmAnalyzer, BpmSync, DjEffect (future)
│   ├── impl/               — concrete implementations
│   │   ├── player.rs       — struct Player (impl Player trait)
│   │   ├── item.rs         — struct Item (ex-Resource, impl PlayerItem)
│   │   ├── engine.rs       — struct Engine (Firewheel graph)
│   │   ├── bridge.rs       — EffectBridgeNode (AudioEffect → Firewheel adapter)
│   │   └── crossfade.rs    — struct Crossfade (fade logic from zvqengine)
│   ├── events.rs           — PlayerEvent, ItemEvent, EngineEvent
│   ├── error.rs            — PlayError
│   ├── time.rs             — MediaTime (kept)
│   ├── types.rs            — SlotId, ObserverId, EqBand, etc.
│   └── metadata.rs         — Metadata, Artwork
```

### Dependencies

```
kithara-play
├── kithara-audio    (Audio<S>, PcmReader, Resampler, AudioEffect, Eq)
├── kithara-stream   (StreamType, MediaInfo)
├── kithara-hls      (Hls, HlsConfig)
├── kithara-file     (File, FileConfig)
├── firewheel        (audio graph + output)
└── tokio, kanal, tracing
```

### Firewheel audio graph (DJ-ready)

```
Slot 0: PlayerNode[0] → VolPanNode[0] ──┐
                                          ├→ SumNode → EQ(EffectBridgeNode) → GraphOut
Slot 1: PlayerNode[1] → VolPanNode[1] ──┘
        (v1: unused, graph ready for DJ)
```

- EQ runs on the master bus via EffectBridgeNode (like zvqengine's EqNode, but as AudioEffect adapter)
- Per-track effects (custom) can be injected into Audio<S> pipeline via AudioConfig::with_effect()
- Master effects (EQ + others) use EffectBridgeNode adapter: AudioEffect → Firewheel AudioNode
- Each slot has independent volume/pan in the Firewheel graph
- v1 uses slot 0 only; DJ mode allocates slot 1+
- Sequential crossfade (item transition) happens within a single slot's PlayerNode
- Parallel crossfade (DJ mixing) happens between slots via MasterMix

### Effects architecture (unified)

Two levels sharing the same `AudioEffect` trait from kithara-audio:

- **Per-track effects** (kithara-audio pipeline): custom effects injected via `AudioConfig::with_effect()`
  - Run in Audio<S> worker thread after decoder + resampler
  - Independent per item/track
- **Master effects** (Firewheel graph): `EffectBridgeNode` wraps `AudioEffect` into Firewheel `AudioNode`
  - **EQ lives here** — on the master bus, like zvqengine's EqNode
  - Run on Firewheel audio thread, applied to mixed output
  - Shared across all slots (e.g., master EQ, master limiter)

```rust
/// Adapter: AudioEffect → Firewheel AudioNode
pub struct EffectBridgeNode {
    effect: Box<dyn AudioEffect>,
}

impl AudioNode for EffectBridgeNode {
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], ..) {
        // Convert Firewheel buffers → PcmChunk, call effect.process(), write back
    }
}
```

## Component Design

### Item (ex-Resource)

Moved from `crates/kithara/src/resource.rs` to `kithara-play`.

```rust
pub struct Item {
    inner: Box<dyn PcmReader>,
    bus: EventBus,
    url: String,
    headers: Option<HashMap<String, String>>,
    preferred_peak_bitrate: AtomicF64,
    preferred_peak_bitrate_expensive: AtomicF64,
    // Observable via atomics
    buffered_duration: AtomicF64,
    duration: AtomicF64,
    error: Mutex<Option<PlayError>>,
    status: AtomicU8,  // ItemStatus
}
```

Constructor matches iOS API:
```rust
impl Item {
    pub async fn new(url: &str, headers: Option<HashMap<String, String>>) -> Self;
    pub fn load(&self);  // starts stream loading
}
```

### Player

```rust
pub struct Player {
    engine: Arc<Engine>,
    items: Mutex<Vec<Arc<Item>>>,
    current_index: AtomicUsize,
    default_rate: AtomicF32,
    // Atomics for iOS Observable mapping
    current_time: AtomicF64,
    rate: AtomicF32,           // 0 = paused, default_rate = playing
    error: Mutex<Option<PlayError>>,
    // Config
    crossfade_duration: AtomicF32,
    quality: Mutex<StreamQuality>,
    authorization: Mutex<Option<String>>,
    // Events
    events_tx: broadcast::Sender<PlayerEvent>,
}
```

Public API (covers iOS `AudioPlayerProtocol`):
```rust
impl Player {
    pub fn new() -> Self;
    // Item management
    pub fn items(&self) -> Vec<Arc<Item>>;
    pub fn insert(&self, item: Arc<Item>, after: Option<&Item>);
    pub fn remove(&self, item: &Item);
    pub fn remove_all_items(&self);
    // Playback
    pub fn play(&self);
    pub fn pause(&self);
    pub fn seek(&self, to: Duration, completion: impl FnOnce(bool) + Send);
    // State (atomics, polled by UI)
    pub fn current_time(&self) -> f64;
    pub fn rate(&self) -> f32;
    pub fn set_default_rate(&self, rate: f32);
    pub fn current_item(&self) -> Option<Arc<Item>>;
    // EQ
    pub fn set_eq_gain(&self, band: usize, db: f32);
    pub fn eq_bands(&self) -> Vec<EqBand>;
    // Crossfade
    pub fn set_crossfade_duration(&self, seconds: f32);
    // Volume & Pan
    pub fn set_volume(&self, volume: f32);
    pub fn volume(&self) -> f32;
    pub fn set_pan(&self, pan: f32);
    // Quality & Auth
    pub fn set_quality(&self, quality: StreamQuality);
    pub fn set_authorization(&self, token: &str);
    // Events
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;
}
```

### Engine

```rust
pub struct Engine {
    firewheel_ctx: FirewheelContext,
    player_nodes: Vec<NodeId>,
    vol_pan_nodes: Vec<NodeId>,
    master_effect_nodes: Vec<NodeId>,  // EffectBridgeNode instances
    host_sample_rate: Arc<AtomicU32>,
    slots: Mutex<Vec<SlotState>>,
    max_slots: usize,
}
```

Internal component — not exposed to iOS. Manages Firewheel graph lifecycle.

### Eq (master bus, AudioEffect via EffectBridgeNode)

Lives in kithara-audio, implements `AudioEffect` trait.
Runs on master bus via `EffectBridgeNode` (like zvqengine's EqNode in Firewheel graph):
- N logarithmically-spaced bands (30Hz–18kHz)
- Biquad filters (DirectForm1) per channel per band
- Gain range: -24dB to +6dB
- Kill switches per band
- Master placement: `EffectBridgeNode(EqEffect)` in Firewheel graph after SumNode
- Can also be used per-track via `AudioConfig::with_effect()` if needed (future DJ: per-deck EQ)

### Crossfade

Two modes sharing `CrossfadeConfig`:
- **Sequential** (v1): MixDSP with SquareRoot fade curve within PlayerNode
- **Parallel** (future DJ): fade between slots via Mixer

```rust
pub struct CrossfadeConfig {
    pub duration: Duration,
    pub curve: CrossfadeCurve,
    pub beat_aligned: bool,  // v1: always false, DJ: BPM-aware
}
```

## Reactive State

| Field | Storage | Update source | Read by |
|-------|---------|---------------|---------|
| `position` | `AtomicF64` | Audio worker thread (~15ms) | UI poll (16ms) |
| `duration` | `AtomicF64` | On item load | UI poll |
| `rate` | `AtomicF32` | On play/pause | UI poll |
| `volume` | `AtomicF32` | On set_volume | UI poll |
| `buffered_duration` | `AtomicF64` | Download progress | UI poll |
| Track changed | `broadcast` | On transition | UI subscribe |
| Error | `broadcast` | On failure | UI subscribe |
| Seek completed | `broadcast` | On seek done | UI subscribe |
| About to end | `broadcast` | ~5s before end | Preload trigger |

iOS wrapper: periodic timer reads atomics → Observable<Double>, broadcast events → Observable<Error>.

## Error Handling

```rust
#[non_exhaustive]
pub enum PlayError {
    // Item
    ItemFailed { reason: String },
    ItemNotReady,
    // Playback
    SeekFailed { position: Duration },
    DecodeFailed { reason: String },
    // Engine
    EngineNotRunning,
    EngineAlreadyRunning,
    SlotNotFound(SlotId),
    ArenaFull,
    // Network / Auth
    NetworkError { reason: String },
    AuthenticationRequired,
    // Crossfade
    CrossfadeActive,
    NoCrossfade,
    // EQ
    InvalidEqBand { index: usize },
    // DJ (architectural placeholders)
    BpmAnalysisFailed { reason: String },
    BpmUnknown,
    EffectParameterNotFound { name: String },
    InvalidParameter { name: String, value: f32 },
    // Session (future)
    SessionActivationFailed { reason: String },
    RouteUnavailable { reason: String },
    // Generic
    Internal(String),
}
```

## What Gets Ported from zvqengine

| zvqengine component | kithara-play target | Notes |
|---------------------|---------------------|-------|
| `ZvqEngineInternal` (Firewheel graph setup) | `Engine` | Adapted for slot arena |
| `PlayerNode` + `PlayerProcessor` | `Engine` internals | PCM rendering per slot |
| `PlayerResource` + `PlayerTrack` | `Item` internals | Merged with existing Resource |
| `EqNode` + `EqProcessor` | `Eq` (AudioEffect in kithara-audio) | Per-track, N bands, reuses AudioEffect trait |
| `MixDSP` + fade logic | `Crossfade` | SquareRoot curve |
| `TrackLoader` (LRU cache) | `Player` internals | Preload next item |
| `SharedState` (atomics) | `Player` fields | position/duration/rate/volume |
| `PlayerNotification` events | `PlayerEvent` broadcast | Unified event system |
| `ListenerRegistry` | Not ported | Replaced by broadcast channels |
| `Dirty<T>` tracking | Not ported | Firewheel Diff/Patch handles this |

## iOS API Mapping

```
AudioPlayerItemProtocol          → Item
  .preferredPeakBitRate          → item.set_preferred_peak_bitrate()
  .bufferedDuration              → item.buffered_duration() (AtomicF64)
  .duration                      → item.duration() (AtomicF64)
  .error                         → item.error() + ItemEvent broadcast
  .init(url, headers)            → Item::new(url, headers)
  .load()                        → item.load()

AudioPlayerProtocol              → Player
  .defaultRate                   → player.set_default_rate() / default_rate()
  .currentItem                   → player.current_item()
  .currentTime                   → player.current_time() (AtomicF64)
  .rate                          → player.rate() (AtomicF32)
  .error                         → player.error() + PlayerEvent broadcast
  .init()                        → Player::new()
  .items()                       → player.items()
  .insert(item, after)           → player.insert(item, after)
  .remove(item)                  → player.remove(item)
  .removeAllItems()              → player.remove_all_items()
  .play()                        → player.play()
  .pause()                       → player.pause()
  .seek(to, completion)          → player.seek(to, completion)
```

## Testing Strategy

- **Unit tests:** Mock Engine/Item via traits, test Player logic
- **EQ:** Pure math, test with synthetic PCM data in kithara-audio (AudioEffect impl)
- **EffectBridgeNode:** Verify AudioEffect→Firewheel adapter with mock effects
- **Crossfade:** Pure math, test with synthetic PCM data
- **Integration:** Player + Engine + real Item from test files
- **Stress tests:** Rapid seek during crossfade (from existing test suite)
- **Feature flag:** `test-utils` enables mocks from kithara-audio

## Future Work (Not in v1)

- AudioSession trait implementation (platform FFI)
- DJ: multi-slot mixing, BPM detection/sync, effects
- Beat-aligned crossfade
- Mixer trait implementation (per-slot gain/pan/mute/solo)
- Device switching callbacks
