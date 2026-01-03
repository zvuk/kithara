# `kithara-assets` (contract-first)

`kithara-assets` is Kithara’s persistent on-disk **assets store**. It stores a *tree of logical
resources* under a single root directory and exposes them through a small, explicit public contract.

## Public contract (normative)

The only public contract of this crate is the `Assets` trait in `crates/kithara-assets/src/lib.rs`.
Everything else is an implementation detail.

### `Assets`

Resource opening:
- `open_streaming_resource(key, cancel) -> CacheResult<AssetResource<StreamingResource>>`
- `open_atomic_resource(key, cancel) -> AssetResource<AtomicResource>`

Global index helpers (best-effort metadata; filesystem is the source of truth):
- `open_index_resource(cancel) -> AssetResource<AtomicResource>`
- `load_index(cancel) -> CacheResult<AssetIndex>`
- `store_index(cancel, &AssetIndex) -> CacheResult<()>`

Cancellation:
- all “open” operations take a `tokio_util::sync::CancellationToken` that is forwarded to the
  underlying on-disk resource.

## Key layout and disk mapping

Keys are chosen by higher layers (`kithara-file`, `kithara-hls`):

- `ResourceKey { asset_root: String, rel_path: String }`

Logical-to-disk mapping:
- `<cache_root>/<asset_root>/<rel_path>`

### Safety rules (normative)

`kithara-assets` does not “invent” keys, but it *must* prevent path traversal:
- no absolute paths,
- no `..`,
- no empty path segments.

Current mapping behavior:
- when `asset_root` or `rel_path` violates these rules, it is not used verbatim; instead a stable
  SHA-256 hex string of that input is used as the corresponding path component.

## `AssetResource<R>` auto-pin semantics

All resources opened through `Assets` are returned as `AssetResource<R>`.

Normative semantics:
- opening any resource pins its `asset_root` for the lifetime of the returned handle,
- pinning is keyed by `asset_root` (not by `rel_path`),
- dropping the handle releases the pin.

Implementation note (observable behavior):
- unpin on drop is best-effort and asynchronous; callers should not rely on the pin count changing
  immediately after `drop`.

## Atomic vs streaming resources

This crate delegates the actual I/O semantics to `kithara-storage` and only decides *which* resource
type to open:

- `AtomicResource` (small objects): playlists, keys, metadata, index files.
  - whole-object `read()` / `write(data)`
  - crash-safe replace (temp file → rename)
- `StreamingResource` (large objects): segments or other large media resources.
  - random-access `write_at(offset, data)` / `read_at(offset, len)`
  - availability is coordinated via `wait_range(range)`

Which layers use which:
- `kithara-hls` typically uses atomic resources for playlists/keys and streaming resources for
  segments.
- `kithara-file` typically uses streaming resources for progressive downloads and atomic resources
  for small metadata.

## Global index: `_index/state.json` (best-effort)

The “global index” is a conventional resource in the same store:
- `asset_root = "_index"`
- `rel_path = "state.json"`

Normative properties:
- it is not a source of truth (filesystem is),
- it is not held open; it is opened on demand as an `AtomicResource`,
- it may be missing and can be rebuilt from the filesystem.

Rebuild strategy (intended):
- scan `<cache_root>/**` excluding `_index/**`,
- derive entries from file metadata (paths and sizes),
- default to conservative statuses (treat unknown as “in progress”/not-safe-to-evict).
