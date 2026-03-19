# Playback State Inventory

Date: 2026-03-13

## Scope

This document inventories runtime state-bearing abstractions across the Kithara workspace and
maps who owns which data.

It is intentionally biased toward the playback/data path:

- `kithara-stream`
- `kithara-audio`
- `kithara-decode`
- `kithara-file`
- `kithara-hls`
- `kithara-assets`
- `kithara-storage`
- `kithara-abr`
- `kithara-drm`
- `kithara-events`

Support crates are still listed, but only to the level needed to understand boundaries.

Excluded from field-level inventory:

- mocks
- `#[cfg(test)]` helpers
- pure config DTOs unless they directly shape ownership boundaries
- marker types
- error enums
- proc-macro implementation details

## State Taxonomy

Every long-lived field in the runtime falls into one of these buckets:

1. Topology
   Immutable or slowly mutating media structure. Example: variants, segments, durations, offsets.
2. Committed layout
   Facts about what is already mapped into the virtual byte space.
3. Resource residency
   Facts about the physical cached resource and its write/read lifecycle.
4. Reader timeline
   Current byte cursor, committed playback time, seek handshake.
5. Planner state
   Transient downloader scheduling state. This is not topology and not committed layout.
6. Decoder session
   Live decoder object and the exact context needed to drive it.
7. Observability
   Event buses and emitted payloads.
8. Control/UI state
   Higher-level player or frontend orchestration above the media pipeline.

The main architectural rule implied by this inventory is:

- one owner per state bucket
- other layers may observe or derive from it, but must not duplicate it as mutable state

## Single-Writer Expectations

The core single-writer map should be:

| Data | Canonical owner |
| --- | --- |
| Reader byte cursor | `kithara-stream::Stream<T>` via `Read` and `Seek` |
| Playback committed position | audio consumer path |
| Seek generation / flush handshake | `kithara-stream::Timeline` |
| HLS topology | `kithara-hls::PlaylistState` |
| HLS committed byte layout | `kithara-hls::DownloadState` |
| Cached resource lifecycle | `kithara-assets` + `kithara-storage::ResourceExt` |
| Track decode workflow | `kithara-audio::TrackState` |
| ABR current variant policy | `kithara-abr::AbrController` |
| Observability fanout | `kithara-events::EventBus` |

Anything outside this map that stores the same fact again is a duplication candidate.

## Workspace Inventory

### `crates/kithara`

Role:
- facade crate only

State-bearing abstractions:
- none

Notes:
- re-exports subcrates
- does not own runtime playback state

### `crates/kithara-stream`

Role:
- generic sync reader facade
- generic downloader orchestration
- shared stream protocol

State-bearing abstractions:

- `Timeline`
  - `byte_position`: current reader byte offset
  - `committed_position_ns`: playback time already committed by the consumer
  - `download_position`: downloaded watermark
  - `eof`: EOF flag
  - `pending_seek_epoch`: seek waiting to be observed/applied
  - `total_bytes`: known total length
  - `total_duration_ns`: known total duration
  - `seek_epoch`: monotonic seek generation
  - `flushing`: I/O gate during seek
  - `seek_target_ns`: requested seek target
  - `seek_pending`: decoder still owes repositioning
  - Bucket: reader timeline

- `Stream<T>`
  - `source`: concrete `Source`
  - `timeline`: shared `Timeline`
  - Bucket: reader facade, owns reader cursor mutation

- `Backend`
  - `cancel`: child cancellation token
  - `worker`: downloader task handle
  - Bucket: downloader task lifetime

- `DownloadCursor<I>`
  - `Stream { from }`: sequential mode start
  - `Fill { floor, next }`: gap-fill state
  - `Complete`: no more planned work
  - Bucket: planner state

- `Writer`
  - `inner`: byte-stream to resource copy driver
  - Bucket: resource write session

- `EpochValidator`
  - `epoch`: current valid fetch epoch
  - Bucket: stale work rejection

- `NullStreamContext`
  - `timeline`: byte-offset source for non-segmented streams
  - Bucket: decoder read-only observation

Protocol types:

- `SourcePhase`
- `ReadOutcome`
- `SourceSeekAnchor`
- `Downloader` / `DownloaderIo`
- `StreamContext`

Important boundary:

- `Stream<T>` is the only generic layer that should move `Timeline.byte_position`.

### `crates/kithara-audio`

Role:
- worker-side decode FSM
- consumer-side PCM reader
- effect and resampler pipeline

State-bearing abstractions:

