# Decal reference (portable) — decode layering, integration boundary, and how it maps to legacy tests

> Актуально: orchestration loop вынесен в `kithara-stream`.
> - Источники (`kithara-file`, `kithara-hls`) реализуют `kithara-stream::Source` (что качать и в каком порядке).
> - `kithara-stream::Stream` владеет общим orchestration loop (`tokio::select!`), cancellation via drop и command contract.
> - “Stop” как отдельная команда **не является** частью контракта источников: остановка = **drop** consumer stream/session.

This document is a **portable “decal-style” reference artifact** for agents working on `kithara`.
Agents **do not have access** to the upstream `decal` repository, so this file explains:

- what “decal-style layering” means in practice for `kithara-decode`,
- where the boundary is between **sources/drivers** and **decode**,
- how this relates to legacy `stream-download-*` tests and behavior,
- what to test (and what not to test) to avoid regressions like false EOF.

This is **normative for kithara** where it overlaps with legacy expectations.

Related docs (read in this order for decode work):
1) `AGENTS.md`
2) `docs/constraints.md`
3) `docs/porting/decode-reference.md`
4) `docs/porting/source-driver-reference.md`
5) `docs/porting/net-reference.md`
6) (legacy snapshots for reference only)
   - `docs/porting/legacy-stream-download-hls-tests.rs`
   - `docs/porting/legacy-stream-download-lib.rs`

---

## 0) What “decal-style” means (in kithara terms)

“Decal-style” is not about copying code; it’s about **layer boundaries** and **contracts**:

### Layer A — Source abstraction (`Source` / `MediaSource`)
A small abstraction that gives the decode stack:

- a **sync** reader implementing `Read + Seek`,
- an optional file extension hint (e.g. `"mp3"`, `"m4a"`) to help probing.

In `kithara`, this must be compatible with:
- async sources (HLS, HTTP) via a bridge (`kithara-io`),
- local files or in-memory fixtures for deterministic tests.

### Layer B — Symphonia glue (`Engine`)
Encapsulates all Symphonia-specific details:
- probing,
- track selection,
- packet decode,
- format conversion,
- seek/reset behavior,
- mapping Symphonia errors into `DecodeError`.

No async here, no networking here, no orchestration here.

### Layer C — Decoder state machine (`Decoder<T>`)
Provides a clean, player-grade state machine around the engine:
- `next() -> Result<Option<PcmChunk<T>>, DecodeError>`
- command handling (at minimum: `Seek(Duration)`),
- strict EOS semantics (see below),
- no deadlocks/hangs.

### Layer D — Pipeline / worker (`AudioStream` or equivalent)
Connects sync decode with async consumers:
- separate worker thread/task that drives `Decoder<T>`,
- bounded queue for backpressure,
- command channel,
- deterministic termination and cancellation.

---

## 1) The critical boundary: sources/drivers vs decode

### Sources/drivers (kithara-file, kithara-hls)
Responsibilities:
- implement resource logic as `kithara-stream::Source` (what to fetch and in what order),
- network I/O, caching, resource orchestration, HLS playlist parsing, segment selection,
- DRM key fetching + decrypt (for HLS),
- ABR decision/apply (for HLS),
- emitting an **async byte stream** (data-plane) and closing it on EOS/fatal error,
- stopping when the consumer drops the stream/session (cancellation via drop).

Note:
- the generic orchestration loop (selecting between data-plane and control-plane, bounded channels, drop-driven cancellation) is owned by `kithara-stream::Stream`.

Non-responsibilities:
- decoding audio formats,
- dealing with Symphonia.

### Decode (kithara-io + kithara-decode)
Responsibilities:
- bridge async byte stream → sync `Read + Seek`,
- enforce correct `Read` semantics for Symphonia,
- decode bytes into PCM and provide a stable consumer API.

Non-responsibilities:
- fetching resources or touching the network,
- HLS orchestration logic.

This separation prevents “network in audio thread” and keeps behavior testable.

---

## 2) The non-negotiable invariant: `Read::read() -> Ok(0)` means **EOF**

This is the most important practical detail from prior failures:

- Many consumers (including Symphonia) treat `Ok(0)` as “true end of stream”.
- Returning `Ok(0)` when “no bytes available yet” causes premature termination.

**Normative rule for kithara:**
- the sync reader used by Symphonia must return `Ok(0)` **only** when upstream is truly finished and all buffered data is drained.
- if upstream is not finished but no data is currently available, `read()` must **block** on the decode worker thread.

This rule is defined in `docs/constraints.md` and must remain true even as we refactor.

