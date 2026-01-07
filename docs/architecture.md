# Kithara Architecture

This document describes the architecture of Kithara, a networking + decoding library for audio playback.

## Overview

Kithara is a modular Rust workspace that provides building blocks for audio streaming applications:
- HTTP networking with retry and timeout handling
- Persistent disk caching for offline playback
- HLS VOD orchestration with ABR (Adaptive Bitrate)
- Audio decoding via Symphonia
- Streaming orchestration with backpressure

## Core Principles

### 1. Resource-Based Model
Kithara operates on **logical resources** rather than raw byte streams:
- Each resource is addressable by a `ResourceKey` (`asset_root` + `rel_path`)
- Resources can be atomic (small files) or streaming (large files)
- The filesystem is the source of truth for resource existence

### 2. Persistent Storage First
All downloaded resources are persisted to disk:
- Enables offline playback
- Survives application restarts
- Tree-like layout for HLS resources

### 3. Modular Design
Components are loosely coupled and can be used independently:
- Networking layer doesn't know about storage
- Storage layer doesn't know about orchestration
- Decoding layer works with any `Read + Seek` source

### 4. Async/Sync Boundary
Clear separation between async networking and sync decoding:
- Async tasks handle network I/O and write to storage
- Sync threads read from storage and decode audio
- `kithara-io` bridge provides sync interface over async storage

## Crate Architecture

### Infrastructure Layer
- **`kithara-net`**: HTTP client with retry, timeout, and range request support
- **`kithara-storage`**: Storage primitives (`AtomicResource`, `StreamingResource`)
- **`kithara-assets`**: Persistent disk assets store with lease/pin semantics

### Orchestration Layer
- **`kithara-stream`**: Generic byte-stream orchestration with backpressure
- **`kithara-file`**: Progressive file download (MP3, AAC, etc.)
- **`kithara-hls`**: HLS VOD orchestration with ABR and caching

### Processing Layer
- **`kithara-decode`**: Audio decoding with Symphonia, generic over sample type

## Critical Contracts

### EOF Semantics
`Read::read()` returning `Ok(0)` means **true End-Of-Stream**. Never return `Ok(0)` to indicate "no data yet." Use `wait_range` to block until data is available.

### Backpressure
Memory must not grow without bounds. Use bounded buffers and block producers when consumers are slow.

### Crash Safety
Small file writes use atomic replace (temp file → rename). The filesystem is the source of truth; metadata is best-effort.

### Offline Support
All resources are cached persistently. Cache hits don't affect ABR throughput estimates.

## Resource Model

### Asset Identification
- `AssetId`: Derived from canonical URL without query/fragment
- `ResourceKey`: `{ asset_root: String, rel_path: String }`
- Disk mapping: `<cache_root>/<asset_root>/<rel_path>`

### Resource Types
1. **AtomicResource**: Small files (playlists, keys, metadata)
   - Whole-object read/write
   - Crash-safe via temp → rename
   - Used via `Assets::open_atomic_resource()`

2. **StreamingResource**: Large files (segments, progressive downloads)
   - Random-access `write_at`/`read_at`
   - `wait_range` for availability waiting
   - Used via `Assets::open_streaming_resource()`

## HLS-Specific Architecture

### Tree-Based Resources
HLS requires a tree-like resource layout:
```
<asset_root>/
├── master.m3u8
├── variant_0.m3u8
├── variant_1.m3u8
├── keys/
│   └── key.bin
└── segments/
    ├── 0001.m4s
    ├── 0002.m4s
    └── ...
```

### ABR Integration
- Throughput estimates based only on network downloads (not cache hits)
- Configurable hysteresis and safety factors
- Events for variant switches with clear semantics

### Key Management
- Keys processed through `key_processor_cb` before caching
- Processed keys stored for offline playback
- No logging of key material

## Error Handling Philosophy

### Error Categories
- **Recoverable**: Network timeouts, temporary failures
- **Fatal**: Invalid formats, unsupported codecs
- **Cancellation**: User-initiated stops

### Error Propagation
- Type-preserving error propagation (no `Box<dyn Error>` in public API)
- Context preservation (URL, range, operation type)
- Clear separation between layers

## Testing Strategy

### TDD-First Development
1. Write tests describing desired behavior
2. Verify tests fail for expected reasons
3. Implement minimal code to pass tests
4. Refactor after tests pass

### Test Characteristics
- Deterministic (no external network)
- Focused on contracts, not implementation details
- Layer-appropriate (unit vs integration)

### Test Layers
- **Storage**: Atomic replace, range waiting, EOF semantics
- **Assets**: Path mapping, lease/pin, eviction
- **IO**: Read/EOF semantics, seek safety
- **Orchestration**: Range requests, cancellation, offline behavior

## Dependency Management

### Workspace-First Policy
All dependency versions declared in root `Cargo.toml`:
```toml
[workspace.dependencies]
tokio = "1.49.0"
```

Crates reference dependencies without versions:
```toml
[dependencies]
tokio = { workspace = true }
```

### No Duplicate Dependencies
Check `[workspace.dependencies]` before adding new dependencies. Use existing solutions when possible.

## Development Rules

### Code Style
- Short, descriptive names
- `use` statements at file beginning (not inside functions)
- Minimal comments in code (documentation in README files)

### Public Contracts
Each crate's public API is explicitly defined in its `README.md`. Implementation details stay private.

### Component Boundaries
Changes typically affect only one crate. Cross-crate changes require explicit justification.

## Integration Patterns

### Typical Usage Flow
1. Create `AssetStore` with `asset_store(root_dir, config)`
2. Create source (`FileSource` or `HlsSource`) with URL and options
3. Open session to get control handle and byte stream
4. Process stream while handling events
5. Drop stream to stop session

### Decoder Integration
1. Get `Read + Seek` source from session (`session.source()`)
2. Wrap in `kithara-stream::io::Reader` if needed
3. Pass to `kithara-decode::Decoder` or `AudioStream`
4. Process PCM chunks

## Future Considerations

### Live Streaming
Current design focuses on VOD but doesn't preclude live streaming additions.

### DRM Integration
Key processing callback allows custom DRM integration.

### Metrics and Telemetry
Event system supports monitoring; detailed metrics can be added as needed.

### Format Extensions
Symphonia-based decoding supports many formats; new formats can be added via Symphonia updates.