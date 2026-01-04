# `kithara-assets` (contract-first)

`kithara-assets` is Kithara’s **assets** abstraction: an *asset* is a logical unit addressed by
`asset_root` that may consist of multiple *resources* (files) addressed by `rel_path`.

Examples:
- MP3: one resource (e.g. `media/audio.mp3`) under an `asset_root`
- HLS VOD: many resources (playlist(s), segments, keys) under one `asset_root`

This crate is not “about opening files” in general. It is about **encapsulating asset resources**
behind an explicit contract and (optionally) **pinning assets** while resource handles are alive.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

- `trait Assets` — base abstraction: open resources by `ResourceKey`
- `struct LeaseAssets<A>` — decorator that adds pin/lease behavior to a base `Assets`
- `type AssetStore = LeaseAssets<DiskAssetStore>` + `fn asset_store(...)` — convenient “ready-to-use”
  composition for disk-backed usage
- `struct DiskAssetStore` — concrete on-disk implementation of `Assets`
- `struct ResourceKey` — resource identifier (`asset_root`, `rel_path`)
- `struct AssetResource<R, L = ()>` — resource handle decorator used by the lease system

Everything else should be treated as an implementation detail.

## `Assets` trait

`Assets` is the base contract. It does **not** implement pinning/leases. It is only responsible for
mapping a `ResourceKey` to underlying storage primitives.

Resource opening:
- `open_atomic_resource(key, cancel) -> CacheResult<AtomicResource>`
- `open_streaming_resource(key, cancel) -> CacheResult<StreamingResource>`

Static metadata resource:
- `open_static_meta_resource(key, cancel) -> CacheResult<AtomicResource>`

`open_static_meta_resource` exists so decorators (like `LeaseAssets`) can persist small state without
this crate performing direct filesystem I/O. This resource must be stable for the lifetime of the
assets instance and must be excluded from pinning by decorators to avoid recursion.

Cancellation:
- all “open” operations take a `tokio_util::sync::CancellationToken` that is forwarded to the
  underlying resource.

## `LeaseAssets<A>` (pin/lease decorator)

`LeaseAssets` is a decorator over a base `Assets` implementation. It returns `AssetResource` handles
that keep an RAII lease guard alive for the lifetime of the handle.

Normative semantics:
- opening any resource through the decorator pins its `asset_root` for the lifetime of the returned
  handle,
- pins are keyed by `asset_root` (not by `rel_path`),
- pin table in memory is a `HashSet<String>` (unique pinned roots; no refcounts),
- each pin/unpin immediately persists the full table to disk (best-effort) by writing JSON through a
  static meta `AtomicResource`.

Persistence details (current behavior):
- the pin table is stored as JSON at `ResourceKey { asset_root: "_index", rel_path: "pins.json" }`
  via `Assets::open_static_meta_resource(...)`,
- writes use `kithara-storage` atomic semantics (temp → rename).

Drop behavior:
- unpin-on-drop is best-effort and asynchronous; do not rely on the on-disk table updating
  immediately after `drop`.

## Ready-to-use disk store

For a convenient default composition, this crate provides:

- `DiskAssetStore` — disk-backed `Assets` implementation
- `AssetStore` — type alias for `LeaseAssets<DiskAssetStore>`
- `asset_store(root_dir) -> AssetStore` — constructor function

This yields a store where:
- mapping and disk I/O are handled by `DiskAssetStore`,
- pinning and persistence are handled by `LeaseAssets`.

## Key layout and disk mapping

Keys are chosen by higher layers (`kithara-file`, `kithara-hls`):

- `ResourceKey { asset_root: String, rel_path: String }`

Logical-to-disk mapping (for `DiskAssetStore`):
- `<root_dir>/<asset_root>/<rel_path>`

### Safety rules (normative)

`kithara-assets` does not invent keys. However, disk-backed implementations must prevent path
traversal:
- no absolute paths,
- no `..`,
- no empty path segments.

Current `DiskAssetStore` behavior:
- validates `asset_root` and `rel_path` against traversal rules,
- if validation fails, falls back to a stable SHA-256 hex representation for that path component to
  preserve safety.

## Atomic vs streaming resources

This crate delegates actual I/O semantics to `kithara-storage`:

- `AtomicResource` (small objects): playlists, keys, metadata blobs
  - whole-object `read()` / `write(data)`
  - crash-safe replace (temp file → rename)
- `StreamingResource` (large objects): segments or other large media resources
  - random-access `write_at(offset, data)` / `read_at(offset, len)`
  - availability is coordinated via `wait_range(range)`

Typical usage:
- `kithara-hls`: atomic for playlists/keys; streaming for segments
- `kithara-file`: streaming for progressive downloads; atomic for small metadata

## Best-effort metadata (“_index” namespace)

The `"_index"` `asset_root` is reserved by convention for small internal metadata files. Today,
`LeaseAssets` uses:

- `_index/pins.json` — persisted pin table (best-effort)

Filesystem remains the source of truth; metadata may be missing and can be rebuilt by higher layers
if needed.