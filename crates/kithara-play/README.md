# kithara-play

Trait-based player architecture mirroring Apple AVPlayer API with DJ engine capabilities.

## Public contract

The crate defines **traits only** (no implementations). All traits are independently
mockable via `unimock` (behind `test-utils` feature) unless noted otherwise.

## Architecture overview

```
                   ┌─────────────────────────────────────────────┐
                   │                  Engine                      │
                   │  (singleton — arena of slots, audio output)  │
                   │                                              │
                   │  ┌─────────┐  ┌─────────┐  ┌─────────┐     │
                   │  │ Slot 0  │  │ Slot 1  │  │ Slot N  │     │
                   │  │ Player  │  │ Player  │  │ Player  │     │
                   │  │  Item   │  │  Item   │  │  Item   │     │
                   │  │  Asset  │  │  Asset  │  │  Asset  │     │
                   │  └────┬────┘  └────┬────┘  └────┬────┘     │
                   │       │            │            │           │
                   │       ▼            ▼            ▼           │
                   │  ┌──────────────────────────────────────┐   │
                   │  │              Mixer                    │   │
                   │  │  gain · pan · mute · solo · EQ       │   │
                   │  │  per-channel + master bus             │   │
                   │  │  hardware-style crossfader            │   │
                   │  └────────────────┬─────────────────────┘   │
                   │                   │                          │
                   │  ┌────────────────▼─────────────────────┐   │
                   │  │         DJ subsystem                  │   │
                   │  │  CrossfadeController · BpmAnalyzer    │   │
                   │  │  BpmSync · Equalizer · DjEffect       │   │
                   │  └──────────────────────────────────────┘   │
                   │                                              │
                   │  ┌──────────────────────────────────────┐   │
                   │  │          AudioSession                 │   │
                   │  │  category · mode · route · latency    │   │
                   │  └──────────────────────────────────────┘   │
                   └─────────────────────────────────────────────┘
```

## Trait catalogue

### AVPlayer API surface (no unimock — high-level consumer API)

| Trait | AVPlayer equivalent | Key methods |
|-------|-------------------|-------------|
| `Asset` | `AVAsset` | `duration`, `is_playable`, `metadata`, `url` |
| `PlayerItem` | `AVPlayerItem` | `status`, `current_time`, `duration`, `loaded_time_ranges`, `seek`, buffering state |
| `Player` | `AVPlayer` | `play`, `pause`, `seek`, `rate`, `volume`, `replace_current_item`, time observers |
| `QueuePlayer` | `AVQueuePlayer` | `items`, `advance_to_next_item`, `insert`, `remove` |
| `AudioSession` | `AVAudioSession` | `category`, `mode`, `set_active`, `output_volume`, `current_route` |

`Player` and `QueuePlayer` use `Box<dyn Fn>` callbacks for time observers, which
makes them unsuitable for unimock's matching ergonomics. They are tested through
integration tests.

`Asset` has unimock support (simple return types).
`AudioSession` has unimock support.

### Engine & mixing (unimock-enabled)

| Trait | Role |
|-------|------|
| `Engine` | Singleton lifecycle, arena slot management, master output, crossfade delegation |
| `Mixer` | Per-channel gain/pan/mute/solo, master bus, per-channel 3-band EQ, hardware crossfader |

### DJ subsystem (unimock-enabled)

| Trait | Role |
|-------|------|
| `CrossfadeController` | Start/cancel crossfade between two slots, curve control, progress |
| `BpmAnalyzer` | BPM detection, beat grid, tap tempo |
| `BpmSync` | Sync follower to leader BPM, phase offset, nudge, quantize |
| `Equalizer` | Per-slot 3-band EQ with kill switches, configurable frequencies |
| `DjEffect` | Pluggable effects (filter, echo, reverb, flanger, brake, etc.) |

## Key types

### Identity

- `SlotId` — arena position in the engine (opaque, `Copy`)
- `ObserverId` — time observer handle (opaque, `Copy`)

### Time

- `MediaTime` — high-precision time with value/timescale (mirrors `CMTime`), supports arithmetic and `Ord`

### Status enums

