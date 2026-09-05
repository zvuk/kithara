# kithara-assets — Context

Contracts and invariants for kithara-assets; the README is the overview.

## Storage backend

Capabilities come from the base store and gate every decorator per call:
`DiskAssetStore` reports `Capabilities::all()`, `MemAssetStore` only
`CACHE | PROCESSING`. Eviction and lease/pin logic are therefore inert on a
memory store, where `max_bytes` is enforced by the cache instead (see "Memory
byte bound").

Every growable byte buffer comes from the application-owned `PoolRegion<S>`, whose
clones share one hard overall budget: there is no global pool, implicit default, or
per-component fallback allocation.

## Key mapping (normative)

Disk mapping is `<cache_root>/<layout.root(source)>/<layout.path(resource)>`.
`cache_root` belongs to `StorageBackend::Disk`; a layout cannot escape or replace
it, and the store validates both layout outputs before constructing a `ResourceKey`.

An absolute `ResourceKey` is the separate read-in-place capability for local source
media: it points at the original file, describes no cache file, and is rejected by
`remove_resource` / `delete_asset` with `AssetsError::InvalidKey`. Cached resources
*derived* from a local source still use `AssetSource::Local` and the normal mapping
under `cache_root`.

### Layout

The store's `AssetLayoutRegistry` is keyed by the exact protocol marker type and
is a snapshot of store construction: mutating a registry afterwards has no effect,
and switching layouts does not migrate existing cache entries.
`AssetStore::scope::<T>(&AssetSource)` calls `AssetLayout::root` once and each
`AssetScope::key(&AssetResource)` calls `AssetLayout::path` once. The resulting
`ResourceKey` carries both resolved components, so no later operation consults the
registry or the layout again; calling `scope` or `key` again is a new derivation that
re-invokes the callback.

`DefaultLayout` mapping below `<cache_root>/<asset_root>`:

| Resource | Path |
|---|---|
| `Source` | `track/track.<safe-ext>` |
| `Url` | `track/<encoded-authority>/<encoded-server-path>` |
| `Named { namespace, name }` | `<encoded-namespace>/<encoded-name>` |

The default remote `asset_root` hashes the canonical source URL with query and
fragment removed; an explicit `discriminator` folds into a domain-separated root
identity. The default local root is a domain-separated hash of the absolute lexical
path, with no canonicalization and no filesystem I/O.
kithara-assets owns layout registration, validation, and filesystem mapping, not
domain or transport policy: a higher layer implements `AssetLayout` over the
complete source/resource values and registers it for `File`, `Hls`, or another
marker. The domain-scoped query identity policy (`QueryIdentityLayout`) lives in
kithara-play.

Every URL path and authority component is encoded portably, so cross-origin HLS
resources cannot alias. A non-empty query becomes an ordered bounded fingerprint
before the leaf extension, so query text and credentials never reach disk; fragments
do not affect the path, and `AssetResource::Source` carries no query.
`AssetResource::Named` is the extension point for derived artifacts — analysis is an
ordinary `Named`, not a bypass — and the reserved `track` namespace encodes
separately (`~ntrack`) so a named resource cannot alias direct source bytes. `_index/`
is store metadata, reserved outside every asset root; `<resource>.tmp` is the
transient atomic-write companion, so layouts cannot mint `.tmp` names.

Custom layouts must be deterministic, non-blocking, and secret-free, and emit
portable ASCII components (`layout/validation.rs` is the normative validator; a
root may not be `_index`). Invalid output returns `AssetsError::InvalidKey`; the
store neither rewrites it nor falls back to another layout (`hostile_layout.rs`).

## Decorator chain

A decorator becomes a transparent pass-through for an absolute `ResourceKey` and
whenever its capability bit is absent. A handle opened through `LeaseAssets` pins its
`asset_root` for the handle's lifetime. Abandoning a `LeaseWriter` without
committing removes the partial resource; an explicit `fail(reason)` keeps it
observable through `resource_state` instead. `retain()` on a cached handle pins it
until `CachedReader::release` for the same key, exempt from capacity and
byte-bound eviction alike.

## Pending/Ready typestate

Acquisition phase is a typestate, not a runtime flag. An encrypted,
not-yet-committed resource (`ctx = Some`) yields `Pending`; a committed resource, a
cache hit, or any `ctx = None` resource yields `Ready`. No runtime `is_readable()`.

- The writer is **not `Clone`** and has no reads. `fail(self)` or dropping it
  uncommitted fails the readiness gate, so no waiting reader deadlocks.
  `reactivate(self)` consumes a reader into a writer carrying a **fresh** gate, so
  clones of a committed reader keep their own generation and their readiness is never
  revoked.
- `ReadSide::read_inflight_at` is the writer's read-back of the active generation,
  bypassing the gate and the published committed snapshot; mint it from
  `WriteSide::reader`. External consumers use `read_at`.
  `WriteSide::raw_write_handle()` yields a `RawWriteHandle` into the writer's
  generation, so a `'static` closure can stream bytes while the non-`Clone` writer
  keeps sole ownership of `commit`.
- The cache stores **only readers** of the current generation: an acquire hitting a
  non-committed slot calls `reactivate`, re-keys the entry with the new generation's
  reader view (carrying its hit count), and returns the writer.

The readiness gate is private to `decorator/processing/gate.rs`, the writer↔reader
handoff primitive reached through `commit` / `wait_range`; lifecycle `status()` is an
independent runtime axis. `ReadSide::wait_range_with_cancel` interrupts one caller
through both the backing wait and the processing gate without cancelling the shared
resource.

## Per-acquire processing (`ProcessCtx`)

The store is **not** generic over a processing context: a `ProcessCtx` travels per
acquire, and `None` is identity passthrough. That is what lets one `AssetStore<S>`
serve plain (file) and decrypting (HLS) scopes with no second store instance and no
second `_index/` set.
There is no build-time `process_fn` and no AES primitive in this crate, only the
trait, implemented by consumers (HLS `DecryptProcessor`). `identity()` is the
immutable cache identity (e.g. `key||iv`); `begin()` mints fresh chaining state that
`ChunkSink::process` then evolves across chunks (e.g. the CBC IV). Every `commit` —
including after `reactivate` — calls `begin()`, so chaining always restarts from the
seed.

Cache identity is exact bytes: the key is
`(ResourceKey, Option<RequestIdentity>, Option<CtxIdentity>)` and equality is
**byte-exact**, not a digest, so distinct processors never collide. `CtxIdentity`
wraps `ResourceProcessor::identity()` through the crate-internal `CacheIdentity`
bridge and redacts its `Debug`, because the bytes can be key material. An open with
`ctx = None` additionally matches any cached **committed** entry for the same
`ResourceKey` whatever its ctx: post-processing bytes are ctx-independent once
committed (`ctx_erasure.rs`).

Memory byte bound: `max_bytes` has backend-specific ownership. On disk
`EvictAssets` applies it to persisted asset roots. On memory `CachedAssets` applies
it to the aggregate bytes of committed, unretained cache entries; eviction follows
the cache's frequency-aware order (fewest hits, ties to the LRU end) and runs the
volatile invalidation hook per removed resource. Retained entries are exempt until
released. A committed resource of unknown length counts as unbounded and so cannot
stay in a byte-bounded memory cache. None of this runs from `read_at` or the decoder
read loop.

## Byte availability — single source of truth

`AssetStore` is the sole authority on which bytes of a resource are present, via
three read-only methods safe on hot paths (e.g. the HLS decoder read loop):
`contains_range`, `available_ranges`, `final_len`. They sit on the aggregate
`AvailabilityIndex`:

- **Updated** by a `ScopedAvailabilityObserver` attached to every resource opened
  through the base stores; opening a pre-existing committed file seeds
  `0..final_len`.
- **Queried** aggregate-only: no handle-cache mutex, no filesystem call, **and no
  lock at all** — the audio worker's produce path asks `contains_range` from inside its
  forbid-blocking region (the HLS `phase_at` cascade), so readers load `arc-swap`
  snapshots and writers publish new ones. A reader racing a writer sees the state from
  a moment ago, which for "is this byte on disk yet" is indistinguishable from having
  asked a moment ago. Reclamation follows the same split: a produce-path read can be
  the last owner of a replaced generation, so it never drops the snapshot it loads — it
  parks it in a bounded retire bin the write side (download and deletion paths) drains
  and pays the frees for. Overflow leaks the reference permanently, and is **not**
  rare: parking follows reads (audio-tick cadence) while draining follows writes, so
  cache-served playback overflows continuously; raising the bin capacity only moves the
  threshold, and the fix is for the read to stop taking ownership
  (`a_read_burst_never_leaks_a_generation`). A resource the aggregate does not know is
  absent and gets refetched — the same verdict `is_confirmed` gives the acquire path.
  `resource_state` is the control-side inspection API and may block.
- **Persisted** in two tiers (below), and it is the **authority on what survives a
  restart**. A segment's file becomes visible at `rename`, but the barrier that puts
  its blocks on the medium is paid later by the availability flush, so a file alone
  proves nothing — after a power cut it can carry the right name and length over
  unwritten blocks. `DiskAssetStore` therefore treats an existing file as ready only
  when `availability` knows its final length; anything else is a torn write and is
  refetched. A missing or corrupt `availability.bin` costs a refetch, never a wrong
  read (`crash_recovery.rs`).
- **Reconciled** on hydration: an entry the store root no longer backs is dropped as
  the snapshot loads, so a pruned cache cannot come back claiming ranges nothing can
  serve. One `exists` per persisted resource, at startup only.

The persisted snapshot is a **committed-only** contract — an uncommitted partial write
is never serialised, so a flush racing a writer's cleanup cannot resurrect a partial
segment whose `.tmp` was never renamed. Live in-memory ranges still serve in-flight
readers; only the snapshot filters. kithara-storage keeps its own per-resource byte
map, but it is an implementation detail: consumers outside that crate must query
through `AssetStore`, never through `contains_range()` on an ad-hoc `open_resource`
handle.

## Index persistence

| File | Holds |
|---|---|
| `_index/pins.bin` | Pinned asset roots (membership only; refcounts stay in memory) |
| `_index/lru.bin` | Monotonic clock + per-root byte accounting |
| `_index/availability.bin` | Committed ranges and final length per resource |

All three write through `Atomic<R>` and register with a shared `FlushHub`. Two
tiers:

- **Non-durable** best-effort: the background worker atomically renames without
  `sync_data`. Availability uses this tier for its own snapshot. The files it vouches
  for are a different matter: each availability flush first `sync_data`s the segment
  files committed since the last one, and only then writes the snapshot. That ordering
  is what lets a segment commit skip its own barrier (`Barrier::Deferred`) — one flush
  pays for many segments, off the resource-write path. Reversing it would let the
  manifest vouch for bytes that never landed.
- **Durable** authoritative: `AssetStore::checkpoint()` runs `flush_durable`
  (`sync_data` before rename) over every source, including sources the worker last
  wrote non-durably whose `dirty` flag is now clear, so a checkpoint never leaves a
  stale worker snapshot behind. The store checkpoints itself when its last handle
  drops; every index holds the flush hub alive, so waiting for the hub's own teardown
  would find the sources already gone.
- Pins and LRU never wait for the worker: their mutators call `flush_sync` eagerly,
  because a pin lost to a crash would let a live asset be evicted. That eager write
  bypasses the hub's flush lock, so each disk-backed index serialises its own file
  under one per-file lock covering both the snapshot and the atomic rename. Without
  it a flush that snapshotted before an unpin could rename after it and resurrect a
  pin nobody holds — the asset then outlives every handle and eviction can never
  reclaim it. The filesystem stays the source of truth: any index may be missing and
  can be rebuilt.

### Pins index

- Each `asset_root` carries two refcounts split by `PinDurability`. Both bar eviction
  identically — `snapshot` covers them together — and differ only in reach: a `Durable`
  pin (an unfinished write) is persisted, a `Local` one (a reader holding committed
  bytes) is not. Only the durable count's 0→1 and 1→0 transitions flush
  `_index/pins.bin`; everything else stays in memory, which is what keeps the decoder
  read loop off the disk. A `Local` pin is deliberately invisible to the next process:
  a pin that cannot outlive its holder would hydrate as a phantom nothing releases.
- **One live `AssetStore` per disk root in a process.** A second store is a second
  owner of `_index/pins.bin`: it hydrates its own copy, so its next flush republishes a
  snapshot taken before the other store's unpin and resurrects a pin nobody holds. It
  also keeps its own `EvictAssets` `seen` set, so eviction runs again against that
  stale pinned set. The per-file lock above reaches only one owner, so neither is
  serialisable from inside one instance. Share by cheap clone of the handle, or by
  passing the built indices through the builder; never by two stores discovering each
  other through the filesystem.

### LRU index and eviction policy

- Eviction is evaluated only the first time an `asset_root` is observed in this
  process (`EvictAssets` keeps a `seen` set), and the newly opened root counts as
  pinned for that pass.
- Decisions read the in-memory `LruIndex` / `PinsIndex` snapshots; `max_assets` and
  `max_bytes` are soft caps enforced best-effort.
- Byte accounting is push-based — the evictor never walks the filesystem.
  `LeaseWriter::commit` measures the committed file and reports it through
  `ByteRecorder` into `LruIndex::update_bytes`, which never bumps recency: re-bumping
  per segment commit would coalesce every bytes-bearing asset to the same logical time
  and skew eviction order.

### Canonical deletion channel

Every path that physically removes bytes goes through `AssetDeleter`: own-asset
teardown, resource removal, and foreign-asset eviction alike. Each method
synchronises the storage-side change with every index reflecting it, so
`AssetStore::remove_resource` must not invalidate availability again. Bypassing this
channel strands `contains_range` / `final_len` on bytes that no longer exist, and the
reader spins on `wait_range = Ready` / `read_at = Retry` until the hang detector
fires.

## Consumer demand

`PendingResourceIndex`, `ResourceTransactionIndex`, and `EvictionRouter` are
consumer-driven siblings of `AvailabilityIndex`: one instance per `build()`, shared
across store clones via `Arc`, needing no decorator threading, so one store serves
file and HLS with no protocol-owned wrapper.

Availability answers "which bytes are present?"; the pending-resource index owns the
one not-yet-ready resource per `ResourceKey` — aggregate consumer demand, writer
election and epoch, the sole `AssetWriter`, its shared reader — and stays
protocol-agnostic (byte offsets and cancellation, no HTTP). A consumer joins through
`AssetStore::attach_pending_resource`; an active hit returns a reader and lease from
the existing pending resource without another cache acquire.

- `read_pos` is shared with the consumer, so the writer sees advances directly. A
  consumer's watermark is the maximum of `read_pos + look_ahead` and a monotonic
  immediate requested-end floor, so a blocking read larger than the prefetch window
  cannot wait on bytes the writer was forbidden to fetch. The aggregate watermark is
  the maximum live consumer watermark.
- The cell mutex is the only source of truth for consumers, lifecycle, writer epoch,
  writer, and reader. Dropping the elected handle invalidates its epoch immediately; a
  surviving lease can take the writer role without replacing the writer, and raw writes
  and terminal operations validate the exact epoch while mutating the canonical
  session.
- The session cancel is a child of the store cancel and is the terminal election gate:
  once it fires, current fetch descendants stop and no attachment or surviving lease
  can elect another writer epoch.
- Lock order is demand-map shard -> cell mutex -> cache/storage. Nothing calls back
  into `PendingResourceIndex` — not cache, availability, pins, writer or lease cleanup
  — and no source-local, event, or user callback runs under these locks; the canonical
  write observer is limited to `AvailabilityIndex`. Audio readers hold only
  `AssetReader` clones and never take a demand lock.
- Reader and peer-poll `Waker` registrations are one-shot. A transition takes them
  under the cell mutex, releases both the cell and demand-map guard, then invokes
  them. The peer arms-checks-rechecks around demand and election: register before the
  check, leave the registration armed when it returns `Pending`, and clear only its
  exact waker after the recheck confirms readiness, so clearing a stale waker cannot
  remove a newer registration. An already in-flight protocol operation instead relies
  on its completion wake. A synchronous reader uses a non-owning `Weak` adapter to its
  worker wake and rearms after wake; synchronous cancel callbacks follow the same
  outside-lock boundary.
- Last-consumer abandonment and terminal failure keep the exact map entry locked
  through outer removal, so a successor cannot publish until old bytes and live state
  are gone. Successful commit instead unpublishes the cell without removing backing
  bytes, then cancels requests from the old writer epochs; old leases become stale. A
  failed outer removal leaves the cell as a typed tombstone retaining the
  `ResourceKey` and shared source error, and later attachments receive a fresh
  `PendingResourceCleanupError` through the existing error surface. A Pending writer
  transfers its `LeaseWriter` abandonment hook to the cell before publication, after
  which that typed outer removal is the sole cleanup owner for abandonment, failure,
  and commit error; no hidden first remove can precede a tombstone.
- The writer driver lives in the protocol crate (currently kithara-file), which speaks
  HTTP; the index hands it only `max_watermark`, the session cancel, and lease-scoped
  reader/peer wake registration. It belongs to the election, not to the spawning
  consumer. The election is sticky while the elected `WriterHandle` lives: dropping it
  clears the matching writer epoch and wakes surviving peer registrations, so a
  survivor's next `try_take_writer()` takes over — one writer at any instant, role
  migratable.
- The key is the `ResourceKey`, matching the granularity at which the store shares a
  resource. HTTP-response metadata (content length, codec) is **not** in the index:
  only the elected writer epoch sees response headers, so other consumers rely on
  availability, the committed `final_len`, and byte-probe codec detection.

## Resource transactions

`AssetStore::with_resource_transaction` serializes one read/validate/mutate operation
per `ResourceKey` across clones of the same store; the closure must re-read state
after entering, and cancellation releases or forwards the transaction to the next
waiter. It is process-local, ephemeral, and non-reentrant for the same store and key:
it coordinates cache mutation, with no rollback and no cross-process locking.

## Eviction subscription

- The ephemeral build path wires the router into the cache's single `on_invalidated`
  hook: on volatile displacement the hook clears the `AvailabilityIndex` entry and
  routes the evicted key by its `asset_root`. Every subscriber for that root receives
  it; the returned `EvictionSubscription` guard deregisters only its own registration.
- Firing is gated on volatile displacement: active for `MemAssetStore`, where
  displacement frees bytes, and dormant for durable disk backings, where displaced
  bytes survive — the disk path wires no hook at all. There is no public callback and
  no builder field; the router reaches the cache only through the ephemeral path.

## Configuration document

`AssetStoreConfig<S>` is this crate's one configuration struct — the eight
tunables and the five handles a caller must supply together — and
`AssetStore::open` takes it whole. `AssetStore::builder(pools)` is bon's builder
over that struct; its `build()` opens the store, `into_config()` stops one step
short and returns the configuration, which is how `kithara-app` applies a
document patch before opening.

`#[derive(Patch)]` generates `AssetStoreConfigPatch`, what a document's
`assets.store:` section may say: `backend`, `cache_capacity`, `max_assets`,
`max_bytes`, `mem_resource_capacity`, `processing_chunk_size`,
`processing_gate_poll_interval`, `segment_reservation`. Two of them are
backend-specific and inert on the other backend, exactly as they are on the
builder: `mem_resource_capacity` is read only on the memory branch, and
`segment_reservation` reaches only `DiskStoreSetup`. `pools`, `cancel`,
`event_bus`, `flush_hub` and `layouts` are skipped — each is a live value only
code can hand over — so naming one is refused rather than dropped.

An unset `backend` falls back to a fresh uniquely-named temp directory per call
(`fresh_temp_root`), which would relocate an application's on-disk cache every
launch. A caller that wants a stable default when the document is silent
resolves the `Option<StorageBackend>` itself; `kithara-app` does that in
`Config::assets_store`.

`StorageBackend` carries a hand-written `Deserialize`, not a derive. `Memory` is
a unit variant and serde checks `deny_unknown_fields` against a variant's own
field list, so a derived `#[serde(tag = "kind", deny_unknown_fields)]` would let
`{kind: memory, root: /x}` parse and silently drop `root`. The private
`BackendDoc` mirror in `store/builder.rs` carries the check instead.
