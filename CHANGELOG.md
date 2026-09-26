# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased](https://github.com/zvuk/kithara/compare/v0.0.1-alpha4...HEAD)

### Added

- **sync**: S2 strict transition contract: modes, strict preparations, recursive transaction, receipt lifecycle ([#432](https://github.com/zvuk/kithara/pull/432))
- **bufpool**: Back cross-thread PCM rings with pooled memory ([#428](https://github.com/zvuk/kithara/pull/428))
- **Breaking** — **android**: Run every Android request through the host application's HTTP client ([#420](https://github.com/zvuk/kithara/pull/420))
- **warp**: Render projected tempo trajectories and varispeed ([#419](https://github.com/zvuk/kithara/pull/419))
- **play**: Open a track with prepared geometry and publish it ([#411](https://github.com/zvuk/kithara/pull/411))
- **warp**: Materialize the served beat grid and project it onto the session ([#407](https://github.com/zvuk/kithara/pull/407))
- **beat**: Add the served beat grid contract and its analysis producer ([#405](https://github.com/zvuk/kithara/pull/405))
- **queue**: Add typed playback policy ([#390](https://github.com/zvuk/kithara/pull/390))
- **host**: True-peak limiter with its policy on HostConfig ([#388](https://github.com/zvuk/kithara/pull/388))
- **ffi**: Make the auth token, crossfade and key rules initial state ([#389](https://github.com/zvuk/kithara/pull/389))
- **ffi**: Add modular framework features ([#376](https://github.com/zvuk/kithara/pull/376))
- **android**: Render and release the Kotlin API documentation ([#375](https://github.com/zvuk/kithara/pull/375))
- **dsp**: One smoothing primitive per parameter with its config on the owner ([#323](https://github.com/zvuk/kithara/pull/323))
- **Breaking** — **host,platform**: Build the browser host without a backend feature, and abort a wasm task ([#356](https://github.com/zvuk/kithara/pull/356))
- **derive**: Replace the ranged macro with #[derive(Ranged)] ([#341](https://github.com/zvuk/kithara/pull/341))
- **analysis**: Compute the waveform and beat grid in the browser ([#318](https://github.com/zvuk/kithara/pull/318))
- **app**: Configure kithara from a configuration document ([#254](https://github.com/zvuk/kithara/pull/254))
- **warp**: Trace live rate delivery ([#296](https://github.com/zvuk/kithara/pull/296))
- **beat,analysis**: Add a beat detector that needs no model ([#288](https://github.com/zvuk/kithara/pull/288))
- **play,warp**: Add opt-in pooled render staging ([#275](https://github.com/zvuk/kithara/pull/275))
- **output**: Preserve outputs across route changes ([#263](https://github.com/zvuk/kithara/pull/263))
- **broadcast**: Migrate live intake to output worker ([#262](https://github.com/zvuk/kithara/pull/262))
- **output,record**: Add live master recording ([#261](https://github.com/zvuk/kithara/pull/261))
- **host,play,warp**: Add transport render observability ([#270](https://github.com/zvuk/kithara/pull/270))
- **output**: Render offline sync listening artifacts ([#260](https://github.com/zvuk/kithara/pull/260))
- **encode**: Add portable continuous WAV sessions ([#258](https://github.com/zvuk/kithara/pull/258))
- **ui**: Let a document keep the state a screen turns, and a Tabs show one page of many ([#244](https://github.com/zvuk/kithara/pull/244))
- **ui**: A toolkit-neutral drawing seam, two hosts painting through one contract, and the layout on an own solver ([#128](https://github.com/zvuk/kithara/pull/128))
- **sync**: Add beat map and sync group foundation ([#223](https://github.com/zvuk/kithara/pull/223))
- **analysis, ui**: Address analysis by source position and draw what it has not covered ([#231](https://github.com/zvuk/kithara/pull/231))
- **ui,app**: Let the window take its blocks as the room for them appears ([#208](https://github.com/zvuk/kithara/pull/208))
- **app,ui**: Give the app a menu and drop the studio name ([#195](https://github.com/zvuk/kithara/pull/195))
- **play**: Expose session transport coordinates ([#186](https://github.com/zvuk/kithara/pull/186))
- **ui**: Master clock and pivot portal components ([#188](https://github.com/zvuk/kithara/pull/188))
- **broadcast**: Put live packaging behind a feature and a lane of its own ([#184](https://github.com/zvuk/kithara/pull/184))
- **broadcast**: Take the session mix on air over live HLS ([#169](https://github.com/zvuk/kithara/pull/169))
- **play,events**: Commit the session transport at an exact frame boundary ([#139](https://github.com/zvuk/kithara/pull/139))
- **stretch,audio,play,events**: Port the elastic engine and a deterministic render clock ([#138](https://github.com/zvuk/kithara/pull/138))
- **app**: Switch studio EQ topology at runtime ([#131](https://github.com/zvuk/kithara/pull/131))
- **ui,app**: The deck picks the quality of its stream ([#130](https://github.com/zvuk/kithara/pull/130))
- **play,drm,hls,ffi**: Own domain policy in play, query cache identity ([#126](https://github.com/zvuk/kithara/pull/126))
- **ui**: Optional blocks, popovers and the app menu ([#124](https://github.com/zvuk/kithara/pull/124))
- Add architecture quality and visualization tooling ([#122](https://github.com/zvuk/kithara/pull/122))
- **app,ui**: Multi-deck DJ studio on compiled kithara-ui documents ([#121](https://github.com/zvuk/kithara/pull/121))
- **ui**: Kithara-ui modular UI crate with canon gallery ([#117](https://github.com/zvuk/kithara/pull/117))
- **assets**: Enforce shared cache layout contracts ([#116](https://github.com/zvuk/kithara/pull/116))
- **events**: Unified correlated event surface + native & web/wasm FFI delivery ([#114](https://github.com/zvuk/kithara/pull/114))
- **decode**: WebCodecs browser decode backend (HE-AAC on wasm) ([#109](https://github.com/zvuk/kithara/pull/109))
- **queue**: Isolate loader lanes and unify per-track ownership ([#107](https://github.com/zvuk/kithara/pull/107))
- No_block async blocking detection (chokepoints, poll budgets, rtsan lane) ([#105](https://github.com/zvuk/kithara/pull/105))

### Changed

- **effects**: Extract audio effects into kithara-effects ([#423](https://github.com/zvuk/kithara/pull/423))
- **ui**: Size the renderer's buffers for the frame, not for an 8K scene ([#403](https://github.com/zvuk/kithara/pull/403))
- **sync**: Give the synchronization group its own crate ([#406](https://github.com/zvuk/kithara/pull/406))
- **signal**: Own the physical render output axis ([#404](https://github.com/zvuk/kithara/pull/404))
- Complete derive backlog consolidation ([#378](https://github.com/zvuk/kithara/pull/378))
- **play**: Move the resource reader tests into their own file ([#398](https://github.com/zvuk/kithara/pull/398))
- **android**: Consolidate the Android integration into kithara-android ([#394](https://github.com/zvuk/kithara/pull/394))
- **download**: Extract standalone download subsystem ([#369](https://github.com/zvuk/kithara/pull/369))
- **events**: Decouple domain events from the bus ([#354](https://github.com/zvuk/kithara/pull/354))
- Consolidate duplicate implementations and tests ([#280](https://github.com/zvuk/kithara/pull/280))
- **app**: Keep beat clock under controller ownership ([#282](https://github.com/zvuk/kithara/pull/282))
- **worker**: Build dispatcher config with bon ([#266](https://github.com/zvuk/kithara/pull/266))
- **api**: Route product consumers through facade ([#252](https://github.com/zvuk/kithara/pull/252))
- **bufpool**: Centralize pools behind typed regions ([#239](https://github.com/zvuk/kithara/pull/239))
- Cut single-implementation interfaces and dead declarations ([#233](https://github.com/zvuk/kithara/pull/233))
- Let each crate's config own its policy parameters (batch 4/6) ([#225](https://github.com/zvuk/kithara/pull/225))
- Let each crate's config own its policy parameters (batch 3/6) ([#224](https://github.com/zvuk/kithara/pull/224))
- Let each crate's config own its policy parameters (batch 2/6) ([#222](https://github.com/zvuk/kithara/pull/222))
- Let each crate's config own its policy parameters (batch 1/6) ([#218](https://github.com/zvuk/kithara/pull/218))
- **mpa,decode**: Own the MPEG demuxer fork as a workspace crate ([#200](https://github.com/zvuk/kithara/pull/200))
- **decode**: Pool-back the WebCodecs host PCM path ([#163](https://github.com/zvuk/kithara/pull/163))
- **assets,storage**: Cut the per-read and per-segment disk cost of HLS playback ([#144](https://github.com/zvuk/kithara/pull/144))
- **play**: Role-first layout, typed session protocol, crate-private configs ([#108](https://github.com/zvuk/kithara/pull/108))
- **audio**: Deep kithara-audio refactor — 77→0 arch violations, cooperative analyzer ([#113](https://github.com/zvuk/kithara/pull/113))
- **bufpool**: Region-owned shared budget + compile-enforced pool injection ([#111](https://github.com/zvuk/kithara/pull/111))
- **resampler**: Centralize backend ownership ([#106](https://github.com/zvuk/kithara/pull/106))
- **stretch**: Extract time-stretch backend into kithara-stretch crate ([#100](https://github.com/zvuk/kithara/pull/100))
- **assets,ffi,apple**: Url-only injectable AssetLayout + typed StorageBackend ([#94](https://github.com/zvuk/kithara/pull/94))

### Fixed

- **ui**: Keep the workspace hack for desktop builds only
- **android**: Keep the desktop UI stack out of the workspace-hack base
- **file**: Stop reading the whole track to index it, and own the walk in kithara-mp4 ([#414](https://github.com/zvuk/kithara/pull/414))
- Settle two CI flakes at their causes ([#415](https://github.com/zvuk/kithara/pull/415))
- **audio**: Preserve consumer readiness notifications ([#418](https://github.com/zvuk/kithara/pull/418))
- Close the two flakes the zero-flake gate exposed ([#413](https://github.com/zvuk/kithara/pull/413))
- **ui**: Stop rebuilding the picture a still page already drew ([#402](https://github.com/zvuk/kithara/pull/402))
- Every stress flake at its root, with a test each
- **resampler**: Keep the glide low-pass filter across a retune ([#399](https://github.com/zvuk/kithara/pull/399))
- **queue**: A selection carries its start intent and a config failure fails synchronously ([#384](https://github.com/zvuk/kithara/pull/384))
- **apple**: Configure the demo audio session before hosting players ([#383](https://github.com/zvuk/kithara/pull/383))
- **decode**: Resample one decoded chunk into one output chunk ([#381](https://github.com/zvuk/kithara/pull/381))
- **android**: Select queue items by identity, not by index ([#386](https://github.com/zvuk/kithara/pull/386))
- **queue**: Reload a repeat-one track whose slot lost its resource ([#377](https://github.com/zvuk/kithara/pull/377))
- **android**: Align CI coverage with the shipped product ([#373](https://github.com/zvuk/kithara/pull/373))
- **audio**: Retire a variant transition its seek epoch superseded ([#361](https://github.com/zvuk/kithara/pull/361))
- **play**: Answer the requested sample rate from the host, and let GitHub gate no_block ([#322](https://github.com/zvuk/kithara/pull/322))
- **audio,decode**: Stop a past-EOF seek from retiring a track for good ([#324](https://github.com/zvuk/kithara/pull/324))
- Drop a player without waiting for the admission gate; end a segmented stream only past its own count; count retried passes and denoise stress evidence ([#274](https://github.com/zvuk/kithara/pull/274))
- **queue**: Name the track a handover request is about ([#297](https://github.com/zvuk/kithara/pull/297))
- **play**: Bound transport response and preserve continuity ([#232](https://github.com/zvuk/kithara/pull/232))
- **queue**: Dispatch apply work through platform ([#314](https://github.com/zvuk/kithara/pull/314))
- **queue**: Apply a finished load off the runtime worker ([#299](https://github.com/zvuk/kithara/pull/299))
- **audio,play**: Deliver reader events by consumer context ([#292](https://github.com/zvuk/kithara/pull/292))
- **storage**: Preserve mmap snapshot when reopen fails ([#295](https://github.com/zvuk/kithara/pull/295))
- Reclaim shared pool capacity and avoid ABR repolling ([#294](https://github.com/zvuk/kithara/pull/294))
- **play**: Bound producer pacing by output quantum ([#287](https://github.com/zvuk/kithara/pull/287))
- **ui**: Make the two render hosts agree, and pin the agreement ([#285](https://github.com/zvuk/kithara/pull/285))
- **ffi**: Preserve stretch backend wire payload ([#284](https://github.com/zvuk/kithara/pull/284))
- **app**: Preserve analysis priority across axis changes ([#283](https://github.com/zvuk/kithara/pull/283))
- **stretch,warp**: Configure backends and stream exact terminal drain ([#272](https://github.com/zvuk/kithara/pull/272))
- **ui**: Show the stepping hand over a surface on the retained host ([#271](https://github.com/zvuk/kithara/pull/271))
- **ui**: Keep a press inside an open popover off the page under it ([#269](https://github.com/zvuk/kithara/pull/269))
- **host**: Own output block and sample rate configuration ([#268](https://github.com/zvuk/kithara/pull/268))
- **host**: Preserve backend-default sample-rate request ([#267](https://github.com/zvuk/kithara/pull/267))
- **audio**: Commit a failed seek's epoch so the terminal marker lands ([#248](https://github.com/zvuk/kithara/pull/248))
- **platform,warp,play,tests**: Bring the wasm browser lane back to green ([#240](https://github.com/zvuk/kithara/pull/240))
- **ui**: Let a stepping surface hold the pointer it armed on ([#250](https://github.com/zvuk/kithara/pull/250))
- **play**: Block offline renders on producer-ring underrun ([#247](https://github.com/zvuk/kithara/pull/247))
- **stream,file,audio**: Close the stress-lane flakes behind seeks, ABR, and shared downloads ([#243](https://github.com/zvuk/kithara/pull/243))
- **assets,hls,app**: An availability claim is only as good as the file behind it ([#242](https://github.com/zvuk/kithara/pull/242))
- **audio**: Keep the audio callback off blocking calls, and resolve the gapless tail where it is knowable ([#238](https://github.com/zvuk/kithara/pull/238))
- **play,events,queue**: Name which item an event is about, and give it a real id ([#229](https://github.com/zvuk/kithara/pull/229))
- **tests,audio,stream**: Stress all-green — loom gate rendezvous and RT byte-space polls off the control mutex ([#226](https://github.com/zvuk/kithara/pull/226))
- **queue,play**: Stop advancing the queue on a background slot's end ([#228](https://github.com/zvuk/kithara/pull/228))
- **play,audio**: Stress all-green — stale EOF fence and initial decoder event order ([#199](https://github.com/zvuk/kithara/pull/199))
- **audio**: Read the grid's tempo off its own beats ([#198](https://github.com/zvuk/kithara/pull/198))
- **storage,hls**: Make a reclaimed tmp start empty and give acquire failures a budget ([#214](https://github.com/zvuk/kithara/pull/214))
- **ffi,play,test**: Give the wasm bundle the resampler its context needs ([#205](https://github.com/zvuk/kithara/pull/205))
- **storage,hls**: Claim a segment tmp by lock, not by existence ([#203](https://github.com/zvuk/kithara/pull/203))
- **ffi**: Avoid duplicate terminal item events ([#179](https://github.com/zvuk/kithara/pull/179))
- **play**: Preserve no-SYNC stereo gain ([#177](https://github.com/zvuk/kithara/pull/177))
- **ffi**: Enter runtime in polling thread ([#181](https://github.com/zvuk/kithara/pull/181))
- **audio**: Let the construction gate pick the read mode, not the seek mode ([#172](https://github.com/zvuk/kithara/pull/172))
- **audio,hls**: Give every decoder reader its own construction gate ([#168](https://github.com/zvuk/kithara/pull/168))
- **hls,assets**: Close the produce-core RT violations the rtsan-hls lane exposes ([#149](https://github.com/zvuk/kithara/pull/149))
- **audio**: Take logging, allocation and seek off the real-time contours ([#141](https://github.com/zvuk/kithara/pull/141))
- **audio**: Make the suite's verdict independent of machine load ([#137](https://github.com/zvuk/kithara/pull/137))
- **app**: An EQ band is flat when its knob points straight up ([#134](https://github.com/zvuk/kithara/pull/134))
- **apple**: Run fat LTO on the release slices and trim the device graph ([#132](https://github.com/zvuk/kithara/pull/132))
- **audio**: Preserve continuity across quality switches ([#119](https://github.com/zvuk/kithara/pull/119))
- **app**: Match the deck EQ layout to the knobs the studio draws ([#129](https://github.com/zvuk/kithara/pull/129))
- **decode**: Find the MP3 Xing tag behind an oversized ID3v2 tag ([#127](https://github.com/zvuk/kithara/pull/127))
- **audio**: Publish consumer events without worker flush ([#115](https://github.com/zvuk/kithara/pull/115))
- **decode**: Eliminate splice click on HLS ABR variant switch ([#102](https://github.com/zvuk/kithara/pull/102))
- **apple**: Unblock xcframework build (pin uniffi 0.31.2 + simulator bindgen triple) ([#104](https://github.com/zvuk/kithara/pull/104))
- **audio**: Make post-seek pcm drain epoch-aware to stop dropping seek-target chunks ([#97](https://github.com/zvuk/kithara/pull/97))
- **stream**: Cap resampled-path position writes at the duration budget ([#96](https://github.com/zvuk/kithara/pull/96))
- **apple**: Pass seek completion closure in demo after wrapper API change ([#93](https://github.com/zvuk/kithara/pull/93))

## [0.0.1-alpha4](https://github.com/zvuk/kithara/releases/tag/v0.0.1-alpha4) - 2026-07-01

### Added

- **net**: NSURLSession HTTP backend (`client-apple`) for Apple targets ([#89](https://github.com/zvuk/kithara/pull/89)).
- **assets**: `AssetStore::subscribe_eviction` returns an `EvictionSubscription` guard that routes evictions per asset root ([#88](https://github.com/zvuk/kithara/pull/88)).

### Changed

- **Breaking** — **assets**: one non-generic `AssetStore` serves both file and HLS; per-resource processing travels per acquire as `ProcessCtx`, and the `HlsStore` wrapper with its eviction registry is gone ([#88](https://github.com/zvuk/kithara/pull/88)).
- **apple**: smaller iOS framework — feature narrowing, a mobile build profile (`panic=abort`, strip, `opt-level=z`, LTO, `build-std`), and no `uniffi-bindgen` in the device static library; the symbol audit reports no Symphonia or fdk-aac symbols ([#89](https://github.com/zvuk/kithara/pull/89)).
- **hls**: a segment owns its file and its typed size, resolved on demand ([#89](https://github.com/zvuk/kithara/pull/89)).
- **decode**: `DecodeError` carries typed fields instead of formatted strings ([#89](https://github.com/zvuk/kithara/pull/89)).

### Fixed

- **wasm**: the threaded holders compile on wasm32 again, and the release `wasm-opt` pass enables threads and bulk memory ([#89](https://github.com/zvuk/kithara/pull/89)).

## [0.0.1-alpha3](https://github.com/zvuk/kithara/releases/tag/v0.0.1-alpha3) - 2026-06-21

### Added

- **assets**: one application-wide `AssetStore` shares a single download between concurrent consumers of a URL ([#72](https://github.com/zvuk/kithara/pull/72)).
- **audio**: waveform analysis (`Envelope`, `PeakAccumulator`) and a pre-resampler time-stretch slot ([#72](https://github.com/zvuk/kithara/pull/72)); a `StretchBackend` seam with timestretch, signalsmith and bungee adapters ([#79](https://github.com/zvuk/kithara/pull/79)).
- **beat**: beat slicing and neural beat tracking ([#79](https://github.com/zvuk/kithara/pull/79)).
- **app**: DJ Studio deck with a colored frequency waveform, zoom, pan, click-to-seek and a beat-grid overlay; analysis publishes the waveform first and the beat grid when it is ready ([#72](https://github.com/zvuk/kithara/pull/72), [#79](https://github.com/zvuk/kithara/pull/79), [#81](https://github.com/zvuk/kithara/pull/81)).
- **ffi**: one cross-platform `AudioPlayer` over native and wasm back ends ([#73](https://github.com/zvuk/kithara/pull/73)).
- **apple**, **android**: SDK packaging through `xtask`, with refreshed Swift and Kotlin examples ([#80](https://github.com/zvuk/kithara/pull/80)).

### Changed

- **Breaking** — typestate re-architecture across storage, assets, audio, HLS, ABR, queue, net and FFI: illegal transitions no longer compile ([#73](https://github.com/zvuk/kithara/pull/73)).
- **audio**: the worker produce core and `process()` run without blocking under RealtimeSanitizer; committed storage reads are lock-free ([#73](https://github.com/zvuk/kithara/pull/73)).
- **hls**: segment size estimation reads the asset store first and probes only cache misses ([#79](https://github.com/zvuk/kithara/pull/79)).
- **assets**: cache file names derive from the asset scope instead of signed URLs ([#80](https://github.com/zvuk/kithara/pull/80)).

### Fixed

- **storage**: the read watchdog resets on progress, so a slow first byte no longer panics the worker ([#72](https://github.com/zvuk/kithara/pull/72)).
- **hls**: an urgent down-switch no longer deadlocks at a segment boundary, and a variant change waits for an in-flight seek ([#73](https://github.com/zvuk/kithara/pull/73)).
- **hls**: end of stream is held while segment sizes are incomplete, so an immediate seek no longer auto-advances ([#80](https://github.com/zvuk/kithara/pull/80)).
- **decode**: seeks no longer strand Symphonia read-ahead at a not-ready segment boundary or leave stale fdk-aac overlap ([#80](https://github.com/zvuk/kithara/pull/80)).
- **audio**: the playback worker parks instead of busy-spinning on its read-ahead window, and a mid-playback recreate resumes from the decode head ([#80](https://github.com/zvuk/kithara/pull/80)).

## [0.0.1-alpha2](https://github.com/zvuk/kithara/releases/tag/v0.0.1-alpha2) - 2026-05-28

Metadata and documentation release; no runtime behavior changes ([#71](https://github.com/zvuk/kithara/pull/71)).

### Changed

- Per-crate keywords, categories and descriptions for crates.io, with crates.io and docs.rs badges in every crate README.
- Architecture moved to `ARCHITECTURE.md`; build, test and mobile packaging moved to `CONTRIBUTING.md`.
- `Package.swift` points at a rebuilt `KitharaFFIInternal.xcframework` whose decode back end is Apple-only.

## [0.0.1-alpha1](https://github.com/zvuk/kithara/releases/tag/v0.0.1-alpha1) - 2026-05-19

First public alpha. Pre-release: public APIs may shift between alpha tags.

### Added

- Player engine (`kithara-play`): AVPlayer-style API on top of Firewheel audio graph with multi-slot arena, crossfading, BPM sync, per-channel EQ, and DJ-ready architecture.
- Queue layer (`kithara-queue`): AVQueuePlayer-analogue with queue, loader, navigation, and crossfade-aware track selection.
- Adaptive bitrate (`kithara-abr`): protocol-agnostic ABR with pull-driven decisions, manual switch, and seed-based initial throughput estimation.
- HLS VOD (`kithara-hls`): variant switching, cross-codec recreate, AES-128-CBC decryption, container-aware ABR commit (fMP4 same-codec recreate), and reader-side publication of `HlsEvent::ReaderSeek` / `HlsEvent::SegmentReadStart` via the `kithara-stream` reader hooks.
- Progressive file (`kithara-file`): pull-driven loop with an internal `FilePeer` registered against the shared `Downloader`; MP3/AAC/FLAC streaming over HTTP; local-file fast path that skips the downloader entirely.
- Decode (`kithara-decode`): public `Decoder` trait plus internal `ComposedDecoder<D, C>` over the `Demuxer` and `FrameCodec` traits; Apple AudioToolbox native standalone (WAV/MP3/ALAC) via AudioFileServices; Android `MediaExtractor` standalone; Symphonia software backend; CBR batching with preserved `io::Error` chain; `Frame.packet_desc` for VBR.
- DRM (`kithara-drm`): AES-128-CBC end-to-end with extensible `KeyProcessor`.
- Events (`kithara-events`): unified `EventBus` with hierarchical `BusScope`, feature-gated by surface (`file`/`hls`/`audio`/`player`).
- App (`kithara-app`): unified app crate with feature-gated `tui` (ratatui) and `gui` (iced) frontends; single binary `kithara` with `--mode auto|tui|gui`.
- FFI (`kithara-ffi`): cross-platform FFI adapter for the player; UniFFI bindings for Apple (Swift) and Android (Kotlin).
- WASM (`kithara-wasm`): browser playback bindings with shared-memory threading, Trunk-served demo, `wasm_safe_thread` runtime.
- Workspace tooling (`xtask`): three lint namespaces (`arch` 33 checks, `style` 5, `idioms` 19) with comment-preserving autofix engine; ast-grep policy filter (55 rules); workspace typos wrapper; health reporter; publish-order resolver; orphans-per-package; similarity audit; arch visualisation.
- USDT probe macro `#[kithara::probe(...)]` for runtime tracing across HLS/stream/ABR/audio/decode.
- E2E suite (`tests/`) with `suite_light` / `suite_heavy` / `suite_stress` targets, nextest stress mode, Selenium WASM scenarios.

### Changed

- Workspace-wide migration to `bon::Builder` for all `*Config` structs (`PlayerConfig`, `ResourceConfig`, `DownloaderConfig`, `NetOptions`, `StoreOptions`, `FetchCmd`, `KeyProcessorRule`, `MediaInfo`, `SourceSeekAnchor`, …).
- Unified transport: `kithara_stream::dl::Downloader` is the global HTTP pool; `PeerHandle` is the per-track API; `Downloader::register(peer)` registers `Peer` implementations.
- HLS rewrite: internal `HlsCoord` orchestrator with persistent fetch queue, cancel-token epoch, and interior mutability for production wiring.
- `kithara-stream`: non-`Optional` `SegmentLayout::init_segment_range`; `Stream::Read` retry signal — `wait_range` errors must contain `"budget exceeded"` to be treated as transient.
- `kithara-test-utils` split into `hang` / `mock` / `probe` / `test` submodules with granular cargo features; `hang-detector` crate merged into `test-utils::hang`.
- `kithara-test-macros` decomposed into `test` and `probe` submodules.
- Integration test domain moved out of `kithara-test-utils` into `kithara-integration-tests` (`tests/` crate); tests now drive via public API rather than white-box helpers.
- Tests consolidated via `#[case]` parametrize; duplicate suite_heavy mounts dropped.
- Public types across crate boundaries marked `#[non_exhaustive]`.

### Fixed

- DRM PKCS7 size shrink: refresh segment range mid-fill so demuxer absorbs the shrink.
- Manual ABR switch fires immediately; cross-codec recreate stability under switch+seek.
- One-shot fMP4 parse + seek-collapse cross-variant continuity.
- Cross-variant byte continuity on `Auto` switch.
- Reentrant deadlock in `poll_state_phase` via `AbrController` tick.
- `Source::current_segment_range`, `media_info`, `format_change_segment_range` correctness after HlsCoord rewrite and ABR commit.
- Audio worker wake-up after `clear_seek_pending` so ABR lock releases when peer is idle.
- WAV continuity race, cross-codec init, segment-index propagation.
- Size-based recreate-readiness gate for backwards seek after manual variant switch.

### Removed

- `kithara-hang-detector` crate (merged into `kithara-test-utils::hang`).
- `kithara-ui` and `kithara-tui` crates (collapsed into `kithara-app::gui` / `kithara-app::tui` modules behind feature flags).
- `DownloadState`, `HlsSegmentView`, `commit_variant_layout`, the file-side demand machinery, and `ResourceConfig` chain shims; semantic mapping is now owned by `HlsCoord`, byte availability by `AssetStore`.
- White-box `test_helpers`; tests now use only the public API.
