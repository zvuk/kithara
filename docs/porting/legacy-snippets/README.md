# Curated legacy code snippets (for agents)

Agents do not have access to external repositories. This directory contains **curated, minimal “look at this” legacy code snapshots** to help you implement `kithara` correctly *without copying code 1:1*.

Rules:
- **Do not copy/paste blindly.** Use these files to understand *layer boundaries, contracts, and patterns*.
- **Do not change public contracts** of `kithara-*` crates to match legacy. Implement the behavior *inside* the existing architecture.
- Implement via **TDD-first**: add a deterministic test in the target `kithara-*` crate → make it pass.
- Prefer the normative portable specs in `kithara/docs/porting/*.md` if they differ from legacy. Legacy is reference, not law.

Related normative specs:
- `kithara/docs/porting/source-driver-reference.md`
- `kithara/docs/porting/hls-vod-basic-reference.md`
- `kithara/docs/porting/downloader-reference.md`
- `kithara/docs/porting/abr-reference.md`
- `kithara/docs/porting/drm-reference.md`
- `kithara/docs/porting/decode-reference.md`
- `kithara/docs/constraints.md`

---

## 1) Decal (decode layering reference code)

These snippets show how “decal-style” layering is structured: a small source abstraction + decoder module layout.

- `decal-lib.rs`
  - Entry point / feature gating / type wrappers.
  - Useful to see how the project separates “decoder” from other concerns.

- `decal-decoder-mod.rs`
  - Decoder module surface and how pieces are split.
  - Useful as a reference for keeping decode responsibilities isolated.

- `decal-decoder-source.rs`
  - The key abstraction: a “source” that provides a sync reader + optional hint.
  - Useful to align `kithara-decode`’s `Source`/`MediaSource` and avoid mixing async orchestration into decode.

What to apply to `kithara`:
- Preserve strict boundary: **sources/drivers** (HLS/file) produce async bytes; **decode** consumes sync `Read + Seek`.
- Keep Symphonia integration (“glue”) isolated in an engine module, with a clean state machine on top.
- Never return `Ok(0)` from a sync `Read` until true EOF (see `docs/constraints.md`).

---

## 2) stream-download-hls ABR (estimator/controller split)

These snippets show the legacy ABR decomposition: exported surface + controller + estimator + EWMA.

- `stream-download-hls-abr-mod.rs`
  - Re-exports that define the public ABR surface.

- `stream-download-hls-abr-controller.rs`
  - ABR decision logic orchestration.
  - Useful for: decision structure, switching policy, and where “manual vs auto” hooks live.

- `stream-download-hls-abr-estimator.rs`
  - Throughput estimation logic.
  - Useful for: sampling model and keeping estimator concerns separate from controller.

- `stream-download-hls-abr-ewma.rs`
  - EWMA primitive for smoothing.
  - Useful for: simple, testable smoothing and parameterization.

What to apply to `kithara-hls`:
- Keep ABR as **two components**:
  - estimator (network throughput samples only),
  - controller (hysteresis/min-interval/buffer gating).
- Make “decision vs applied” explicit (tests should prove applied semantics using bytes/prefixes).
- Ensure **cache hits never update throughput** (offline invariant).

---

## 3) stream-download-hls downloader (decorator/layered pattern)

These snippets show the legacy “downloader” composition approach (decorator pattern): base HTTP + retry + timeout + cache.

- `stream-download-hls-downloader-mod.rs`
  - Module exports and top-level API composition.

- `stream-download-hls-downloader-traits.rs`
  - Trait surface for a downloader and extension/composition helpers.
  - Useful for: keeping the surface minimal and composable.

- `stream-download-hls-downloader-types.rs`
  - Request/response/resource types.
  - Useful for: separating “resource identity” from fetch logic.

- `stream-download-hls-downloader-builder.rs`
  - Builder that assembles layers into a default stack.
  - Useful for: producing a convenient facade without losing testability.

- `stream-download-hls-downloader-base.rs`
  - Base HTTP implementation.
  - Useful for: explicit status handling and streaming output.

- `stream-download-hls-downloader-timeout.rs`
  - Timeout decorator.
  - Useful for: defining which stages are timed out and testing deterministically.

- `stream-download-hls-downloader-retry.rs`
  - Retry decorator.
  - Useful for: classification rules and “retry only before body streaming” semantics.

- `stream-download-hls-downloader-cache.rs`
  - Cache decorator.
  - Useful for: cache-first semantics and separating caching from HTTP.

What to apply to `kithara`:
- In `kithara`, **do not move caching into `kithara-net`**. Net is stateless.
- Apply the layered pattern *inside source crates*:
  - `kithara-hls`: cache-first playlists/segments/keys; later add transform layer for DRM decrypt.
  - `kithara-file`: stream + optional cache-through-write and offline replay.
- For timeouts/retries, follow `kithara/docs/porting/net-reference.md` (contract + test matrix).

---

## 4) How agents should use these snippets (practical workflow)

1) Identify the target kanban task in `kithara/docs/kanban-kithara-*.md`.
2) Read the matching spec:
   - HLS VOD basic playback: `docs/porting/hls-vod-basic-reference.md`
   - Downloader layering: `docs/porting/downloader-reference.md`
   - ABR: `docs/porting/abr-reference.md`
   - Decode layering: `docs/porting/decode-reference.md` + `docs/porting/decal-reference.md`
3) Use the snippets here only to answer “what does a good split look like?”.
4) Implement in the target crate via tests.
5) Mark kanban checkboxes as done (`[ ]` → `[x]`), run `cargo fmt`, run crate tests.

---

## 5) Notes / pitfalls

- These files are **not** compiled as part of `kithara`. They’re documentation artifacts.
- Avoid introducing new dependencies just because legacy used them. Check workspace deps first.
- If you discover that a needed contract is missing/unclear in kanban, stop and add/adjust the kanban entry (with repository owner confirmation).