# Downloader reference spec (portable) — decorator/layered pattern for resource fetching (HLS + File)

This document is a portable reference for autonomous agents implementing real, testable downloading behavior
in `kithara`, inspired by the downloader/decorator architecture in legacy codebases.

Goal:
- Provide a clear *pattern* for building download stacks (base HTTP + retry + timeout + cache + transform).
- Keep responsibilities separated and testable.
- Make it easy to “wire” a correct driver loop in `kithara-hls` and `kithara-file` without changing public contracts.

This document complements:
- `docs/constraints.md` (EOF/backpressure, offline, identity, ABR cache-hit rules)
- `docs/porting/source-driver-reference.md` (driver loop model, cancellation via drop)
- `docs/porting/net-reference.md` (timeouts/retries semantics and test matrix)
- `docs/porting/hls-vod-basic-reference.md` (HLS VOD basic playback checklist)
- `docs/porting/drm-reference.md` (AES-128, processed keys caching)
- `docs/porting/abr-reference.md` (ABR estimator/controller, decision vs applied)
- Copied legacy artifacts:
  - `docs/porting/legacy-stream-download-hls-lib.rs`
  - `docs/porting/legacy-stream-download-hls-tests.rs`
  - `docs/porting/legacy-stream-download-lib.rs`

---

## 0) Normative rules

1) **No public contract changes**
- Do not rename/remove/add public types/functions/fields in `kithara-*` just to fit legacy patterns.
- All improvements should happen via internal modules and tests.

2) **Stateless net**
- `kithara-net` stays stateless (no persistent caches, no HLS awareness).
- Caching policy is owned by `kithara-file` / `kithara-hls` using `kithara-cache`.

3) **Offline mode**
- `offline_mode=true`: any cache miss for required resources must be a fatal error.
- No network attempt in offline mode.

4) **Throughput and ABR**
- ABR throughput estimator must be updated only by real network downloads.
- Cache hits do not improve estimator.

5) **Cancellation via drop**
- “Stop” is not the primary mechanism. If the consumer drops the stream/session, the driver loop must terminate.

6) **No `unwrap`/`expect`**
- All production code must use typed errors with context.

---

## 1) Why we need a downloader pattern in `kithara`

Both `kithara-file` and `kithara-hls` need “resource fetch” behavior that combines:
- base HTTP GET (streaming and/or buffered),
- explicit status handling,
- timeouts and retries with clear semantics,
- caching (persistent disk),
- optional transforms (DRM decrypt, byte rewrite, validation),
- deterministic observability for tests (request counters, error mapping, offline rules).

If this logic is implemented ad-hoc inside driver loops, it tends to:
- become untestable,
- violate offline rules,
- mix concerns (HLS parsing + net + cache + crypto),
- regress on edge-cases (timeouts, partial failures, “fake EOF”).

A decorator/layered downloader makes each concern local and testable.

---

## 2) Portable model: “resource fetch pipeline” with layers

Think of resource fetching as a stack of layers around a base HTTP client.

### 2.1 Core interfaces (conceptual)

A minimal interface used by `kithara-hls` and `kithara-file` drivers:

- **Fetch bytes (buffered)**:
  - best for small resources: playlists, keys
- **Fetch bytes (streamed)**:
  - best for large resources: media segments, progressive files
- **Fetch range**:
  - used for seek where applicable

Conceptual request metadata:
- `url`
- optional `headers`
- optional `query params` (usually only for key fetch in HLS)
- `offline_mode`
- `cache_policy` (cache-first vs network-first; typically cache-first in kithara)

Conceptual response metadata:
- `bytes_source`: `Network` or `Cache` (critical for ABR and offline tests)
- `content_length` (optional)
- `etag/last_modified` (optional, v2)

### 2.2 The layers

A typical stack:

1) **Base HTTP layer**
- Uses `kithara-net` to do actual requests.
- Responsible for:
  - status handling (4xx/5xx are errors),
  - returning a byte stream or buffered bytes,
  - mapping errors with URL + stage context.
- Not responsible for caching, offline rules, ABR accounting, crypto.

2) **Timeout layer**
- Applies timeouts according to `net-reference.md`.
- Must be deterministic in tests.

3) **Retry layer**
- Retries only before body streaming begins (v1).
- Classification rules fixed by tests.

4) **Cache layer (in source crate, not in net)**
- Implements cache-first semantics:
  - if cached => return cached bytes (`bytes_source=Cache`)
  - else => fetch via lower layer (`bytes_source=Network`) and store crash-safely
- Offline mode enforcement:
  - if offline and miss => fatal OfflineMiss immediately
- Must not leak secrets (keys) to logs or filenames.

5) **Transform layer (optional)**
- Applied *after* bytes are fetched:
  - DRM decrypt for segments
  - key processing for keys (but key processing is often modeled as part of the KeyManager)
  - validation (prefix checks in tests, length checks for keys)
- Must preserve error typing and include context.

### 2.3 Why “cache layer in source crate”
Because:
- HLS caching is tree-shaped and uses `asset_id/resource_hash` rules.
- File caching might be a different layout and may require different policies.
- Keeping caching in `kithara-hls`/`kithara-file` avoids pushing HLS/file-specific rules into `kithara-net`.

---

## 3) Resource taxonomy: what gets fetched and how

### 3.1 HLS resources
- Master playlist: small, buffered fetch, cache-first
- Media playlist: small, buffered fetch, cache-first
- Init segment: medium, can be buffered or streamed, cache-first
- Media segments: large, streamed fetch preferred, cache-first
- Key resources: small, buffered fetch, cache-first (processed keys stored)

