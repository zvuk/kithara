<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![docs.rs](https://docs.rs/kithara-assets/badge.svg)](https://docs.rs/kithara-assets)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-assets

Assets store (disk or in-memory) with lease/pin semantics and LRU eviction. An *asset* is a logical unit addressed by `asset_root` containing multiple *resources* addressed by `rel_path`. A single `AssetStore` services every asset under its `root_dir`: `asset_root` is a per-request argument, so one shared store can back many URLs (e.g. app-wide playback + waveform). All resources use the unified `StorageResource` from `kithara-storage`. `AssetStore` is a unified wrapper with two backends: file-backed mmap (`Disk`) and ephemeral in-memory storage (`Mem`).

## Usage

```rust
use kithara_assets::AssetStoreBuilder;

let store = AssetStoreBuilder::new()
    .root_dir(cache_dir)
    .cancel(cancel.clone())
    .build();
// Bind a scope to one `asset_root`; it mints self-identifying keys.
let scope = store.scope("asset123");
let key = scope.key("segments/001.m4s");
// Per-resource ops live on the store; request identity is passed per call.
let resource = scope.store().acquire_resource(&key, None)?;
```

## Public contract

The explicit public contract is the unified `AssetStore` type. Everything else should be considered an implementation detail (even if currently `pub`), and constructors must propagate the shared cancellation token (use `AssetStoreBuilder`).

## Key mapping (normative)

Resources are addressed by strings chosen by higher layers:

- `asset_root`: e.g. `hex(hash(canonical_url))`
- `rel_path`: e.g. `media/audio.mp3`, `segments/0001.m4s`

Disk mapping is `<cache_root>/<asset_root>/<rel_path>`. Assets does not "invent" paths; it only enforces safety (no absolute paths, no `..`, no empty segments).

Auto-pin (lease) semantics: all resources opened through the leasing decorator (`LeaseAssets`) are automatically pinned by `asset_root` for the lifetime of the returned handle. The pin is an RAII guard stored inside `LeaseResource`; drop the handle to release the pin.

The global index (`_index/*`) stores small best-effort metadata files. The filesystem remains the source of truth; indexes may be missing and can be rebuilt later.

## Decorator Chain

Requests flow through five layers (outermost to innermost):

<table>
<tr><th>Layer</th><th>Responsibility</th></tr>
<tr><td><code>CachedAssets</code></td><td>In-memory LRU cache (default 5 entries); prevents duplicate mmap opens</td></tr>
<tr><td><code>LeaseAssets</code></td><td>RAII-based pinning; <code>LeaseGuard</code> unpins on drop; prevents eviction of in-use assets</td></tr>
<tr><td><code>ProcessingAssets</code></td><td>Optional chunk-based transformation on <code>commit()</code> (e.g., AES-128-CBC decryption)</td></tr>
<tr><td><code>EvictAssets</code></td><td>LRU eviction by asset count and/or byte size; pinned assets excluded</td></tr>
<tr><td><code>DiskAssetStore</code> / <code>MemAssetStore</code></td><td>Base storage backend; maps <code>ResourceKey</code> to filesystem paths or in-memory resources</td></tr>
</table>

Decorator behavior is capability-gated per call. For an absolute `ResourceKey` (local-file mode), the cache/lease/evict layers bypass and become pass-through; for a relative key under an `asset_root` they apply normally.

```mermaid
sequenceDiagram
    participant Caller
    participant Cache as CachedAssets
    participant Lease as LeaseAssets
    participant Proc as ProcessingAssets
    participant Evict as EvictAssets
    participant Disk as DiskAssetStore
    participant FS as Filesystem

    Caller->>Cache: open_resource_with_ctx(key, ctx)
    Cache->>Cache: check LRU cache
    alt Cache hit
        Cache-->>Caller: cached resource
    else Cache miss
        Cache->>Lease: open_resource_with_ctx(key, ctx)
        Lease->>Lease: pin(asset_root)
        Lease->>Lease: persist _index/pins.bin
        Lease->>Proc: open_resource_with_ctx(key, ctx)
        Proc->>Evict: open_resource_with_ctx(key, ctx)
        Evict->>Evict: first access? check LRU limits
        alt Over limits
            Evict->>Evict: evict oldest unpinned assets
        end
        Evict->>Disk: open_resource_with_ctx(key, ctx)
        Disk->>FS: MmapResource::open(path, mode)
        FS-->>Disk: StorageResource::Mmap
        Disk-->>Evict: StorageResource
        Evict-->>Proc: StorageResource
        Proc->>Proc: wrap in ProcessedResource(res, ctx)
        Proc-->>Lease: ProcessedResource
        Lease->>Lease: wrap in LeaseResource(res, guard)
        Lease-->>Cache: LeaseResource
        Cache->>Cache: insert into LRU cache
        Cache-->>Caller: LeaseResource
    end

    Note over Caller: On commit():
    Caller->>Proc: commit(final_len)
    alt ctx is Some (encrypted)
        Proc->>Proc: read 64KB chunks from disk
        Proc->>Proc: aes128_cbc_process_chunk()
        Proc->>Proc: write decrypted back to disk
    end
    Proc->>Disk: commit(actual_len)

    Note over Caller: On drop(LeaseResource):
    Caller->>Lease: drop LeaseGuard
    Lease->>Lease: unpin(asset_root)
    Lease->>Lease: persist _index/pins.bin
```

## Index Persistence

Three index types are persisted under `_index/` for crash recovery:

<table>
<tr><th>Index</th><th>File</th><th>Purpose</th></tr>
<tr><td>Pins</td><td><code>_index/pins.bin</code></td><td>Persists pinned asset roots</td></tr>
<tr><td>LRU</td><td><code>_index/lru.bin</code></td><td>Monotonic clock + byte accounting for eviction</td></tr>
<tr><td>Availability</td><td><code>_index/availability.bin</code></td><td>Per-resource byte ranges and committed final length — the aggregate snapshot of <code>AvailabilityIndex</code> (see below)</td></tr>
</table>

All indices use `postcard` serialization with `Atomic<R>` for crash-safe writes. The availability index is **explicit only** — persisted via `AssetStore::checkpoint()`, never from a `Drop` hook or background timer.

### Pins index

`PinsIndex` is the in-memory + best-effort disk-backed index of pinned `asset_root`s. It is architecturally symmetric to `AvailabilityIndex`: the `Arc` is encapsulated inside the type, `Clone` is a cheap atomic refcount bump, and every mutation that crosses the pinned/unpinned boundary immediately flushes to the optional disk-backed `Atomic` tempfile.

- Each `asset_root` is tracked by a refcount. Concurrent leases on the same root increment it and drops decrement it. The on-disk pinned set only changes (and only flushes) on the 0→1 and 1→0 transitions; intermediate increments/decrements are pure in-memory updates.
- Persistence is lazy: the disk file is materialised only on the first `flush`. A pre-existing on-disk file from a previous run is opened eagerly during `with_persist_at` (native only) for hydration. On wasm32 the index is always ephemeral.
- Three call-sites share a single instance per `cache_dir`: `LeaseAssets` (pin/unpin on resource lifecycle), `EvictAssets` (read pinned set when picking eviction candidates), and `DiskAssetDeleter` (drop pin when an `asset_root` is fully removed).

## Byte Availability — single source of truth

`AssetStore` is the sole authoritative answer to "which bytes of this resource are present?". Callers query it through three read-only methods that are safe to invoke from high-frequency hot paths (e.g. the HLS decoder read loop):

```rust
let scope = store.scope(asset_root);
scope.contains_range(&key, 0..4096); // bool — every byte in range
scope.available_ranges(&key);        // RangeSet<u64> — full snapshot
scope.final_len(&key);               // Option<u64> — committed size
```

Internally these sit on top of an aggregate `AvailabilityIndex` keyed by `(asset_root, ResourceKey)`:

- **Updated** by a `ScopedAvailabilityObserver` attached to every `Resource` opened through `DiskAssetStore` / `MemAssetStore`. Each `Resource::write_at` fires `on_write(range)` and each successful `Resource::commit(Some(len))` fires `on_commit(len)`. Opening a pre-existing committed file also seeds `0..final_len`.
- **Queried** with a fast path first (`DashMap::get → Arc::clone → Mutex::lock`, with the shard guard released before the inner lock). A cold miss on `Disk` falls back once to `resource_state` so pre-existing committed files on disk are still discoverable before the observer has ever fired.
- **Persisted** on demand via `AssetStore::checkpoint()` to `_index/availability.bin`. Missing / corrupt / wrong-version files are silently treated as an empty seed on rebuild.

`Resource<D>::CommonState.available` remains the per-resource byte map inside `kithara-storage`, but it is an implementation detail — consumers outside `kithara-storage` must query through `AssetStore`, not through `resource.contains_range()` on an ad-hoc `open_resource` call.

## Processing & readiness gate (Pending/Ready typestate)

Decrypt-readiness is a **phantom typestate** on a split acquisition handle, not a runtime flag. Acquiring an encrypted resource (`ctx = Some`) yields `AcquisitionResult::Pending(ProcessedWriter)`; a committed/cache-hit resource — or any `ctx = None` resource (playlists, keys) — yields `AcquisitionResult::Ready(ProcessedReader)`. Callers pattern-match the outcome; there is no runtime `is_readable()` check.

- `ProcessedWriter` (Pending, **non-`Clone`**) owns the write+decrypt capability: `write_at` streams raw ciphertext to disk; `commit(self, final_len)` reads it back in chunks, transforms each via the callback (no allocation, e.g. AES-128-CBC), writes the plaintext back, and **consumes** the writer into a `ProcessedReader`. `fail(self)` — or dropping the writer without committing — fails the gate so no waiting reader deadlocks. The writer exposes **no reads**: `read_at` on a Pending handle is a compile error.
- `ProcessedReader` (Ready, `Clone`) exposes `read_at`/`wait_range`/`contains_range`/`len`/`next_gap`/`path` over already-processed bytes. `reactivate(self)` consumes the reader into a fresh `ProcessedWriter` carrying a **new** readiness gate.

This replaces the former runtime `ReadinessGate` (a `processed: Mutex<bool>` + `Condvar` shared across `Clone`s of one `ProcessedResource`). That shared gate was the root of the DRM read-before-ready race: a re-fetching writer's `reactivate` flipped the gate for an extant reader's committed clone, so `read_at` hit `StorageError::NotReadable` mid-playback (`live_ephemeral_small_cache_playback_drm`; the older `local_queue_playlist_behavior_*` post-seek hang). The fresh-gate-on-`reactivate` plus the non-`Clone` writer make that race unrepresentable — a reacquiring writer gets its own gate and a committed reader's type-level readiness is never revoked. The gate survives **privately** inside `process.rs` only as the writer↔reader handoff primitive, reachable through `commit`/`await_ready`. Storage lifecycle `status()` stays a runtime `&self` facade (see "Decorator Chain"); the two axes are independent.

## Trait Bridges

- `&Url` → `ResourceKey` (`From`) — derive a unique key from a URL, query-aware
- `&StoreOptions` → `EvictConfig` (`From`) — extract eviction config from store options
- `ResourceStatus` → `AssetResourceState` (`From`) — map storage status to asset state
- `&LruState` ↔ `LruIndexFile` (`From` both ways) — LRU index persistence round-trip
- `DiskStore<Ctx>` / `MemStore<Ctx>` → `AssetStore<Ctx>` (`From`) — wrap a backend into the unified store

## Consumer Demand

`DemandIndex` is a sibling of `AvailabilityIndex` on `AssetStore`: one instance per `build()`, shared across store clones via `Arc`. Where availability answers "which bytes are present?", demand answers "how far ahead should the single download producer fetch?". It is consumer-driven and protocol-agnostic (byte offsets + a refcount + a cancel token, no HTTP), so unlike availability it needs no storage observer and no decorator threading — just a field and accessors.

Each consumer attaches once:

```rust
let (lease, producer) = store.attach_demand(&key, read_pos, look_ahead);
```

- `read_pos` is an `Arc<AtomicU64>` shared with the consumer; the producer reads its advances directly. `look_ahead` of `None` means "wants the whole file" and collapses that consumer's watermark to `u64::MAX`.
- Per-key state lives in a `DemandCell { entries, refcount, producer_spawned, producer_cancel, notify }`. The aggregate watermark is the **max** of every live consumer's `read_pos + look_ahead` — one mechanism, no "wants-full" flag.
- `attach_demand` get-or-inserts a live slot, pushes the entry, bumps the refcount, and wakes the producer. It returns a `DemandLease` (always) and a `ProducerHandle` to the single CAS-winning attacher only. `producer_cancel` is a child of the store cancel.
- `DemandLease::Drop` removes the entry and decrements the refcount; on zero it cancels `producer_cancel` and removes the slot. `DemandLease::note_progress()` wakes the producer when the consumer's read position advances. Attach and detach both serialize through the per-key shard lock, closing the attach-during-last-drop race.
- The producer driver lives in the protocol crate (`kithara-file`, `kithara-hls`), which speaks HTTP; the index only hands it the slot (`max_watermark`, `notify`) and `producer_cancel`. The driver is owned by the producer election, not by the spawning consumer — dropping any one consumer just decrements the refcount.
- Producer handoff: the election is sticky while the elected `ProducerHandle` is alive. Dropping it (the producer's peer/source went away) re-opens the election by clearing `producer_spawned` and waking the slot, so a surviving consumer's next `DemandLease::try_take_producer()` wins and takes over the download. This keeps a single producer at any instant while letting the role migrate when the original producer leaves but others still need bytes.

Known v1 limitation: the producer observes `producer_cancel` only at its throttle await, not mid-chunk. If the last consumer detaches and a new one attaches almost immediately, the old task can briefly overlap the new one — a short window of two in-flight GETs. Overlapping writes are idempotent, so this is wasteful, not incorrect.

`DemandKey` is the `ResourceKey`, matching the granularity at which the store shares a resource. HTTP-response metadata (content length, codec) is **not** part of the demand index: only the producer (the CAS winner) observes the response headers; other consumers see the shared bytes via availability and the committed `final_len`, and rely on byte-probe codec detection (best-effort, same as a single consumer that lacks a `Content-Type`).

## Integration

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides a unified `AssetStore` type (`Disk`/`Mem`) that internally composes decorators: `CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<...>>>>`.
