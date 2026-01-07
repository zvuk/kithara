# Kithara: Constraints & Lessons Learned

This document captures the "constitution" of the project: constraints, principles, and mistakes we've already encountered. The goal is to avoid repeating these mistakes during `kithara` development.

## 1) What is `kithara` (boundaries)

`kithara` is a **networking + resource orchestration + decoding** library, but **not a full player**:
- Provides ability to get PCM (through decoding layer)
- Works with progressive HTTP (MP3, etc.) and HLS VOD
- Has mandatory persistent disk assets store (for offline)
- Built around a *resource-based* data model: inside `kithara` we work not with "byte streams" but with **logical resources** addressable by keys

Not within scope:
- UI, playlists, output device management, mixing (host application responsibility)
- HLS Live (not currently required, but design shouldn't block adding it later)
- "Stop command" as mandatory stop mechanism: stop = drop session

### 1.1) Layer roles (normative)

- `kithara-net`: HTTP client (timeouts/retries/headers/range)
- `kithara-storage`: Storage primitives for **single** resource:
  - `AtomicResource`: Small files (playlist, keys, metadata) — whole-object `read`/`write` with atomic replace (temp → rename)
  - `StreamingResource`: Large files/segments — random-access `write_at`/`read_at` + `wait_range` for waiting on range availability
- `kithara-assets`: Manager of **resource trees on disk**:
  - Stores resource trees under a single root (`root_dir`)
  - Provides base contract `Assets` for opening *raw* resources (`AtomicResource` / `StreamingResource`) by key
  - Lease/pin implemented as **decorator** (`LeaseAssets`) over `Assets`
  - Keys provided by external layers (assets doesn't "invent" paths)
- `kithara-stream`: Stream orchestration with async-to-sync bridge functionality (includes former `kithara-io` concerns)
  - Provides sync `Read+Seek` over `StreamingResource` via `wait_range` (no "false EOF")
  - Generic byte-stream orchestration with backpressure
- `kithara-file` / `kithara-hls`: Orchestration: which resources to download and how to fill `StreamingResource` (range requests), how to cache small objects via `AtomicResource`

## 2) Architectural reason "why it was painful before"

HLS by nature is a **tree of resources**:
- master/media playlists
- init segments
- media segments
- keys/DRM
- variant switching (ABR/manual)
- discontinuity

Attempts to "force" HLS into a bytes-only model ("one file/one byte stream") lead to:
- Loss of boundary semantics
- Complex seek behavior
- Hard-to-test variant/codec transitions
- Undesirable compromises

**Conclusion (normative):** In `kithara`, HLS remains "resource-based" internally. "Byte stream" is just a representation over resources (usually through `kithara-stream`), but the public contract for the decoder is sync `Read+Seek`, not `Stream<Item=Bytes>`.

## 3) Critical `Read` contract: `Ok(0)` == EOF

We've already caught bugs where `Read::read()` returned `Ok(0)` not as true EOF, but as "no data yet".
For the vast majority of consumers (including Symphonia) `Ok(0)` means:
- End of file/stream
- Further reading stops

**Rule (normative):**
- `Read::read()` returns `Ok(0)` only for proven End-Of-Stream
- If data isn't available yet (and resource will be written to later), `Read::read()` must **block** on decode thread, with waiting implemented via `StreamingResource::wait_range` (with cancellation)

## 4) Async networking + sync decoding: thread separation

Network layer must be async (usually `tokio`), but Symphonia requires sync `Read + Seek`.
Cannot perform network/orchestration in audio-thread.

**Rule (normative):**
- There's a separate decode thread (or worker) that synchronously reads bytes and decodes
- Async tasks download data and **write** it to `StreamingResource` via `write_at` (including out-of-order by range)
- Sync reading for decoder implemented via `kithara-stream`, which blocks on `wait_range` (and never returns "false EOF")

## 5) Events: "decision" ≠ "applied"

Out-of-band events (telemetry) don't guarantee ordering relative to data.
We've already encountered situations where:
- `VariantChanged` could mean ABR decision, but not necessarily applied in decoding

**Rule:**
- If event is used for strict logic (e.g., decoder reinit on codec/variant change), it must be tied to actual application and have clear semantics
- Telemetry allows out-of-band, but strict invariants cannot be built on it

## 6) ABR: cache hits shouldn't "improve" throughput

If estimator is updated with cache-hit, ABR starts thinking network is fast and selects variant not in cache. This breaks offline.

**Rule:**
- throughput/bandwidth estimator updated only by real network downloads
- cache-hit doesn't affect ABR

ABR policy should be parameterizable (similar to previous `stream-download-hls`).

## 7) Assets store: persistent, tree-friendly, crash-safe

Persistent disk storage is mandatory and must survive restarts. HLS requires tree-like layout.

Requirements (normative):
- Persistent disk assets store (not "memory-only")
- Cache everything needed for offline playback: playlists, segments, keys
- Storage is tree of files under cache root:
  - Path formed as `<root_dir>/<asset_root>/<rel_path>`
  - `asset_root` and `rel_path` provided by external layers (e.g.: `<asset_hash>/<path-from-playlist>`)
  - Assets layer must prevent path traversal (`..`, absolute paths)
- Resource opening must preserve "resource" model:
  - Small objects: `AtomicResource` (whole-object `read`/`write` with atomic replace)
  - Large objects/segments: `StreamingResource` (`write_at`/`read_at` + `wait_range`)
- Writing small objects (playlist, keys, metadata) must be crash-safe:
  - temp file → rename (atomic replacement) via `AtomicResource::write`
- Writing large objects/segments supports immediate-read semantics:
  - `StreamingResource::write_at` + `wait_range` (without waiting for "download entire file")
- Eviction and lease:
  - Limit by total cache size (bytes)
  - LRU by asset
  - Active asset pinned/leased (not eligible for eviction)
  - Eviction must not delete resources being filled (in-progress)

Note about current `kithara-assets` implementation (actual reality):
- Base contract `Assets` doesn't implement lease/pin; lease/pin is decorator `LeaseAssets` over `Assets`
- Pin table persisted best-effort via `AtomicResource` as static meta-resource in namespace `_index/*` (e.g., `_index/pins.json`), FS remains source of truth

## 8) Identification: `asset_id` without query, `resource_hash` with query

Reason: query can be dynamic (especially for keys/DRM), but track should be considered one entity.

**Rule:**
- `asset_id` built from canonical URL **without query/fragment**
- `resource_hash` (for specific resource: segment/playlist/key) built from canonical URL **with query**, without fragment

## 9) DRM keys: cache processed keys

For offline, it's important to store keys in decryptable form:
- Key may arrive "wrapped" and be unwrapped via `key_processor_cb`

**Rule:**
- Cache stores keys after `key_processor_cb` processing (processed keys)
- Don't log keys/secrets (only hash/fingerprint allowed)

## 10) Tests (TDD): what to test and what to avoid

We've seen tests that are easily "green" or tied to random behavior.

**Rule (normative):**
- TDD-first: write tests before implementation
- Tests deterministic, without external network
- Distribute tests by responsibility layer:
  - `kithara-storage`:
    - `AtomicResource`: atomic replace semantics, read/write small objects
    - `StreamingResource`: `wait_range` (with cancellation), `write_at`/`read_at`, EOF semantics after `commit(Some(final_len))`
  - `kithara-assets`:
    - Mapping `<root>/<asset_root>/<rel_path>`, traversal protection
    - Lease/pin as decorator over `Assets` with best-effort persisted pin table via `AtomicResource` (`_index/pins.json`)
    - Best-effort metadata in namespace `_index/*` (can be deleted/lost; FS remains source of truth)
  - `kithara-stream` / `kithara-decode`:
    - `Read`/EOF semantics (`Ok(0)==EOF`)
    - Seek safety (arbitrary seek during playback)
    - No deadlock while waiting for data (`wait_range`)
  - `kithara-file` / `kithara-hls`:
    - Orchestration: which range requests we make, how we fill resources, cancellation via token/drop
    - Offline behavior over persistent assets store
- Avoid tests building strict invariants on "zero network requests" without explicit contract (revalidation/ttl)

## 11) Comments and documentation

Long code comments quickly become outdated.

**Rule:**
- Code comments: maximum single-line, only when necessary
- Architecture explanations and contracts — in crate `README.md` or `docs/`

## 12) Workspace-first dependencies

All dependency versions set in workspace root `Cargo.toml` (`[workspace.dependencies]`),
and in subcrates referenced via `{ workspace = true }` (including dev/build).

See also `AGENTS.md`.

## 13) Crate consolidation notes

### Former `kithara-core`
- Functionality merged into `kithara-assets`
- Identity types (`AssetId`) and error definitions now part of assets layer

### Former `kithara-io`
- Bridge functionality integrated into `kithara-stream`
- Async-to-sync concerns handled within stream orchestration layer
- `Read + Seek` interface provided via `kithara-stream::io::Reader`

See [`crate-consolidation.md`](crate-consolidation.md) for detailed migration guidance.

## Related Documents

- [`AGENTS.md`](../AGENTS.md) — Development rules for autonomous agents
- [`architecture.md`](architecture.md) — System architecture overview
- [`crate-consolidation.md`](crate-consolidation.md) — Crate consolidation details
- Individual crate `README.md` files for public contract documentation