Each resource should have a deterministic cache key:
- Within an asset tree (keyed by `asset_id`)
- Resource key (`resource_hash`) includes query for keys/segments if applicable

### 3.2 File resources
- Progressive file bytes: streamed fetch, optionally cached through write
- Range fetch: for seek (if contract supports it)

---

## 4) Deterministic testing: what you must expose (internally)

To make tests deterministic without changing public API:
- Use test-only fixtures (`tests/` or `cfg(test)` helpers) that:
  - serve resources under known paths,
  - provide per-path request counters,
  - can simulate:
    - fail-then-succeed (for retry),
    - delayed headers (for timeout),
    - mid-stream drop (for no mid-stream retry),
    - 404/403/500,
    - “segments remapped under base_url”.

Within the downloader/caching layer:
- Ensure the driver can assert:
  - “all segments were fetched at least once” via fixture counters
  - “key fetched <= 1” via counters
  - offline mode: no network calls (or at minimum, offline miss fails before network)

---

## 5) Concrete “portable wiring” guidelines for `kithara-hls` and `kithara-file`

### 5.1 `kithara-hls`: VOD driver uses fetch stack
The driver loop should *not* contain raw HTTP code; it should call:
- `playlist_manager.fetch_master_playlist()`
- `playlist_manager.fetch_media_playlist()`
- `fetch_manager.fetch_segment_bytes(...)` or `stream_segment_bytes(...)`
- `key_manager.get_key(...)` (for keys), which itself uses net+cache rules

Then a transform hook:
- if encryption active => decrypt segment bytes
- else passthrough

But to keep DRM non-blocking for basic playback:
- Basic fixture should be plaintext (no EXT-X-KEY).
- Still, the code must have an explicit place where decrypt will be applied later:
  - “if key present then decrypt else passthrough”
- If EXT-X-KEY appears unexpectedly and decrypt isn’t implemented, fail deterministically with a typed error.

### 5.2 `kithara-file`: file driver uses fetch stack
The file driver loop should:
- stream bytes from net via base+retry+timeout
- optionally cache-through-write (if cache enabled and part of the contract)
- close stream on EOS
- terminate promptly on receiver drop (cancellation via drop)

---

## 6) Error handling contract (portable)

Downloader layers must preserve typed errors and attach context:
- URL
- resource kind (playlist/segment/key/file/range)
- stage (request/headers/body/cache read/cache write/transform)

Recommended outcome mapping:
- Offline miss => a dedicated error (`OfflineMiss`) and immediate termination
- Cache read/write errors => surfaced as cache errors with path context
- Transform errors (decrypt/key processing) => fatal errors with resource URL context

---

## 7) Minimal “downloader test matrix” (portable)

These are not necessarily all new tests; they are the tests that define correctness.

### 7.1 For `kithara-hls` (basic playback, DRM-aware)
- `hls_vod_completes_and_stream_closes`
- `hls_vod_fetches_all_segments_for_selected_variant`
- `hls_manual_variant_outputs_only_selected_variant_prefixes`
- `hls_drop_cancels_driver_and_stops_requests`
- `hls_offline_miss_is_fatal` (if offline_mode is part of the contract)
- Later (DRM):
  - `aes128_decrypt_smoke_plaintext_prefix`
  - `processed_key_cached_not_fetched_per_segment`

### 7.2 For `kithara-file`
- `file_stream_downloads_all_bytes_and_closes`
- `file_receiver_drop_cancels_driver`
- `file_offline_replays_from_cache` (if supported)
- `file_offline_miss_is_fatal` (if supported)
- `seek_roundtrip_correctness` (when seek contract is fixed)

### 7.3 For `kithara-net` (supporting contract)
- No mid-stream retry
- Timeout matrix
- Range “200 vs 206” defined behavior

---

## 8) Implementation checklist for agents (per crate, no conflicts)

### 8.1 `kithara-hls` (first priority)
1) Implement fixture mock mode with:
   - deterministic paths for master/media/segments
   - per-path counters
   - deterministic segment prefixes
2) Wire VOD driver: master → media → segment loop → EOS
3) Ensure segment bytes are emitted (not playlists)
4) Cancellation via drop test
5) Keep DRM hook present but use plaintext fixture for basic playback

### 8.2 `kithara-file` (parallel, second priority)
1) Ensure driver streams full content and closes
2) Cancellation via drop test
3) Cache-through-write and offline replay (if part of contract)

### 8.3 DRM and ABR (after basic playback)
- DRM decrypt pipeline + tests
- ABR estimator/controller + decision/applied + tests

---

## 9) Notes on “decal artifacts” (where they fit)

The downloader pattern described here is intentionally orthogonal to decode layering.
Decode layering (“decal-style”) lives in:
- `docs/porting/decode-reference.md`

The critical integration boundary:
- sources/drivers emit async byte streams,
- `kithara-io` bridges to sync `Read+Seek` with correct EOF semantics,
- `kithara-decode` consumes sync bytes and outputs PCM.

Downloader layers must never violate constraints that would break decode:
- no fake EOF,
- deterministic termination,
- bounded buffering.

---

## 10) Anti-patterns to avoid

- Fetching playlists and then stopping (no segment loop).
- Emitting playlist bytes into the media byte stream.
- Doing caching or HLS-specific logic inside `kithara-net`.
- Treating cache hits as network throughput updates.
- Returning `Ok(0)` from a `Read` bridge “temporarily”.
- Relying on “Stop” command instead of drop-driven cancellation.

---