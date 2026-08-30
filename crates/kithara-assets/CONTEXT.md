# kithara-assets — Context

Contracts and invariants for kithara-assets; the README is the overview.

## Storage backend

`StorageBackend` selects where committed bytes live: `Memory` (dies with the process) or
`Disk { root }`. `AssetStore::builder()` with no `backend` gets a fresh unique temp directory on
native; wasm ignores the argument and is always in-memory. Capabilities come from the base store
and gate every decorator per call: `DiskAssetStore` reports
`Capabilities::all()`, `MemAssetStore` only `CACHE | PROCESSING`. Eviction and lease/pin logic are
therefore inert on a memory store, where `max_bytes` is enforced by the cache instead (see "Memory
byte bound").

## Key mapping (normative)

Disk mapping is `<cache_root>/<layout.root(source)>/<layout.path(resource)>`. `cache_root` belongs
to `StorageBackend::Disk`; a layout cannot escape or replace it. Higher layers describe an
`AssetSource` and `AssetResource` and never supply a preformed relative cache path; the store
validates both layout outputs before constructing a `ResourceKey`.
An absolute `ResourceKey` is the separate read-in-place capability for local source media: it
points at the original file, so it describes no cache file and `remove_resource` / `delete_asset`
reject it with `AssetsError::InvalidKey`. Cached resources derived from a local source still use
`AssetSource::Local` and the normal layout mapping under `cache_root`.

### Layout

The store owns an immutable `AssetLayoutRegistry` supplied through the builder's `layouts` setter.
A registration is keyed by the exact protocol marker type; an absent marker uses the registry
default, and re-registering a marker replaces its layout. The selected layout is a snapshot of
store construction: mutating a registry afterwards has no effect, and switching layouts does not
migrate existing cache entries. `AssetStore::scope::<T>(&AssetSource)` does one registry lookup and
calls `AssetLayout::root` once; each `AssetScope::key(&AssetResource)` calls `AssetLayout::path`
once. The resulting `ResourceKey` carries both resolved components, so acquire, open, read, write,
seek, state, availability, demand, and eviction never consult the registry or the layout again.
Calling `scope` or `key` again is a new derivation and re-invokes the callback.

`DefaultLayout` mapping below `<cache_root>/<asset_root>`:

| Resource | Path |
|---|---|
| `Source` (direct remote file bytes) | `track/track.<safe-ext>` |
| `Url` (playlist, init, segment, key) | `track/<encoded-authority>/<encoded-server-path>` |
| `Named { namespace, name }` | `<encoded-namespace>/<encoded-name>` |

The default remote `asset_root` is a 128-bit SHA-256 prefix over the canonical source URL with
query and fragment removed; an explicit `discriminator` folds into a domain-separated root
identity. The default local root is a domain-separated hash of the absolute lexical path, with no
canonicalization and no filesystem I/O. kithara-assets owns layout registration, validation, and
filesystem mapping — not domain or transport policy: higher layers implement `AssetLayout` over the
complete source/resource values and register it for `File`, `Hls`, or another marker, and the
domain-scoped query identity policy (`QueryIdentityLayout`) lives in kithara-play.

Every URL path and authority component is encoded portably, so cross-origin HLS resources cannot
alias. A non-empty query becomes an ordered bounded fingerprint inserted before the leaf extension;
query text and credentials never reach disk, fragments do not affect the path, and
`AssetResource::Source` carries no query. `AssetResource::Named` is the extension point for derived
artifacts (analysis is the ordinary `Named { namespace: "analysis", name: "track.analysis" }`, not
a bypass); the reserved `track` namespace encodes separately (`~ntrack`) so a named resource cannot
alias direct source bytes. `_index/` is store metadata, reserved outside every asset root;
`<resource>.tmp` is the transient atomic-write companion, so layouts cannot mint `.tmp` names.

Custom layouts must be deterministic, non-blocking, and secret-free. A root is one non-empty
portable component and cannot be `_index`; a path is a non-empty relative sequence of portable
components. Components are ASCII, ≤96 bytes, not `.` or `..`, no Windows device names or reserved
path characters, and cannot end in a dot, space, or `.tmp`. Invalid output returns
`AssetsError::InvalidKey`; the store neither rewrites it nor falls back to another layout.

## Decorator chain

Outermost to innermost:

| Layer | Responsibility |
|---|---|
| `LeaseAssets` | RAII pin by `asset_root`; partial-write cleanup; asset events |
| `CachedAssets` | In-memory LRU handle cache (default capacity 5); optional byte bound |
| `ProcessingAssets` | Optional chunk transform on `commit` (e.g. AES-128-CBC) |
| `EvictAssets` | LRU eviction by asset count and/or bytes; pinned roots excluded |
| `DiskAssetStore` / `MemAssetStore` | Base storage for a `ResourceKey` |

A decorator becomes a transparent pass-through for an absolute `ResourceKey` and whenever its
capability bit is absent. Handles opened through `LeaseAssets` pin their `asset_root` for the
handle's lifetime via an RAII guard inside `LeaseWriter` / `LeaseReader`; a writer's pin is durable
and its 0→1 / 1→0 transitions flush `_index/pins.bin`, a reader's pin is process-local and stays in
memory. Abandoning a `LeaseWriter` without committing removes the
partial resource; an explicit `fail(reason)` keeps it observable through `resource_state` instead.
`retain()` on a cached handle pins it in the LRU cache until `CachedReader::release` for the same
key; retained entries are exempt from both capacity and byte-bound eviction.

## Pending/Ready typestate

Acquisition phase is a typestate, not a runtime flag. `acquire_resource*` returns
`AcquisitionResult<ActiveRes: WriteSide, ReadyRes: ReadSide>`; `open_resource*` returns the reader
directly. An encrypted, not-yet-committed resource (`ctx = Some`) yields `Pending`; a committed
resource, a cache hit, or any `ctx = None` resource yields `Ready`. No runtime `is_readable()`.

- The writer is **not `Clone`** and has no reads. `commit(self, final_len)` runs the transform and
  consumes it into a reader; `fail(self)` or dropping it uncommitted fails the readiness gate so no
  waiting reader deadlocks. The reader is `Clone` and read-only; `reactivate(self)` consumes it
  into a writer carrying a **fresh** gate, so clones of a committed reader keep their own
  generation and their readiness is never revoked.
- `ReadSide::read_inflight_at` is the writer's read-back of the active generation, bypassing the
  gate and the published committed snapshot; mint it from `WriteSide::reader`. External consumers
  use `read_at`. `WriteSide::raw_write_handle()` yields a clone-able `RawWriteHandle` into the
  writer's generation, so a `'static` raw-write closure can stream bytes while the non-`Clone`
  writer keeps sole ownership of `commit`.

Decorators carry the split through mutually recursive associated types
(`WriteSide::Reader: ReadSide<Writer = Self>` ↔ `ReadSide::Writer: WriteSide<Reader = Self>`), from
`BaseWriter`/`BaseReader` at the storage seam up to the facade aliases `AssetWriter` /
`AssetReader`. The cache stores **only readers** of the current generation: an acquire hitting a
non-committed slot calls `reactivate`, re-keys the entry with the new generation's reader view
(carrying its hit count), and returns the writer. The readiness gate is private to
`decorator/processing/gate.rs`, the writer↔reader handoff primitive reached through `commit` /
`wait_range`; lifecycle `status()` is an independent runtime axis. `ReadSide::wait_range_with_cancel`
interrupts one caller through both the backing wait and the processing gate without cancelling the
shared resource.

## Per-acquire processing (`ProcessCtx`)

The store is **not** generic over a processing context. Processing travels per acquire as
`ProcessCtx = Arc<dyn ResourceProcessor>`; `None` is identity passthrough. That is what lets one
non-generic `AssetStore` serve plain (file) and decrypting (HLS) scopes with no second store
instance and no second `_index/` set. There is no build-time `process_fn` and no AES primitive in
this crate, only the trait. `ResourceProcessor` is implemented by consumers (HLS
`DecryptProcessor`): `identity() -> &[u8]` is the immutable cache identity (e.g. `key||iv`);
`begin() -> Box<dyn ChunkSink>` mints fresh per-commit chaining state. `ChunkSink::process` carries
the evolving state (e.g. the CBC IV across 64 KiB chunks), and every `commit` — including after
`reactivate` — calls `begin()`, so chaining always restarts from the seed.

Cache identity is exact bytes. The key is `(ResourceKey, Option<RequestIdentity>, Option<CtxIdentity>)`, where `CtxIdentity` wraps `ResourceProcessor::identity()` through the
crate-internal `CacheIdentity` bridge. Equality is **byte-exact**, not a digest, so distinct
processors never collide, and `CtxIdentity`'s `Debug` is redacted because the bytes can be key
material. An open with `ctx = None` additionally matches any cached **committed** entry for the
same `ResourceKey` whatever its ctx: post-processing bytes are ctx-independent once committed.

Memory byte bound: `max_bytes` has backend-specific ownership. On disk `EvictAssets` applies it to
persisted asset roots. On memory `CachedAssets` applies it to the aggregate bytes of committed,
unretained cache entries, checked after cache insertion, after commit, and on
`CachedReader::release`; eviction follows the cache's frequency-aware order (fewest hits, ties to
the LRU end) and runs the volatile invalidation hook per removed resource. Retained entries are
exempt until released. A committed resource of unknown length counts as unbounded and so cannot
stay in a byte-bounded memory cache. None of this runs from `read_at` or the decoder read loop.

## Byte availability — single source of truth

`AssetStore` is the sole authority on which bytes of a resource are present, via three read-only
methods safe on hot paths (e.g. the HLS decoder read loop): `contains_range(&key, range) -> bool`,
`available_ranges(&key) -> RangeSet<u64>`, `final_len(&key) -> Option<u64>`. They sit on an
aggregate `AvailabilityIndex` keyed by the `asset_root` and relative path already carried in the
`ResourceKey`:

- **Updated** by a `ScopedAvailabilityObserver` attached to every resource opened through the base
  stores: `write_at` fires `on_write(range)`, a successful `commit(Some(len))` fires
  `on_commit(len)`, and opening a pre-existing committed file seeds `0..final_len`.
- **Queried** aggregate-only: no handle-cache mutex, no filesystem call, **and no lock at all** —
  the audio worker's produce path asks `contains_range` from inside its forbid-blocking region (the
  HLS `phase_at` cascade), so readers load `arc-swap` snapshots and writers publish new ones. A
  reader racing a writer sees the state from a moment ago, which for "is this byte on disk yet" is
  indistinguishable from having asked a moment ago. Reclamation follows the same split: a reader
  racing a writer can end up the last owner of a replaced snapshot generation, so produce-path
  reads never drop the snapshots they load — they park them in a bounded retire bin and the write
  side (download and deletion paths) drains it and pays the frees; overflow leaks instead of
  freeing on the reader, and only happens while writers are idle, i.e. while generations are not
  being replaced. A resource the aggregate does not know is
  absent and gets refetched — the same verdict `is_confirmed` gives the acquire path.
  `resource_state` is the control-side inspection API and may block.
- **Persisted** in two tiers (below), and it is the **authority on what survives a restart**. A
  segment's file becomes visible at `rename`, but the barrier that puts its blocks on the medium is
  paid later, by the manifest flush, which forces every queued file down *before* naming it. So a
  file alone proves nothing — after a power cut it can carry the right name and length over
  unwritten blocks. `DiskAssetStore` therefore treats an existing file as ready only when
  `availability` knows its final length; anything else is a torn write and is refetched. A missing
  or corrupt `availability.bin` costs a refetch, never a wrong read.
- **Reconciled** on hydration: an entry the store root no longer backs is dropped as the snapshot
  loads, so a pruned cache cannot come back claiming ranges nothing can serve. One `exists` per
  persisted resource, at startup only; `contains_range` stays lock-free and filesystem-free.

The persisted snapshot is a **committed-only** contract — an uncommitted partial write is never
serialised, so a flush racing a writer's cleanup cannot resurrect a partial segment whose `.tmp`
was never renamed. Live in-memory ranges still serve in-flight readers; only the snapshot filters.
kithara-storage keeps its own per-resource byte map, but it is an implementation detail: consumers
outside that crate must query through `AssetStore`, never through `contains_range()` on an ad-hoc
`open_resource` handle.

## Index persistence

| Index | File | Purpose |
|---|---|---|
| Pins | `_index/pins.bin` | Pinned asset roots (membership only; refcounts stay in memory) |
| LRU | `_index/lru.bin` | Monotonic clock + per-root byte accounting |
| Availability | `_index/availability.bin` | Committed ranges and final length per resource |

All three write through `Atomic<R>` and register with a shared `FlushHub` (`FlushPolicy` defaults:
50 ms debounce, 100 ms poll, forced flush every 256 signals). Two tiers:

- **Non-durable** best-effort: the background worker atomically renames without `sync_data`.
  Availability uses this tier for the snapshot itself because it is rewritten on every
  write/commit. The files it vouches for are a different matter: each availability flush first
  `sync_data`s the segment files committed since the last one, and only then writes the snapshot.
  That ordering is what lets a segment commit skip its own barrier (`Barrier::Deferred`) — the cost
  is paid once per flush instead of once per segment, off the resource-write path. Reversing it would let
  the manifest vouch for bytes that never landed.
- `AssetStore` checkpoints itself when its last handle drops. Every index holds the flush hub
  alive, so waiting for the hub's own teardown would find the sources already gone.
- **Durable** authoritative: `AssetStore::checkpoint()` → `FlushHub::flush_now` → `flush_durable`
  (`sync_data` before rename) over every source. It also re-flushes sources the worker last wrote
  non-durably even when their `dirty` flag is clear, so a checkpoint never leaves a stale worker
  snapshot behind.
- Pins and LRU never wait for the worker: their mutators call `flush_sync` eagerly, because a pin
  lost to a crash would let a live asset be evicted. That eager write bypasses the hub's flush lock,
  so each disk-backed index serialises its own file instead: the snapshot and the atomic rename that
  publishes it happen under one per-file lock. Without it a flush that snapshotted before an unpin
  could rename after it and resurrect a pin nobody holds — the asset then outlives every handle and
  eviction can never reclaim it. The filesystem stays the source of truth — any index may be missing
  and can be rebuilt.

### Pins index

- `PinsIndex` encapsulates its `Arc`, so `Clone` is a refcount bump. Each `asset_root` carries two
  refcounts, split by `PinDurability`. Both bar eviction identically — `snapshot` covers them
  together — and they differ only in reach: a `Durable` pin (an unfinished write) is persisted, a
  `Local` one (a reader holding committed bytes) is not. Only the durable count's 0→1 and 1→0
  transitions flush `_index/pins.bin`; everything else stays in memory, which is what keeps the
  decoder read loop off the disk. A `Local` pin is deliberately invisible to the next process: a pin
  that cannot outlive its holder would hydrate as a phantom that nothing ever releases.
- Persistence is lazy — the file materialises on the first flush — while an existing file from a
  previous run is opened and hydrated eagerly in `with_persist_at` (native only). On wasm32 the
  index is always ephemeral.
- Three call sites share one instance per disk root: `LeaseAssets` (pin/unpin), `EvictAssets` (reads
  the pinned set when picking candidates), `DiskAssetDeleter` (drops the pin on root removal).
- One live `AssetStore` per disk root in a process — the topology `README.md` states, and the reach
  of the per-file flush lock above. A second store over the same root is a second owner of
  `_index/pins.bin`: it hydrates its own copy in `with_persist_at`, so its next flush (or a worker
  checkpoint, which writes indices whose `dirty` flag is clear) republishes a snapshot taken before
  the other store's unpin and resurrects a pin nobody holds. It also keeps its own `EvictAssets`
  `seen` set, so eviction runs a second time against that stale pinned set. Neither is serialisable
  by a lock inside one instance. Sharing is by cheap clone of the handle, or by passing the built
  indices through the builder — never by two stores discovering each other through the filesystem.

### LRU index and eviction policy

- Eviction is evaluated only the first time an `asset_root` is observed in this process
  (`EvictAssets` keeps a `seen` set), and the newly opened root counts as pinned for that pass.
- Decisions read the in-memory `LruIndex` / `PinsIndex` snapshots; candidates are oldest-first,
  pinned roots excluded, and `max_assets` / `max_bytes` are soft caps enforced best-effort.
- Byte accounting is push-based — the evictor never walks the filesystem. `LeaseWriter::commit`
  measures the committed file and reports it through `ByteRecorder` into `LruIndex::update_bytes`,
  which is byte-only and never bumps recency: re-bumping per segment commit would coalesce every
  bytes-bearing asset to the same logical time and skew eviction order.
- `touch` always flushes; `update_bytes` flushes only when the total changed.

### Canonical deletion channel

Every path that physically removes bytes goes through `AssetDeleter` (`DiskAssetDeleter` /
`MemAssetDeleter`): own-asset teardown, resource-level removal, and foreign-asset LRU eviction
alike. Each method synchronises the storage-side change with every index reflecting it —
`delete_asset` clears the whole root's availability entries plus its pins and LRU entries,
`remove_resource` clears the single availability entry — so `AssetStore::remove_resource` must not
invalidate availability again. Bypassing this channel strands `contains_range` / `final_len` on
bytes that no longer exist, and the reader spins on `wait_range = Ready` / `read_at = Retry` until
the hang detector fires.

## Consumer demand

`PendingResourceIndex` is a sibling of `AvailabilityIndex`: one instance per `build()`, shared across store
clones via `Arc`. Availability answers "which bytes are present?"; the pending-resource index
owns the one not-yet-ready resource per `ResourceKey`: aggregate consumer demand, writer election
and epoch, the sole `AssetWriter`, and its shared reader. It remains protocol-agnostic (byte
offsets and cancellation, no HTTP) and needs no decorator threading. A protocol consumer joins
through `AssetStore::attach_pending_resource`; an active hit returns a reader and lease from the
existing pending resource without another cache acquire.

- `read_pos` is an `Arc<AtomicU64>` shared with the consumer, so the writer sees advances
  directly. Each entry also keeps a monotonic immediate requested-end floor. A bounded watermark
  is the maximum of that floor and `read_pos + look_ahead`, so a blocking read larger than the
  prefetch window cannot wait on bytes the writer was forbidden to fetch. `look_ahead = None`
  means "whole file" and collapses the watermark to `u64::MAX`.
- The cell mutex is the only source of truth for consumers, lifecycle, writer epoch, writer, and
  reader. The aggregate watermark is the maximum live consumer watermark.
- Dropping the elected handle invalidates its epoch immediately. A surviving lease can take the
  writer role without replacing the writer; raw writes and terminal operations validate the
  exact epoch while mutating the canonical session.
- The session cancel is a child of the store cancel and is the terminal election gate. Once it
  fires, current fetch descendants stop and no attachment or surviving lease can elect another
  writer epoch.
- The lock order is demand-map shard -> cell mutex -> cache/storage. Cache, availability, pins,
  writer cleanup, and lease cleanup never call back into `PendingResourceIndex`. No source-local, event, or
  user callback runs under these locks; the canonical write observer is limited to
  `AvailabilityIndex`. Audio readers hold only `AssetReader` clones and never take a demand lock.
- Reader and peer-poll `Waker` registrations are one-shot. A transition takes registered wakers
  under the cell mutex, releases both the cell and demand-map guard, then invokes them. The peer
  uses arm-check-recheck around demand and election: register before that check, leave the
  registration armed when it returns `Pending`, and clear only its exact waker after the recheck
  confirms readiness. An already in-flight protocol operation instead relies on its completion
  wake. Clearing a stale waker cannot remove a newer registration. A synchronous reader uses a
  non-owning `Weak` adapter to its worker wake and rearms after wake. Synchronous cancel callbacks
  follow the same outside-lock boundary.
- Last-consumer abandonment and terminal failure keep the exact map entry locked through outer
  removal, so a successor cannot publish until old bytes and live state are gone. Successful commit
  instead unpublishes the cell without removing backing bytes, then cancels requests from the old
  writer epochs; old leases become stale. If outer removal fails, the exact cell remains as a
  typed tombstone retaining the `ResourceKey` and shared source error; later attachments receive a
  fresh `PendingResourceCleanupError` carrier through the existing assets/storage error surface.
- A Pending writer transfers its legacy `LeaseWriter` abandonment hook to the pending-resource cell before
  publication. From that point the cell's typed outer removal is the sole cleanup owner for
  abandonment, failure, and commit error; no hidden first remove can precede a tombstone.
- The writer driver lives in the protocol crate (currently kithara-file), which speaks HTTP;
  the index hands it only `max_watermark`, the session cancel, and lease-scoped reader/peer wake
  registration. It belongs to the election, not to the spawning consumer. The election is sticky
  while the elected `WriterHandle` lives: dropping it clears the matching writer epoch and
  wakes surviving peer registrations, so a survivor's next `try_take_writer()` takes over - one
  writer at any instant, role migratable.
- The key is the `ResourceKey`, matching the granularity at which the store shares a resource.
  HTTP-response metadata (content length, codec) is **not** in the index: only the elected writer
  epoch sees response headers, so other consumers rely on availability, the committed `final_len`,
  and byte-probe codec detection.

## Resource transactions

`AssetStore::with_resource_transaction` serializes one read/validate/mutate operation per
`ResourceKey` across clones of the same store; the closure must re-read state after entering.
Cancellation releases or forwards the transaction to the next waiter. It is process-local,
ephemeral, and non-reentrant for the same store and key: it coordinates cache mutation, with no
rollback and no cross-process locking.

## Eviction subscription

`EvictionRouter` is the third consumer-driven sibling: one instance per `build()`, shared across
clones via `Arc`, so one store serves file and HLS with no protocol-owned wrapper. Subscribe with
`let guard = store.subscribe_eviction(asset_root, tx);`.

- The ephemeral build path wires the router into the cache's single `on_invalidated` hook: on
  volatile displacement the hook clears the `AvailabilityIndex` entry and routes the evicted key by
  its `asset_root`. Keys under another root are not delivered.
- Every subscriber for a root receives each eviction under it. The returned `EvictionSubscription`
  guard deregisters only its own registration on drop; absolute keys and unsubscribed roots no-op.
- Firing is gated on volatile displacement: active for `MemAssetStore`, where displacement frees
  bytes, and dormant for durable disk backings, where displaced bytes survive — the disk path wires
  no hook at all. There is no public callback and no builder field; the router reaches the cache
  only through the ephemeral path.
