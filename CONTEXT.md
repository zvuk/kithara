# Kithara — Project Context

The cross-crate architectural narrative for the Kithara audio engine: how the
pieces fit together and where the contracts that span crate boundaries live.
This file explains the *system*; it does not restate rules. For repo-wide
invariants, conventions, and coordination shapes see [`AGENTS.md`](AGENTS.md);
for the test/flash model see [`TESTING.md`](TESTING.md); for per-crate detail
follow the links in [Where to look](#where-to-look).

Kithara is a streaming, gapless, adaptive audio engine: it fetches media (local
file or HLS), demuxes and decodes it, runs a real-time audio pipeline (resample,
mix, crossfade, time-stretch, waveform/beat analysis), and plays it out through
a platform audio device or a queue/FFI/app surface. The whole stack is built so
that timing correctness is *testable* — see the flash virtual clock below.

## Crate map and dependency direction

Dependencies point strictly **downward** toward base abstractions; nothing lower
reaches back up into orchestration. The layers, bottom to top:

```
            kithara-app        kithara-ffi          ← surfaces (consumer-crate tops; cancel roots)
                  \             /
                   kithara  (facade: file/hls/assets/net/storage modules + re-exports)
                      |
                 kithara-queue  (multi-track queue, auto-advance)
                      |
                 kithara-play   (player engine: transport, crossfade, tempo/key-lock, sessions)
                      |
                 kithara-audio  (RT pipeline: ring, preload gate, seek-epoch, stretch routing, waveform)
                  /       |        \
        kithara-decode  kithara-stretch  kithara-encode  ← demux/decode, optional stretch DSP, encode fixtures
                  |
        kithara-file   kithara-hls              ← protocol peers (Peer impls)
                  \        /
                 kithara-stream  (unified transport: Downloader, Peer, async→sync bridge, shared media types)
            /     /      |       \        \
   kithara-abr  kithara-events  kithara-assets  kithara-storage  ← orchestration leaves
                  \       |        /        |
                 kithara-net   kithara-drm  kithara-bufpool   kithara-beat   ← utilities
                            \      |        /
                          kithara-platform  (clock/threads/sync/cancel; flash virtual-time engine)

                 kithara-devtools  (workspace tooling command core)
```

- **`kithara-platform`** is the true leaf (no `kithara-*` deps). It owns the
  clock/threads/sync backends, the cancel-token Node tree, and the `flash`
  virtual-time engine that every timing-sensitive crate transitively depends on.
- **`kithara-stream`** is the architectural waist. It owns the shared media
  types (`AudioCodec`, `ContainerFormat`, `MediaInfo`), the unified transport
  (`Downloader` / `Peer`), and the async→sync `Stream::Read` bridge. The protocol
  crates (`kithara-file`, `kithara-hls`) are `Peer` implementations *above* it;
  the orchestration leaves (`kithara-abr`, `kithara-assets`, `kithara-storage`,
  `kithara-net`, `kithara-drm`, `kithara-events`, `kithara-bufpool`) sit *below*.
- **`kithara`** is the facade crate that aggregates protocols + storage + net
  behind feature flags (`file`, `hls`, …) and is what `kithara-ffi` and
  `kithara-app` consume — they never reach into protocol crates directly.
- **`kithara-stretch`**, **`kithara-beat`**, and **`kithara-encode`** are side
  branches: optional time-stretch DSP backends consumed by `kithara-audio`,
  beat-analysis DSP consumed by `kithara-audio` (optional), and an encoder used
  mainly for test fixtures and transcode that depends only on
  `kithara-stream`'s shared types.
- The **test crates** (`kithara-test-macros`, `kithara-test-utils`) cut across
  every layer but only via `cfg(test)`/feature gates, so they are no-ops in
  production builds.
- **`kithara-devtools`** is tooling, not runtime: the workspace `xtask` binary
  uses it for lint, format, test, health, perf, and visualization commands.

## End-to-end data flow

A track plays out through a pull-driven pipeline. Each layer pulls from the one
below; backpressure and wakeups propagate the same way.

1. **Source / stream.** A `Peer` (file or HLS) is registered with the global
   `Downloader`. The `Downloader` is the single HTTP pool; the `Peer` exposes a
   per-track API. Bytes land in `kithara-storage` (`AssetStore` resources, mmap
   or in-memory) via `kithara-assets`, which is the single source of truth for
   byte availability. `kithara-stream` bridges the async fetch world to a
   **synchronous `Stream::Read`** that the decoder can block on.
2. **Decode.** `kithara-decode`'s `UniversalDecoder<D, C>` (Demuxer + FrameCodec)
   demuxes the container and produces PCM frames, handling gapless priming/trim
   and seek pre-roll. Backend codecs are feature-gated (symphonia / apple /
   android / fmp4).
3. **Audio pipeline.** `kithara-audio` drives the decode worker on its own
   thread, fills a lock-free ring behind a preload gate, resamples to the device
   format, routes optional time-stretch through `kithara-stretch`, and applies
   waveform/beat taps. Seek and format-change are state machines (seek-epoch,
   recreate) so a re-aim never replays stale audio.
4. **Play / queue.** `kithara-play` owns transport: start/stop, crossfade between
   decks, tempo/key-lock, session hosting, and the current-item announce
   contract. `kithara-queue` stacks multiple tracks on top with auto-advance and
   a pause gate, handing decks to `kithara-play`.
5. **Surfaces.** `kithara-ffi` exposes an `AudioPlayer` facade across the FFI /
   wasm boundary (worker-vs-main-thread ownership protocol); `kithara-app` is the
   TUI/GUI app and adds the track-analysis cache. Both consume the `kithara`
   facade, not the internals.

### How HLS and file differ

Both implement the same `Peer` / `Stream` contract, so everything above
`kithara-stream` is protocol-agnostic. The differences are localized:

- **File** (`kithara-file`) is largely fully-buffered: a single resource, an
  EOF readiness-probe, and local/remote source orchestration. Once fetched the
  byte space is stable.
- **HLS** (`kithara-hls`) is segmented and adaptive: a playlist cache, per-segment
  `AssetResource`s, AES-128-CBC decryption (`kithara-drm`), ABR variant switching
  (`kithara-abr`), decoder-probe rebuilds on variant/format change, and a
  two-mode `wait_range` (budget-bounded) seek/EOF contract with event-driven
  read/worker wake. A fully-buffered file source can let the flash clock run
  ahead; an HLS source is real-I/O-paced (see [TESTING.md](TESTING.md)).

This asymmetry is why the *fully-buffered vs real-I/O-paced* distinction matters
across the whole stack: the same timer behaves differently depending on which
`Peer` paces it.

## Cross-crate contracts

These are the contracts that no single crate owns; they are the seams agents
most often get wrong.

- **Cancel-token hierarchy.** Cancellation is a tree of `Arc<Node>` living in
  `kithara-platform` (`common/cancel/`). A `CancelToken` is a handle; `child()`
  derives a descendant, `cancel()` cancels its own subtree. Masters are minted
  only at owner sites — the consumer-crate tops (`kithara-app`, `kithara-ffi`,
  or `PlayerImpl` when used directly) call `root()`; everything below
  (Downloader, AssetStore, audio worker, epoch/per-fetch cancels) is a child of
  that master. `CancelScope::new(Option<CancelToken>)` is the canonical seam.
  Hard-coded `root()`/`never()` are forbidden outside a small allowlist. The full
  contract and enforcement (`cargo xtask lint arch` → `cancel_root_sites`) are in
  [`AGENTS.md`](AGENTS.md) and [`crates/kithara-play/CONTEXT.md`](crates/kithara-play/CONTEXT.md).
- **Coordinate / state spaces.** Position lives in several spaces — byte offsets,
  committed layout, virtual-reader, decode-frame index, playback time/samples —
  and values must cross an **explicit translation boundary**, never be copied
  raw. Seek and variant-switch are where these spaces collide: `kithara-hls`
  translates byte ranges across variant boundaries, `kithara-decode` translates
  seek targets into pre-roll/trim, and `kithara-audio` owns the seek-epoch /
  playhead. Mixing spaces silently is the root of most seek/recreate bugs.
- **Shared media types.** `AudioCodec`, `ContainerFormat`, and `MediaInfo` are
  owned by `kithara-stream` and must not be duplicated. Decode, encode, and the
  protocol peers all speak these types so the pipeline stays generic.
- **Unified transport (Downloader / Peer).** One global `Downloader` HTTP pool;
  each track is a `Peer` registered into it. `Peer::poll_next()` returns batches
  of self-contained `FetchCmd`s (closure-carried `writer` / `on_complete`,
  cancel via token). `kithara-assets` enforces a **single-producer /
  consumer-demand** contract on top: byte availability has one writer.
- **EventBus scoping.** `kithara-events` provides unified event types and a
  hierarchical `EventBus` with `BusScope`, feature-gated per domain (`file`,
  `hls`, `audio`, `player`, `app`, `queue`, `abr`, `downloader`). Events flow up as a side
  channel parallel to the data path; the facade and surfaces subscribe by scope
  rather than polling internals.
- **Flash virtual-clock determinism.** `kithara-platform`'s `flash` feature
  swaps real clock/runtime primitives for a quiescence-driven virtual clock so
  timing-sensitive behavior (seek settle, ABR switch, underrun, preload gating)
  is deterministic and fast — and so lost-wakeups / scheduler-blind primitives
  become hard failures instead of rare flakes. Every async primitive that can
  cross the real↔sim boundary needs a sim-participating wrapper; production async
  helpers with virtual sleeps carry `#[kithara::flash]`. **Do not restate the
  model here** — see [`TESTING.md`](TESTING.md).

## Where to look

**Rules & process:** [`AGENTS.md`](AGENTS.md) (repo-wide invariants, conventions,
coordination shapes) · [`docs/workflows/rust-ai.md`](docs/workflows/rust-ai.md)
(planning/split/handoff) · [`TESTING.md`](TESTING.md) (test gate + flash model).

**Per-crate detail** — each `crates/<crate>/CONTEXT.md`:

| Crate | What its CONTEXT.md covers |
| --- | --- |
| [`kithara-platform`](crates/kithara-platform/CONTEXT.md) | flash virtual-time/quiescence engine, real-I/O pacing, CancelToken Node tree, sync/thread/time backend tables, trait bridges |
| [`kithara-stream`](crates/kithara-stream/CONTEXT.md) | async→sync bridge lifecycle, per-source EOF invariants, public-item/feature/trait-bridge tables, arch mermaid, agent guardrails |
| [`kithara-bufpool`](crates/kithara-bufpool/CONTEXT.md) | lock-free get/put flow, shard capacity / byte-budget caps, config wiring, `doc(hidden)` low-level re-exports |
| [`kithara-storage`](crates/kithara-storage/CONTEXT.md) | blocking-coordination diagram, Mmap-vs-Mem table, chunked atomic claim, Condvar/RangeSet sync, redundant_reexport audit |
| [`kithara-assets`](crates/kithara-assets/CONTEXT.md) | key-mapping rules, decorator chain + sequence diagram, index persistence (pins/LRU/availability), byte-availability SSoT, Pending/Ready gate, consumer-demand single-producer |
| [`kithara-net`](crates/kithara-net/CONTEXT.md) | decorator/`NetExt` composition, `NetOptions` timeout semantics + defaults, four trait-bridge conversions |
| [`kithara-drm`](crates/kithara-drm/CONTEXT.md) | in-place AES-128-CBC chunk/PKCS7 commit lifecycle, KeyStore IV-derivation/key-unwrap, DomainMatcher table + registry ordering |
| [`kithara-events`](crates/kithara-events/CONTEXT.md) | per-feature gating table, trait-bridge conversions, integration/consumer list |
| [`kithara-abr`](crates/kithara-abr/CONTEXT.md) | ABR decision rules, EWMA throughput, initial-seed contract, `current_variant_index` two-writer invariant, decision-flow diagram, bench |
| [`kithara-file`](crates/kithara-file/CONTEXT.md) | arch diagram, peer/writer/EOF readiness-probe contract, local/remote source orchestration |
| [`kithara-hls`](crates/kithara-hls/CONTEXT.md) | full HLS internals: arch + public-items, ABR/variant-switch + decoder-probe rebuild + init-segment routing + format-change byte ranges, AES-128-CBC, segment cache, two-mode wait_range/seek/EOF + event wake |
| [`kithara-decode`](crates/kithara-decode/CONTEXT.md) | init paths, recreate/no-fallback strategy, `InputRequirement`, gapless + seek pre-roll/trim, read-ahead strand, features, module layout, ESDS rationale, trait bridges |
| [`kithara-encode`](crates/kithara-encode/CONTEXT.md) | `EncoderFactory` output-method contracts (Outputs), kithara-stream shared-type / test-infra integration |
| [`kithara-beat`](crates/kithara-beat/CONTEXT.md) | input/output contract, frozen pipeline constants (chunk 1500, border 6, stride 1488, 50 fps, hop 441), parity validation, MIT attribution |
| [`kithara-stretch`](crates/kithara-stretch/CONTEXT.md) | backend contract, backend-selector ownership, PcmPool scratch rules, adding-backend checklist, no-backend/wasm constraints |
| [`kithara-audio`](crates/kithara-audio/CONTEXT.md) | threading/ring/preload-gate contracts, seek-epoch + format-change/recreate state machines, waveform + blob-codec DSP, time-stretch routing, agent guardrails |
| [`kithara-play`](crates/kithara-play/CONTEXT.md) | crossfade, tempo/key-lock, events, queue handover, atomic engine start, cancel tree, RT-audio rtsan rules, session hosting, features, invariants, announce contract |
| [`kithara-queue`](crates/kithara-queue/CONTEXT.md) | queue API surface, event flow, auto-advance/pause-gate, loading lifecycle, select serialization race, kithara-app migration table |
| [`kithara`](crates/kithara/ARCHITECTURE.md) | facade arch + EventBus side-channel, Features/Key-Types tables, Re-exports module map, integration/feature-set guidance |
| [`kithara-ffi`](crates/kithara-ffi/CONTEXT.md) | wasm32/target_os cfg-gating boundary + lint exemption, AudioPlayer facade / worker-vs-main-thread ownership, Android `android_test` gating, wasm postbuild internals |
| [`kithara-app`](crates/kithara-app/CONTEXT.md) | track-analysis cache: TrackId vs AnalysisKey identity spaces, in-memory/disk tiers, AssetScope lifecycle, `ANALYSIS_BYTES_VERSION` invalidation |
| [`kithara-test-macros`](crates/kithara-test-macros/CONTEXT.md) | per-flag test semantics, flash ambient-holder-per-emit-path rules, probe-argument contract (6-arg USDT ceiling, backtrace cost) |
| [`kithara-test-utils`](crates/kithara-test-utils/CONTEXT.md) | probe-capture contract: tracing-layer-vs-EventBus rationale, process-wide subscriber install, `#[serial]` requirement, feature-gated activation |
| [`kithara-devtools`](crates/kithara-devtools/CONTEXT.md) | reusable xtask command core: Ctx/config lifecycle, CLI surface, feature gating, public common API |