- `TrackState`
  - `Decoding`
  - `SeekRequested(SeekRequest)`
  - `WaitingForSource { context, reason }`
  - `ApplyingSeek(ApplySeekState)`
  - `RecreatingDecoder(RecreateState)`
  - `AwaitingResume(ResumeState)`
  - `AtEof`
  - `Failed(TrackFailure)`
  - Bucket: track decode workflow

- `SeekContext`
  - `epoch`, `target`

- `SeekRequest`
  - `attempt`, `seek`

- `ApplySeekState`
  - `mode`, `request`

- `ResumeState`
  - `recover_attempts`, `seek`, `skip`

- `RecreateState`
  - `attempt`, `cause`, `media_info`, `next`, `offset`

- `DecoderSession`
  - `base_offset`
  - `decoder`
  - `media_info`
  - Bucket: decoder session

- `ConsumerPhase`
  - `Buffering`
  - `Playing`
  - `SeekPending { epoch }`
  - `AtEof`
  - `Failed`
  - Bucket: consumer workflow

- `SharedStream<T>`
  - `inner: Arc<Mutex<Stream<T>>>`
  - Bucket: shared reader access

- `OffsetReader<T>`
  - `shared`
  - `base_offset`
  - Bucket: decoder-facing seek translation

- `StreamAudioSource<T>`
  - `shared_stream`
  - `session`
  - `state`
  - `decoder_factory`
  - `epoch`
  - `chunks_decoded`
  - `total_samples`
  - `last_spec`
  - `emit`
  - `effects`
  - `timeline`
  - Bucket: worker-local decode engine

- `Audio<S>`
  - `cmd_tx`
  - `pcm_rx`
  - `_epoch`
  - `validator`
  - `spec`
  - `current_chunk`
  - `chunk_offset`
  - `consumer_phase`
  - `timeline`
  - `metadata`
  - `audio_events_tx`
  - `bus`
  - `cancel`
  - `pcm_pool`
  - `host_sample_rate`
  - `playback_rate`
  - `preload_notify`
  - `preloaded`
  - `notify_waiting`
  - `track_id`
  - `worker`
  - `reader_wake`
  - `is_standalone_worker`
  - `_marker`
  - Bucket: consumer-side playback/session state

- `AudioWorkerHandle`
  - `cmd_tx`
  - `wake`
  - `id_gen`
  - `cancel`
  - Bucket: shared worker control plane

- `TrackSlot`
  - `consumer_wake`
  - `track_id`
  - `source`
  - `data_tx`
  - `cmd_rx`
  - `pending_fetch`
  - `service_class`
  - `preload_notify`
  - `preload_chunks`
  - `chunks_sent`
  - `preloaded`
  - `cancel`
  - `terminal`
  - `eof_sent`
  - Bucket: per-track worker slot

- `WorkerWake`
  - `woken`
  - `condvar`

- `ThreadWake`
  - `waiter`

- `TrackIdGen`
  - atomic counter

- `EqEffect`
  - `bands`
  - `states`
  - `filters_l`
  - `filters_r`
  - `sample_rate`
  - `channels`
  - `smooth_coeff`

- `ResamplerProcessor`
  - `channels`
  - `chunk_size`
  - `current_playback_rate`
  - `current_ratio`
  - `host_sample_rate`
  - `playback_rate`
  - `source_rate`
  - `quality`
  - `resampler`
  - `output_spec`
  - `pool`
  - `input_buffer`
  - scratch buffers

### `crates/kithara-decode`

Role:
- concrete decoder backends
- runtime decoder factory

State-bearing abstractions:

- `SymphoniaInner`
  - `format_reader`
  - `decoder`
  - `track_id`
  - `spec`
  - `position`
  - `frame_offset`
  - `duration`
  - `metadata`
  - `byte_len_handle`
  - `stream_ctx`
  - `epoch`
  - `pool`
  - Bucket: live software decoder state

- `Symphonia<C>`
  - `inner`
  - `_codec`

- `ReadSeekAdapter<R>`
  - `inner`
  - `byte_len`
  - `seek_enabled`
  - Bucket: decoder input adapter

- `AppleInner`
  - AudioToolbox handles and callback bridges
  - source progress fields
  - output timing fields
  - metadata and pool
  - Bucket: live hardware decoder state

- `Apple<C>`
  - `inner`
  - `_codec`

- `AndroidInner`
  - `spec`
  - `metadata`
  - `byte_len_handle`

- `Android<C>`
  - `inner`
  - `_codec`

Protocol/boundary types:

- `DecoderFactory`
- `InnerDecoder`
- `AudioDecoder`

