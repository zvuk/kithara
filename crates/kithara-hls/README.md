# `kithara-hls` — HLS VOD orchestration with caching and ABR

`kithara-hls` provides HLS (HTTP Live Streaming) VOD (Video-on-Demand) orchestration for Kithara, with support for:
- Adaptive Bitrate (ABR) switching based on network conditions
- Persistent caching for offline playback
- Playlist parsing and management
- Segment fetching with retry logic
- Key management for encrypted streams
- Event-driven architecture for monitoring and control

## Public contract (normative)

### Entry point
- `struct Hls` — Type parameter for `StreamSource::<Hls>::open(url, params)`
- `struct HlsSource` — Random-access source adapter (implements `kithara_stream::Source`)
- `StreamSource::<Hls>` — Unified source interface with event subscription

### Configuration
- `struct HlsOptions` — Comprehensive configuration for HLS behavior
- `struct AbrOptions` — ABR-specific configuration
- `struct NetworkOptions` — Network request configuration
- `struct CacheOptions` — Cache configuration
- `struct KeyOptions` — Key processing configuration

### Events and errors
- `enum HlsEvent` — Events emitted during HLS playback
- `enum HlsError` — Error type for HLS operations
- `type HlsResult<T> = Result<T, HlsError>` — Result alias

### ABR types
- `struct AbrDecision` — ABR decision result
- `enum AbrReason` — Reason for ABR decision
- `struct ThroughputSample` — Network throughput measurement
- `struct Variant` — Variant (quality level) information

## Architecture overview

```
┌──────────────────────────────────────────────────────────┐
│           StreamSource::<Hls>::open()                    │
│              (Creates HlsSource)                         │
└──────────────────────────┬───────────────────────────────┘
                           │
┌──────────────────────────▼───────────────────────────────┐
│                   StreamSource<Hls>                      │
│              (Wraps HlsSource)                           │
│              .events() → Receiver<HlsEvent>              │
└──────────────────────────┬───────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────┐
│                   SegmentStream                          │
│   (Iterates variants and segments, handles ABR)          │
└─────┬────────────┬────────────┬────────────┬────────────┘
      │            │            │            │
┌─────▼─────┐ ┌───▼────┐ ┌─────▼─────┐ ┌───▼──────┐
│ Playlist  │ │ Fetch  │ │   Key    │ │   ABR    │
│ Manager   │ │Manager │ │ Manager  │ │Controller│
└───────────┘ └────────┘ └──────────┘ └──────────┘
      │            │
┌─────▼─────┐ ┌───▼────┐
│kithara-net│ │kithara-│
│ (HTTP)    │ │ assets │
└───────────┘ └────────┘
```

### Module structure

```
src/
  lib.rs              — Public API re-exports
  error.rs            — HlsError, HlsResult
  events.rs           — HlsEvent, EventEmitter
  options.rs          — HlsOptions, AbrOptions, etc.
  source.rs           — Hls factory (SourceFactory impl)
  session.rs          — HlsSession
  source_adapter.rs   — HlsSource (implements Source trait)
  abr/                — ABR logic
    controller.rs     — AbrController
    estimator.rs      — ThroughputEstimator
    types.rs          — Variant, ThroughputSample
  stream/             — Segment streaming pipeline
    segment_stream.rs — SegmentStream
    pipeline.rs       — Main async generator
    commands.rs       — Seek/ForceVariant commands
    context.rs        — Helper functions
    types.rs          — PipelineEvent, SegmentMeta
  fetch.rs            — FetchManager (internal)
  keys.rs             — KeyManager (internal)
  playlist.rs         — PlaylistManager (internal)
```

## Core invariants

1. **VOD focus**: Designed for Video-on-Demand, not live streaming
2. **Offline support**: All resources are cached for offline playback
3. **ABR awareness**: Adapts bitrate based on network conditions
4. **Event-driven**: Rich event system for monitoring and control
5. **Storage persistence**: Uses `kithara-assets` for persistent caching
6. **Error resilience**: Robust error handling with retry logic
7. **Cancellation**: All async operations respect cancellation

## Example usage

### Basic HLS playback
```rust
use kithara_hls::{Hls, HlsParams};
use kithara_stream::StreamSource;
use url::Url;

let url = Url::parse("https://example.com/master.m3u8").unwrap();
let params = HlsParams::default();

// Open HLS source
let source = StreamSource::<Hls>::open(url, params).await?;

// Subscribe to events
let mut events = source.events();

// Use source with kithara_stream::SyncReader for decoding
```

### Custom ABR configuration
```rust
use kithara_hls::{HlsOptions, AbrOptions};
use std::time::Duration;

let opts = HlsOptions {
    abr: AbrOptions {
        min_buffer_for_up_switch_secs: 15.0,
        down_switch_buffer_secs: 8.0,
        throughput_safety_factor: 1.8,
        up_hysteresis_ratio: 1.4,
        down_hysteresis_ratio: 0.7,
        min_switch_interval: Duration::from_secs(20),
        ..Default::default()
    },
    ..Default::default()
};
```

## Events

### `HlsEvent` variants

- `VariantApplied { from_variant, to_variant, reason }` — Bitrate switch occurred
- `SegmentStart { variant, segment_index, byte_offset }` — Segment download started
- `SegmentComplete { variant, segment_index, bytes_transferred, duration }` — Segment downloaded
- `KeyFetch { key_url, success, cached }` — Key fetch completed
- `BufferLevel { level_seconds }` — Current buffer level
- `ThroughputSample { bytes_per_second }` — Network throughput sample
- `DownloadProgress { offset, percent }` — Download progress
- `PlaybackProgress { position, percent }` — Playback progress
- `Error { error, recoverable }` — Error occurred
- `EndOfStream` — Stream ended

Events are emitted through a broadcast channel accessible via `HlsSession::events()`.

## Error handling

### `HlsError` categories

- `Net(NetError)` — Network-related errors
- `Assets(AssetsError)` — Asset storage errors
- `Storage(StorageError)` — Storage errors
- `PlaylistParse(String)` — Playlist parsing errors
- `VariantNotFound(String)` — Requested variant not found
- `SegmentNotFound(String)` — Requested segment not found
- `NoSuitableVariant` — No suitable variant available
- `KeyProcessing(String)` — Key processing errors
- `Abr(String)` — ABR-related errors
- `OfflineMiss` — Resource not available in offline mode
- `InvalidUrl(String)` — Invalid URL
- `Driver(String)` — Driver errors

## Integration with other Kithara crates

### `kithara-stream`
- `HlsSource` implements the `Source` trait
- Can be wrapped with `SyncReader` for synchronous decoding

### `kithara-net`
- Uses `HttpClient` for all HTTP operations
- Leverages retry and timeout policies

### `kithara-assets`
- Stores playlists, segments, and keys
- Uses both `AtomicResource` (playlists, keys) and `StreamingResource` (segments)
- Manages asset lifecycle with lease/eviction semantics

## Design philosophy

1. **Modular architecture**: Separated concerns (playlist, fetch, keys, ABR)
2. **Hidden internals**: Only public API is exposed, internals are private
3. **VOD first**: Optimized for Video-on-Demand with full caching
4. **ABR intelligence**: Sophisticated bitrate adaptation
5. **Offline capability**: Full support for offline playback
6. **Event-driven**: Comprehensive event system for monitoring
7. **Storage integration**: Deep integration with `kithara-assets`
