# `kithara-hls` — HLS VOD orchestration with caching and ABR

`kithara-hls` provides HLS (HTTP Live Streaming) VOD (Video-on-Demand) orchestration for Kithara, with support for:
- Adaptive Bitrate (ABR) switching based on network conditions
- Persistent caching for offline playback
- Playlist parsing and management
- Segment fetching with retry logic
- Key management for encrypted streams
- Event-driven architecture for monitoring and control

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core components
- `struct HlsSource` — Main entry point for creating HLS sessions
- `struct HlsSession` — Active HLS session with stream and event access
- `trait HlsSourceContract` — Trait for HLS source implementations

### Configuration and options
- `struct HlsOptions` — Comprehensive configuration for HLS behavior
- `struct KeyContext` — Context for key processing operations

### Managers and controllers
- `struct PlaylistManager` — Manages playlist fetching, parsing, and caching
- `struct FetchManager` — Handles segment fetching with retry logic
- `struct KeyManager` — Manages encryption key fetching and processing
- `struct AbrController` — Implements ABR decision logic
- `struct EventEmitter` — Emits HLS events for monitoring

### Events and errors
- `enum HlsEvent` — Events emitted during HLS playback
- `enum HlsError` — Error type for HLS operations
- `type HlsResult<T> = Result<T, HlsError>` — Result alias

## Architecture overview

`kithara-hls` implements a modular architecture that coordinates multiple subsystems:

```
┌─────────────────────────────────────────────────────────┐
│                    HlsSession                           │
│      (Public API: stream(), events(), source())         │
└──────────────────────────┬──────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────┐
│                    HlsDriver                            │
│      (Orchestrates all managers and controllers)        │
└─────┬────────────┬────────────┬────────────┬────────────┘
      │            │            │            │
┌─────▼─────┐ ┌───▼────┐ ┌─────▼─────┐ ┌───▼──────┐
│ Playlist  │ │ Fetch  │ │   Key    │ │   ABR   │
│ Manager   │ │Manager │ │ Manager  │ │Controller│
└───────────┘ └────────┘ └──────────┘ └──────────┘
      │            │            │            │
┌─────▼─────┐ ┌───▼────┐ ┌─────▼─────┐ ┌───▼──────┐
│kithara-net│ │kithara-│ │kithara-net│ │Throughput│
│ (HTTP)    │ │ assets │ │ (HTTP)    │ │  Metrics │
└───────────┘ └────────┘ └──────────┘ └──────────┘
```

### Key responsibilities

1. **Playlist management**: Fetches, parses, and caches master/playlist files
2. **Segment fetching**: Downloads media segments with retry and timeout handling
3. **Key management**: Handles encryption key fetching and processing
4. **ABR control**: Monitors network conditions and switches bitrates
5. **Event emission**: Emits events for UI integration and monitoring
6. **Storage integration**: Caches all resources via `kithara-assets`

## Core invariants

1. **VOD focus**: Designed for Video-on-Demand, not live streaming
2. **Offline support**: All resources are cached for offline playback
3. **ABR awareness**: Adapts bitrate based on network conditions
4. **Event-driven**: Rich event system for monitoring and control
5. **Storage persistence**: Uses `kithara-assets` for persistent caching
6. **Error resilience**: Robust error handling with retry logic
7. **Cancellation**: All async operations respect cancellation

## Configuration

### `HlsOptions` key fields

- `base_url: Option<Url>` — Base URL for resolving relative URLs
- `variant_stream_selector: Option<...>` — Custom variant selection logic
- `abr_*` fields — ABR configuration parameters
- `request_timeout: Duration` — Network request timeout
- `max_retries: u32` — Maximum retry attempts
- `prefetch_buffer_size: usize` — Number of segments to prefetch
- `key_processor_cb: Option<...>` — Custom key processing callback
- `key_query_params: Option<HashMap>` — Query parameters for key requests
- `key_request_headers: Option<HashMap>` — Headers for key requests

### ABR Configuration

- `abr_min_buffer_for_up_switch: f32` — Minimum buffer (seconds) for upgrading
- `abr_down_switch_buffer: f32` — Buffer threshold (seconds) for downgrading
- `abr_throughput_safety_factor: f32` — Safety factor for throughput estimates
- `abr_up_hysteresis_ratio: f32` — Hysteresis ratio for upgrading
- `abr_down_hysteresis_ratio: f32` — Hysteresis ratio for downgrading
- `abr_min_switch_interval: Duration` — Minimum time between bitrate switches

## Events

### `HlsEvent` variants