### `crates/kithara-file`

Role:
- progressive file stream implementation
- partial-cache downloader and gap filler

State-bearing abstractions:

- `FileStreamState`
  - `url`
  - `cancel`
  - `res`
  - `bus`
  - `headers`
  - `timeline`
  - Bucket: immutable per-stream session context

- `Progress`
  - `read_pos`
  - `timeline`
  - `reader_advanced`
  - Bucket: file reader/download progress

- `SharedFileState`
  - `range_requests`
  - `reader_needs_data`
  - Bucket: source-to-downloader demand channel

- `FileSource`
  - `res`
  - `progress`
  - `bus`
  - `shared`
  - `_backend`
  - Bucket: sync source facade

- `FileDownloader`
  - `io`
  - `writer`
  - `res`
  - `progress`
  - `bus`
  - `total`
  - `look_ahead_bytes`
  - `shared`
  - `cursor`
  - Bucket: planner and commit path

- `FileIo`
  - `net_client`
  - `url`
  - `res`
  - `cancel`
  - `headers`
  - Bucket: pure I/O executor

Important note:

- `Progress.read_pos` is not the same fact as `Timeline.byte_position`.
  It is downloader demand state, not the canonical reader cursor.

### `crates/kithara-hls`

Role:
- segmented source implementation
- HLS playlist topology
- ABR-aware segment downloader
- segment fetch/cache bridge

State-bearing abstractions:

- `PlaylistState`
  - `variants: Vec<RwLock<VariantState>>`
  - Bucket: topology

- `VariantState`
  - `id`
  - `uri`
  - `bandwidth`
  - `codec`
  - `container`
  - `init_url`
  - `segments`
  - `size_map`
  - Bucket: per-variant topology

- `SegmentState`
  - `index`
  - `url`
  - `duration`
  - `key`

- `VariantSizeMap`
  - `init_size`
  - `segment_sizes`
  - `offsets`
  - `total`

- `DownloadState`
  - `entries`
  - `loaded_keys`
  - `loaded_ranges`
  - `last_offset`
  - Bucket: committed layout

- `LoadedSegment`
  - `variant`
  - `segment_index`
  - `byte_offset`
  - `init_len`
  - `media_len`
  - `init_url`
  - `media_url`

- `FetchManager<N>`
  - `backend`
  - `key_manager`
  - `net`
  - `cancel`
  - `headers`
  - `master_url`
  - `base_url`
  - `master`
  - `media`
  - `num_variants_cache`
  - `init_segments`
  - `media_segments`
  - `playlist_state`
  - Bucket: fetch/cache/playlist caches

- `KeyManager`
  - `fetch`
  - `key_processor`
  - `key_query_params`
  - `key_request_headers`
  - Bucket: DRM key runtime state

- `SharedSegments`
  - `segments`
  - `condvar`
  - `timeline`
  - `reader_advanced`
  - `segment_requests`
  - `pending_segment_request`
  - `playlist_state`
  - `had_midstream_switch`
  - `cancel`
  - `stopped`
  - `current_segment_index`
  - `abr_variant_index`
  - Bucket: mixed shared bridge between source and downloader

- `SegmentRequest`
  - `segment_index`
  - `variant`
  - `seek_epoch`

- `HlsSource`
  - `fetch`
  - `shared`
  - `playlist_state`
  - `bus`
  - `variant_fence`
  - `_backend`
  - Bucket: sync source facade

- `HlsDownloader`
  - `active_seek_epoch`
  - `io`
  - `fetch`
  - `playlist_state`
  - `cursor`
  - `last_committed_variant`
  - `force_init_for_seek`
  - `sent_init_for_variant`
  - `abr`
  - `shared`
  - `bus`
  - `look_ahead_bytes`
  - `look_ahead_segments`
  - `prefetch_count`
  - Bucket: planner + commit + ABR control

- `HlsIo`
  - `fetch`
  - Bucket: pure I/O executor

- `HlsStreamContext`
  - `timeline`
  - `segment_index`
  - `variant_index`
  - Bucket: decoder read-only observation

Important note:

- `PlaylistState` is topology.
- `DownloadState` is committed layout.
- `FetchManager` is fetch/cache orchestration.
- `HlsDownloader.cursor` is planner state.
- `SharedSegments` currently mixes multiple buckets in one object.

### `crates/kithara-assets`

Role:
- unified resource store facade
- resource state query
- cache, lease, processing, eviction composition

State-bearing abstractions:

- `AssetStore<Ctx>`
  - `Disk(...)` or `Mem(...)`
  - Bucket: unified store facade