- `PlayerStatus` — `Unknown | ReadyToPlay | Failed`
- `TimeControlStatus` — `Paused | WaitingToPlay | Playing`
- `ItemStatus` — `Unknown | ReadyToPlay | Failed`
- `WaitingReason` — why playback is stalled
- `ActionAtItemEnd` — `Advance | Pause | None`

### Audio session

- `SessionCategory` — `Ambient | SoloAmbient | Playback | Record | PlayAndRecord | MultiRoute`
- `SessionMode` — `Default | VoiceChat | VideoChat | GameChat | Measurement | MoviePlayback | SpokenAudio`
- `SessionOptions` — mix/duck/bluetooth/airplay flags
- `PortType`, `PortDescription`, `RouteDescription` — audio routing

### DJ types

- `CrossfadeCurve` — `EqualPower | Linear | SCurve | ConstantPower | FastFadeIn | FastFadeOut`
- `CrossfadeConfig` — duration, curve, beat-aligned flag, cut points
- `BpmInfo` — detected BPM, confidence, first beat offset
- `BeatGrid` — BPM, offset, beats per bar
- `EqBand` — `Low | Mid | High`
- `EqConfig` — frequency and Q per band
- `DjEffectKind` — `Filter | Echo | Reverb | Flanger | Phaser | Brake | Spinback | Backspin | Gate | BitCrusher`

## Event system

Events use `tokio::sync::broadcast`. Subscribe via `player.subscribe()` or
`engine.subscribe()`.

| Enum | Scope |
|------|-------|
| `PlayerEvent` | Status, rate, volume, mute, current item changes |
| `ItemEvent` | Item status, buffering, seek, end-of-stream, stall |
| `EngineEvent` | Slot lifecycle, crossfade progress, master volume |
| `SessionEvent` | Interruption, route change, media services |
| `DjEvent` | BPM detection, beat tick, sync engage/disengage |

## Engine lifecycle

1. `Engine::start()` — activate audio output
2. `Engine::allocate_slot()` → `SlotId` — reserve arena position
3. Attach a `Player` to the slot (player holds `slot_id()`)
4. `Player::replace_current_item(Some(item))` — load media
5. `Player::play()` — engine begins pulling PCM from slot
6. For crossfade: allocate second slot, load second player, call `Engine::crossfade(from, to, config)`
7. `Engine::release_slot(id)` — return arena position
8. `Engine::stop()` — deactivate audio output

## Crossfade flow

```
Slot A: ████████████▓▓▓▓▓▓░░░░ (fade out)
Slot B:             ░░░░▓▓▓▓▓▓████████████ (fade in)
        ↑                      ↑
        crossfade start        crossfade end
```

`CrossfadeController::start(a, b, config)` initiates the transition.
When `beat_aligned: true`, the controller waits for the next beat boundary
before starting the fade.

## DJ mixing flow

1. Analyze both tracks: `BpmAnalyzer::analyze(slot_a)`, `BpmAnalyzer::analyze(slot_b)`
2. Sync tempos: `BpmSync::sync(follower: slot_b, leader: slot_a)`
3. Adjust phase: `BpmSync::set_phase_offset(slot_b, 0.0)` (align downbeats)
4. Shape sound: `Equalizer::set_gain(slot_b, EqBand::Low, -24.0)` (cut bass on incoming)
5. Crossfade: `CrossfadeController::start(slot_a, slot_b, config)`
6. During transition: gradually restore bass on incoming, cut bass on outgoing
7. Cleanup: `BpmSync::unsync(slot_b)`, release slot A

## Invariants

- `SlotId` is only valid between `allocate_slot()` and `release_slot()`
- At most `Engine::max_slots()` slots may be allocated simultaneously
- Only one crossfade may be active at a time
- `Player::slot_id()` returns `None` until registered with the engine
- All `#[non_exhaustive]` enums and structs require builder or constructor
- `MediaTime::INVALID` has `timescale == 0`; all arithmetic on invalid times returns invalid

## Error handling

`PlayError` covers all failure modes. No `unwrap()`/`expect()` in production code.
Implementations must propagate errors via `Result<T, PlayError>`.