- `VariantSwitched { from: usize, to: usize, reason: AbrReason }` — Bitrate switch occurred
- `SegmentFetched { variant_idx: usize, segment_idx: usize, bytes: u64 }` — Segment downloaded
- `SegmentPlayed { variant_idx: usize, segment_idx: usize }` — Segment played
- `BufferLevel { seconds: f32 }` — Current buffer level
- `ThroughputSample { bytes_per_second: f64 }` — Network throughput sample
- `PlaylistRefreshed` — Playlist was refreshed
- `Error(HlsError)` — Error occurred

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
- Can be integrated as an `EngineSource` for byte stream orchestration
- Uses similar event and error patterns

### `kithara-net`
- Uses `Net` trait for all HTTP operations
- Leverages retry and timeout policies
- Uses range requests for efficient downloading

### `kithara-assets`
- Stores playlists, segments, and keys as assets
- Uses both `AtomicResource` and `StreamingResource` appropriately
- Manages asset lifecycle and cleanup

### `kithara-file`
- Complementary crate for single-file progressive downloads
- Shares similar architecture patterns

## Example usage

### Basic HLS playback
```rust
use kithara_hls::{HlsSource, HlsOptions};
use kithara_assets::{asset_store, EvictConfig};
use url::Url;

// Create asset store
let assets = asset_store("/cache/dir", EvictConfig::default());

// Configure HLS options
let opts = HlsOptions {
    base_url: Some(Url::parse("https://example.com/").unwrap()),
    ..Default::default()
};

// Create HLS session
let url = Url::parse("https://example.com/master.m3u8").unwrap();
let session = HlsSource::open(url, opts, assets).await?;

// Get event receiver
let mut events = session.events();

// Process the stream
let mut stream = session.stream();
while let Some(result) = stream.next().await {
    match result {
        Ok(bytes) => {
            // Process segment bytes
        }
        Err(e) => {
            eprintln!("Stream error: {:?}", e);
            break;
        }
    }

    // Check for events
    while let Ok(event) = events.try_recv() {
        match event {
            HlsEvent::VariantSwitched { from, to, reason } => {
                println!("Bitrate switched from {} to {}: {:?}", from, to, reason);
            }
            HlsEvent::SegmentFetched { variant_idx, segment_idx, bytes } => {
                println!("Segment {}-{} fetched: {} bytes", variant_idx, segment_idx, bytes);
            }
            HlsEvent::BufferLevel { seconds } => {
                println!("Buffer level: {:.1}s", seconds);
            }
            _ => {}
        }
    }
}
```

### Custom key processing
```rust
use kithara_hls::{HlsOptions, KeyContext};
use bytes::Bytes;

let opts = HlsOptions {
    key_processor_cb: Some(Arc::new(|bytes: Bytes, ctx: KeyContext| {
        // Custom key processing logic
        println!("Processing key from: {}", ctx.url);
        
        // Example: decrypt or transform the key
        let processed = bytes; // In reality, you'd decrypt here
        
        Ok(processed)
    })),
    ..Default::default()
};
```

### Offline playback
```rust
use kithara_hls::{HlsSource, HlsOptions};

// Assuming assets were previously cached
let opts = HlsOptions::default();
let url = Url::parse("https://example.com/master.m3u8").unwrap();

// The session will use cached resources
let session = HlsSource::open(url, opts, assets).await?;

// If resources are missing, operations will fail with HlsError::OfflineMiss
```

### ABR configuration
```rust
use kithara_hls::HlsOptions;
use std::time::Duration;

let opts = HlsOptions {
    abr_min_buffer_for_up_switch: 15.0, // Need 15s buffer to upgrade
    abr_down_switch_buffer: 8.0,        // Downgrade if buffer < 8s
    abr_throughput_safety_factor: 1.8,  // Conservative throughput estimate
    abr_up_hysteresis_ratio: 1.4,       // Need 40% better throughput to upgrade
    abr_down_hysteresis_ratio: 0.7,     // Downgrade at 70% of required throughput
    abr_min_switch_interval: Duration::from_secs(20), // Min 20s between switches
    ..Default::default()
};
```

## Design philosophy

1. **Modular architecture**: Separated concerns (playlist, fetch, keys, ABR)
2. **VOD first**: Optimized for Video-on-Demand with full caching
3. **ABR intelligence**: Sophisticated bitrate adaptation based on multiple factors
4. **Offline capability**: Full support for offline playback via persistent cache
5. **Event-driven**: Comprehensive event system for monitoring and UI integration
6. **Error resilience**: Robust error handling with clear error categories
7. **Storage integration**: Deep integration with `kithara-assets` for caching
8. **Configurable**: Extensive configuration options for different use cases