- `AssetResourceState`
  - `Missing`
  - `Active`
  - `Committed { final_len }`
  - `Failed`
  - Bucket: resource residency

- `CachedAssets<A>`
  - cache of opened resources

- `LeaseAssets<A>`
  - pin/unpin lifetime wrapper around opened resources

- `LeaseResource<R, L>`
  - resource plus lease guard

- `LeaseGuard`
  - pin lifetime holder

- `ProcessingAssets<A, Ctx>`
  - commit-time processing wrapper

- `EvictAssets<A>`
  - eviction layer

- `MemAssetStore`
- `DiskAssetStore`
- `AssetStoreBuilder`
- `StoreOptions`
- `PinsIndex`
- `LruIndex`
- `LruState`
- `LruEntry`
- `ResourceKey`

Important boundary:

- resource lifecycle is owned here, not by HLS or File
- `resource_state()` is read-only inspection, not a synchronization primitive

### `crates/kithara-storage`

Role:
- low-level resource and driver lifecycle

State-bearing abstractions:

- `ResourceStatus`
  - `Active`
  - `Committed { final_len }`
  - `Failed(String)`
  - Bucket: physical resource lifecycle

- `Resource<D>`
  - concrete resource over a driver

- `StorageResource`
  - unified concrete resource facade

- `DriverState`
  - low-level driver state

- `Atomic<R>`
  - atomic whole-resource adapter

- `MmapDriver`
- `MemDriver`
- driver-specific state machines

Protocol/boundary types:

- `ResourceExt`
- `OpenMode`
- `WaitOutcome`

### `crates/kithara-abr`

Role:
- ABR policy and throughput estimation

State-bearing abstractions:

- `AbrController<E>`
  - `cfg`
  - `current_variant`
  - `estimator`
  - `last_switch_at_nanos`
  - `reference_instant`
  - Bucket: ABR control state

- `ThroughputEstimator`
  - EWMA state for measured throughput

- `AbrDecision`
- `AbrReason`
- `AbrOptions`
- `Variant`
- `VariantInfo`
- `ThroughputSample`

### `crates/kithara-drm`

Role:
- decrypt context and key material plumbing

State-bearing abstractions:

- `DecryptContext`
  - `key`
  - `iv`

### `crates/kithara-events`

Role:
- shared observability contract

State-bearing abstractions:

- `EventBus`
  - `tx`
  - Bucket: event fanout

- `AudioEvent`
  - format changes
  - playback progress
  - seek lifecycle payload
  - EOF

- `HlsEvent`
  - variant changes
  - segment start/complete
  - throughput
  - download progress
  - stale work drops
  - seek diagnostics
  - EOF and errors

- `FileEvent`
  - download progress
  - byte progress
  - EOF and errors

### `crates/kithara-net`

Role:
- HTTP/network abstraction

Primary abstractions:

- `HttpClient`
- `ByteStream`
- `Net` / `NetExt`
- `RetryNet<N, P>`
- `TimeoutNet<N>`
- `DefaultRetryPolicy`
- `Headers`
- `RangeSpec`
- `RetryPolicy`
- `NetOptions`

Notes:

- support layer only
- does not own playback timeline, layout, or decoder state

### `crates/kithara-bufpool`

Role:
- reusable pooled buffers

Primary abstractions:

- `Pool`
- `Pooled`
- `PooledOwned`
- `SharedPool`
- `BytePool`
- `PcmPool`
- `PoolStats`

Notes:

- memory reuse support layer
- not a protocol owner

### `crates/kithara-platform`

Role:
- platform portability layer

Primary abstractions:

- `Mutex`
- `RwLock`
- `Condvar`
- `Sender` / `Receiver`
- `MaybeSend` / `MaybeSync`
- thread and tokio wrappers

Notes:

- synchronization substrate only

### `crates/kithara-play`

Role:
- player-level control plane above the raw stream/decode path

Primary abstractions:

- `EngineImpl`
- `PlayerImpl`
- `PlayerTrack`
- `SharedPlayerState`
- `Resource`
- session and mixer traits

Notes:

- owns player/session workflow
- not the owner of source layout, resource lifecycle, or decoder internals

### `crates/kithara-app`

Role:
- CLI/TUI/GUI frontend layer

Primary abstractions:

- `AppController`
- `Playlist`
- `UiSession`
- `Dashboard`
- `GuiFrontend`
- `TuiFrontend`

Notes:

- control/UI state only

### `crates/kithara-ffi`

Role:
- FFI adapter over `kithara-play`

Primary abstractions:

- `AudioPlayer`
- `AudioPlayerItem`
- `EventBridge`
- `ItemEventBridge`
- observer traits
- shared `FFI_RUNTIME`

Notes:

- cross-language control layer

### `crates/kithara-wasm`

Role:
- wasm worker/player bridge

Primary abstractions:

- worker command types
- wasm player wrapper

Notes:

- platform adapter, not core media ownership

### `crates/kithara-hang-detector`

Role:
- instrumentation only

Primary abstractions:

- `HangDetector`
- `hang_watchdog` macro re-export

### `crates/kithara-hang-detector-macros`

Role:
- proc-macro instrumentation only

### `crates/kithara-test-utils`

Role:
- test fixtures only

Primary abstractions:

- asset/http fixture servers
- memory sources
- fixture protocol DTOs

### `crates/kithara-test-macros`

Role:
- test proc-macros only

### `crates/kithara-wasm-macros`

Role:
- wasm proc-macros only

### `crates/kithara-workspace-hack`

Role:
- cargo workspace maintenance only

### `tests`, `tests/fuzz`, `examples`, `xtask`

Role:
- integration tests, fuzzing, examples, tooling

## Cross-Crate Interaction Map

The playback/data path is:

1. `kithara-play` or `kithara-audio::Audio<S>` requests PCM.
2. `kithara-audio::StreamAudioSource<T>` drives a decoder session and consults `TrackState`.
3. The decoder reads from `SharedStream<T>`, which wraps `kithara-stream::Stream<T>`.
4. `Stream<T>` talks to a concrete `Source`:
   - `kithara-file::FileSource`
   - `kithara-hls::HlsSource`
5. The concrete source consults:
   - `Timeline` for reader/seek state
   - source-local shared state (`Progress` or `SharedSegments`)
   - resource/cache state via `kithara-assets` / `kithara-storage`
6. Background data movement is driven by `kithara-stream::Backend` and a concrete `Downloader`:
   - `FileDownloader`
   - `HlsDownloader`
7. `Downloader` uses a pure I/O helper:
   - `FileIo`
   - `HlsIo`
8. HLS download planning also consults:
   - `PlaylistState` for topology
   - `DownloadState` for committed layout
   - `AbrController` for variant policy
9. Physical resource reads/writes flow through:
   - `AssetStore`
   - `ResourceExt`
10. All layers publish diagnostics via `EventBus`.

## Current Ownership Violations and Blind Spots

These are the highest-signal duplication points surfaced by the inventory:

1. `kithara-hls::SharedSegments` mixes too many buckets.
   It currently holds committed layout, planner demand queue, timeline reference, cancellation,
   stop flag, switch marker, and duplicated current position hints in one shared object.

2. `SharedSegments.current_segment_index` duplicates planner/reader state.
   It is not topology, not committed layout, and not canonical reader cursor. It exists only as
   an extra mutable copy.

3. `SharedSegments.pending_segment_request` duplicates queue state.
   The system already has `segment_requests`; the extra field is a shadow lifecycle.

4. `HlsDownloader.cursor` is valid only as planner state.
   If source code, stream context, or shared state start treating it as committed truth, the
   model breaks.

5. `HlsStreamContext.segment_index` is currently backed by a dedicated atomic, not derived from a
   canonical owner.

6. `Timeline.download_position` and `Timeline.eof` are source/downloader facts stored in the
   generic stream timeline.
   This is a likely layering smell even if it is not the immediate failure root cause.

7. `FetchManager.media_segments` is HLS-local in-flight deduplication over resource acquisition.
   That is a duplication risk if resource write admission is supposed to be owned by assets/storage.

## Refactor Constraints Implied by This Inventory

Any future refactor should preserve these constraints:

1. Do not add a new shared state object unless it owns a new state bucket.
2. Do not rename a duplicate field into a "hint". Remove it or derive it.
3. Reader cursor changes must remain inside `Stream<T>::read` and `Stream<T>::seek`.
4. HLS topology must stay in `PlaylistState`.
5. HLS committed layout must stay in `DownloadState`.
6. Resource lifecycle must stay in assets/storage.
7. Audio worker workflow must stay in `TrackState`.
8. Planner transient state must stay local to the downloader implementation unless another layer
   truly needs it and cannot derive it.

## Immediate Next Use Of This Document

This inventory is meant to answer three questions before any further bug fix:

1. What exact fact is duplicated?
2. Which crate should own it?
3. Can observers derive it instead of storing it?

If the answer to question 3 is yes, the duplicate field should be removed rather than moved.
