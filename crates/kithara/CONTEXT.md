# Kithara — Project Context

Cross-crate architecture contract for the Kithara audio engine: layering, end-to-end data flow, and the contracts that no
single crate owns. Repo-wide rules and coordination shapes live in [`AGENTS.md`](../../AGENTS.md); the test / flash model
lives in [`TESTING.md`](../../TESTING.md); the facade crate's own surface lives in [`ARCHITECTURE.md`](ARCHITECTURE.md).

Kithara fetches media (local file or HLS), demuxes and decodes it, runs a real-time audio pipeline (resample, mix,
crossfade, time-stretch, waveform/beat analysis), and plays out through a platform device or a queue/FFI/app surface.

## Crate map and dependency direction

Dependencies point strictly **downward**; nothing lower reaches back up into orchestration. Bottom to top:

```
        kithara-app          kithara-ffi     ← surfaces (cancel-root owners)
            |   \             /
      kithara-ui  kithara  (facade: feature-gated re-export modules)
                     |
               kithara-queue   (multi-track queue, auto-advance, pause gate)
                     |
               kithara-play    (transport, crossfade, tempo/key-lock, sessions)
                     |
               kithara-audio   (RT pipeline: ring, PreloadGate, seek-epoch, analysis)
              /      |      \        \
   kithara-decode  kithara-stretch  kithara-beat  kithara-encode
              |
   kithara-file   kithara-hls       ← protocol Peer/Source implementations
              \      /
             kithara-stream  (Downloader/Peer, Source, Stream→io::Read, media types)
        /      /      |       \
kithara-abr  kithara-events  kithara-storage  kithara-assets
        \       |        /        |
      kithara-net  kithara-drm  kithara-bufpool  kithara-resampler
                    \    |      /
                  kithara-platform  · kithara-apple
```

Side crates: `kithara-devtools` (xtask command core, no runtime deps), `kithara-workspace-hack` (feature unification),
`kithara-test-macros` / `kithara-test-utils` (cut across all layers, but only through `cfg(test)` / feature gates, so they
are no-ops in production builds).

- **`kithara-platform`** is the true leaf (no `kithara-*` runtime deps). It owns clock/threads/sync backends, the
  cancel-token `Node` tree (`src/common/cancel/`), and the `flash` virtual-time engine that every timing-sensitive crate
  transitively depends on.
- **`kithara-apple`** is a second leaf: raw AudioToolbox / Accelerate / Foundation bindings and safe wrappers. Crates
  needing Apple FFI (`kithara-decode`, `kithara-net`, `kithara-resampler`) import it instead of declaring local externs;
  codec, resampler, and HTTP policy stay in those crates.
- **`kithara-stream`** is the architectural waist. It owns the shared media types (`AudioCodec`, `ContainerFormat`,
  `MediaInfo`), the unified transport (`Downloader` / `Peer`), the protocol-agnostic `Source` trait, and
  `Stream<T: StreamType>` which implements `std::io::Read` — the async→sync bridge the decoder blocks on. Protocol crates
  sit *above* it; `kithara-abr`, `kithara-events`, `kithara-net`, and `kithara-storage` sit *below*.
- **`kithara-assets`** sits beside storage (over storage/bufpool/drm/events) and is consumed by the protocol crates and
  everything above them. `kithara-stream` does **not** depend on it.
- **`kithara`** is the facade aggregating protocols + storage + net behind feature flags; `kithara-ffi` and `kithara-app`
  consume it rather than reaching into protocol crates for the playback path.
- **Side branches:** `kithara-stretch` and `kithara-beat` are optional DSP for `kithara-audio`; `kithara-resampler` serves
  decode/audio/play; `kithara-encode` depends only on `kithara-stream` and serves the integration harness and transcode;
  `kithara-ui` is the toolkit-independent UI model (RON skin/layout/module documents → `CompiledUi`) used by
  `kithara-app` under its `gui` feature.

## End-to-end data flow

Pull-driven: each layer pulls from the one below; backpressure and wakeups propagate the same way.

1. **Source / stream.** A `Peer` (file or HLS) is registered with the global `Downloader` — the single HTTP pool; the
  `Peer` is the per-track API. Bytes land in `kithara-storage` resources (mmap or in-memory) via `kithara-assets`, the
  single source of truth for byte availability. `kithara-stream` bridges async fetch to a synchronous `io::Read`.
1. **Decode.** `kithara-decode` composes a `Demuxer` + `FrameCodec` into `ComposedDecoder`, selected at runtime by
  `DecoderFactory`, producing PCM frames and handling gapless priming/trim and seek pre-roll. Backends are feature-gated
  (symphonia / apple / android / webcodecs).
1. **Audio pipeline.** `kithara-audio` drives the decode worker on its own thread, fills a lock-free ring behind a
  `PreloadGate`, resamples to the device format, routes optional time-stretch through `kithara-stretch`, and applies
  waveform/beat taps. Seek and format-change are state machines (seek-epoch, recreate) so a re-aim never replays stale
  audio.
1. **Play / queue.** `kithara-play` owns transport: start/stop, crossfade between decks, tempo/key-lock, session hosting,
  and the current-item announce contract (`PlayerImpl` / `EngineImpl`). `kithara-queue` stacks multiple tracks with
  auto-advance and a pause gate, handing decks to `kithara-play`.
1. **Surfaces.** `kithara-ffi` exposes the `AudioPlayer` facade across the FFI / wasm boundary (worker-vs-main-thread
  ownership protocol); `kithara-app` is the desktop app (iced + `kithara-ui`) and adds the track-analysis cache.

### How HLS and file differ

Both implement the same `Peer` / `Source` contract, so everything above `kithara-stream` is protocol-agnostic. The
differences are localized:

- **File** (`kithara-file`) is largely fully-buffered: a single resource, an EOF readiness-probe, local/remote source
  orchestration. Once fetched the byte space is stable.
- **HLS** (`kithara-hls`) is segmented and adaptive: playlist cache, per-segment `AssetResource`s, AES-128-CBC decryption
  (`kithara-drm`), ABR variant switching (`kithara-abr`), decoder-probe rebuilds on variant/format change, and a two-mode
  `wait_range` (budget-bounded) seek/EOF contract with event-driven read/worker wake.

A fully-buffered file source can let the flash clock run ahead; an HLS source is real-I/O-paced. This asymmetry propagates
through the whole stack: the same timer behaves differently depending on which `Peer` paces it (see
[`TESTING.md`](../../TESTING.md)).

## Cross-crate contracts

Seams that no single crate owns, and that agents most often get wrong.

- **Cancel-token hierarchy.** Cancellation is a tree of `Arc<Node>` in `kithara-platform` (`src/common/cancel/`).
  `CancelToken` is a handle; `child()` derives a descendant, `cancel()` cancels its own subtree.
  `CancelScope::new(Option<CancelToken>)` is the canonical seam: `Some` derives a child, `None` mints a standalone root.
  Masters are minted only at owner sites; hard-coded `CancelToken::root()` / `never()` are denied outside the
  `cancel_root_sites` allowlist in `.config/arch/thresholds.toml` (`kithara-app/src/main.rs`,
  `kithara-ffi/src/native/inner.rs`, `kithara-ffi/src/core/item.rs`, `CancelScope`, plus the sentinel sites
  `kithara-stream/src/dl/batch.rs` and `kithara-hls/src/peer.rs`). Enforced by `just lint arch`; policy detail in
  [`docs/guides/cancel-policy.md`](../../docs/guides/cancel-policy.md) and
  [`kithara-play/CONTEXT.md`](../kithara-play/CONTEXT.md).
- **Coordinate / state spaces.** Position lives in several spaces — byte offsets, committed layout, virtual-reader,
  decode-frame index, playback time/samples — and values must cross an **explicit translation boundary**, never be copied
  raw. Seek and variant-switch are where these spaces collide: `kithara-hls` translates byte ranges across variant
  boundaries, `kithara-decode` translates seek targets into pre-roll/trim, `kithara-audio` owns the seek-epoch and
  playhead. Mixing spaces silently is the root of most seek/recreate bugs.
- **Shared media types.** `AudioCodec`, `ContainerFormat`, and `MediaInfo` are owned by `kithara-stream` and must not be
  duplicated. Decode, encode, and the protocol peers all speak these types so the pipeline stays generic.
- **Unified transport (Downloader / Peer).** One global `Downloader` HTTP pool; each track is a `Peer: Abr` registered
  into it. `Peer::poll_next(cx)` returns `Poll<Option<Vec<FetchCmd>>>` — self-contained commands carrying their own
  writer / completion closures and cancel token. `kithara-assets` enforces a **single-producer / consumer-demand**
  contract on top: byte availability has exactly one writer.
- **EventBus scoping.** `kithara-events` provides the unified event types and a hierarchical `EventBus` with `BusScope`,
  feature-gated per domain (`abr`, `app`, `asset`, `audio`, `decoder`, `downloader`, `drm`, `file`, `hls`, `player`,
  `queue`; `hls` implies `abr`). Events flow up as a side channel parallel to the data path and never sit in the audio
  path; the facade and surfaces subscribe by scope rather than polling internals.
- **Flash virtual-clock determinism.** `kithara-platform`'s `flash` feature swaps real clock/runtime primitives for a
  quiescence-driven virtual clock so timing-sensitive behavior (seek settle, ABR switch, underrun, preload gating) is
  deterministic and fast — and so lost wakeups and scheduler-blind primitives become hard failures instead of rare
  flakes. Every async primitive that can cross the real↔sim boundary needs a sim-participating wrapper; production async
  helpers with virtual sleeps carry `#[kithara::flash]`. The model itself is owned by [`TESTING.md`](../../TESTING.md).

## Where to look

**Rules & process:** [`AGENTS.md`](../../AGENTS.md) · [`docs/workflows/rust-ai.md`](../../docs/workflows/rust-ai.md) ·
[`TESTING.md`](../../TESTING.md).

**Per-crate detail:** every crate carries its own `crates/<crate>/CONTEXT.md` with its contracts and invariants; the
facade's own surface (features, key types, re-export map) lives in [`ARCHITECTURE.md`](ARCHITECTURE.md). Routing for
contracts whose owner is not obvious from the crate name:

- flash/quiescence engine, real-I/O pacing, cancel Node tree, sync/thread/time backends → `kithara-platform`
- byte-availability SSoT, Pending/Ready gate, consumer-demand single-producer, index persistence (pins/LRU) →
  `kithara-assets`
- blocking coordination, Mmap-vs-Mem, chunked atomic claim → `kithara-storage`
- ring/preload-gate threading, seek-epoch and format-change/recreate state machines, time-stretch routing →
  `kithara-audio`
- gapless priming/trim, seek pre-roll, recreate/no-fallback strategy, read-ahead strand → `kithara-decode`
- two-mode `wait_range`, variant switch + decoder-probe rebuild, init-segment routing, format-change byte ranges →
  `kithara-hls`
- atomic engine start, session hosting, announce contract, RT-audio rtsan rules → `kithara-play`; select serialization
  race → `kithara-queue`
- worker-vs-main-thread ownership, cfg-gating boundary, wasm postbuild → `kithara-ffi`; `ANALYSIS_BYTES_VERSION` cache
  identity → `kithara-app`