---

## 3) How decode layering maps to legacy tests

Legacy `stream-download` had “Read + Seek over async” and many tests around:
- seek correctness across segment boundaries,
- reading known offsets,
- ensuring the stream drains and closes.

In `kithara`, the mapping is:

### Legacy “stream download read/seek tests” → `kithara-io` + `kithara-decode`
- The seek/read correctness tests belong primarily to the bridge (`kithara-io`) and decoder state machine (`kithara-decode`).
- The source (HLS/file) should be tested for producing correct bytes and closing correctly, but **not** for implementing the sync `Read` itself.

### Legacy “HLS VOD completes and channel closes” → `kithara-hls`
- VOD completion (segment loop reaches end) is a source/driver concern.
- Decode tests should consume from a deterministic source but should not re-test HLS orchestration.

### Legacy “variant changes / ABR” → `kithara-hls` (plus decoder reset behavior)
- HLS tests validate variant switching decisions and applied semantics.
- Decode tests validate that when codec changes happen (if your fixture models that), the decoder can reset/reopen and continue producing PCM without deadlocks.

---

## 4) What to test in `kithara-decode` (portable, deterministic)

You should add tests that fix **contracts**, not implementation details:

1) **EOS semantics**
- decoding a finite input eventually yields `Ok(None)` (or the crate’s equivalent EOS signal).
- no hang on EOS.

2) **Seek command does not deadlock**
- multiple seek commands + decode calls complete within timeouts.
- seek is best-effort, but must be safe.

3) **PCM invariants**
- `PcmChunk<T>` always frame-aligned, valid `channels/sample_rate`, interleaved layout.

4) **Codec reset handling (if modeled)**
- if the engine indicates “reset required”, decoder handles it as defined by kithara contract:
  - either surfaces a typed error requiring reopen,
  - or does the reopen internally and continues.
- whichever is chosen must be fixed by tests and documented.

Avoid:
- tests that depend on external network,
- tests that assume “no retries” or “no refetch” unless contract explicitly says so,
- tests that rely on event ordering across data-plane unless applied semantics are explicit.

---

## 5) How to test integration without mixing responsibilities

To keep “one change — one crate” and avoid cross-crate entanglement:

### For `kithara-decode`
Use deterministic local fixtures:
- tiny embedded audio assets (mp3/aac) or a local in-process server under tests,
- a fake `MediaSource` / `Source` implementation that returns a `Cursor<Vec<u8>>` for unit tests,
- no HLS orchestration in decode tests.

### For `kithara-hls`
Use deterministic byte payload prefixes per segment to validate:
- which variant is being streamed,
- that segments are fetched and emitted,
- that EOS is reached.

Then separately ensure decode can consume the produced bytes via `kithara-io` in higher-level tests or examples (but don’t make decode depend on HLS internals).

---

## 6) Practical checklist for agents (decode work)

When implementing “decal-style layering” changes:

1) Re-read:
- `docs/constraints.md` (EOF/backpressure rules)
- `docs/porting/decode-reference.md` (layer responsibilities and test plan)

2) Ensure these boundaries exist and stay small:
- `types` (public types/errors/traits)
- `symphonia_glue` (Symphonia-only)
- `engine` (low-level decoding)
- `decoder` (state machine)
- `pipeline` (worker + queue + commands)

3) Add/extend tests:
- EOS termination
- seek no deadlock
- PCM invariants

4) Run formatting/tests per agent rules:
- mark completed kanban checkboxes `[x]`
- `cargo fmt`
- `cargo test -p kithara-decode`
- `cargo clippy -p kithara-decode`

---

## 7) Anti-patterns (what breaks the architecture)

- Doing networking or HLS orchestration inside `Read::read()` or inside the decode worker.
- Returning `Ok(0)` from `Read` while expecting “more bytes later”.
- Mixing “decision” and “applied” events as if ordering is guaranteed by the data stream.
- Making decode tests depend on HLS fixtures (that belongs to `kithara-hls` tests).

---

## 8) Summary

“Decal-style” in `kithara` is a strict separation of:
- **sources/drivers** (async resource orchestration, caching, DRM/ABR; produce async bytes)
- **bridge + decode** (sync `Read+Seek` with correct EOF semantics; decode to PCM)

If you preserve the layer boundaries and enforce the `Ok(0) == EOF` invariant, you can iterate quickly:
- first make HLS VOD segment streaming work (plaintext fixtures),
- then wire decode through the bridge,
- then add DRM/ABR without rewriting the architecture.
