# Stream/Audio/HLS Modularization And Isolation Plan
> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** разложить `kithara-stream`, `kithara-audio` и `kithara-hls` на изолированные модули с четкими dependency rules, чтобы никакой модуль не “знал обо всем”, а изменение одной runtime-подсистемы не пробивало инкапсуляцию через весь крейт.

**Architecture:** each crate gets a small public API layer, a narrow domain/core layer, and a runtime/integration layer. Big files are split by responsibility, not “по случайным helper-ам”. All cross-module dependencies become directional and documented.

**Tech Stack:** current Rust workspace layout, existing `mod.rs`/`lib.rs` barrel exports, existing types from `kithara-stream`, `kithara-audio`, `kithara-hls`, no new dependencies.

## 1. Why this modularization is needed

Current file sizes already показывают архитектурную проблему:

- `kithara-stream/src/backend.rs`: 755 lines
- `kithara-stream/src/stream.rs`: 594 lines
- `kithara-audio/src/pipeline/source.rs`: 1535 lines
- `kithara-audio/src/pipeline/audio.rs`: 1264 lines
- `kithara-audio/src/pipeline/audio_worker.rs`: 857 lines
- `kithara-hls/src/downloader.rs`: 3782 lines
- `kithara-hls/src/source.rs`: 1842 lines
- `kithara-hls/src/fetch.rs`: 1313 lines
- `kithara-hls/src/playlist.rs`: 788 lines

Это не просто “длинные файлы”. Это признак того, что несколько независимых решений живут в одном модуле и знают друг о друге слишком много:

- runtime assembly смешана с domain types;
- state machines смешаны с IO details;
- mutable state, public contracts и helper logic лежат рядом;
- integration test support частично определяет production visibility.

## 2. Target modularization principles

- `lib.rs` и `mod.rs` только объявляют модули и `pub use`.
- Public API layer не владеет runtime логикой.
- Domain/core modules не знают про event bus, background tasks и storage backends.
- Runtime modules не определяют shared domain vocabulary, только используют его.
- Each large FSM gets its own module tree.
- Each shared mutable state gets one dedicated module owner.
- Internal module boundaries must be enforceable through `pub(crate)` and imports, not only by convention.

## 3. `kithara-stream` target module design

### 3.1 Current pain points

- `stream.rs` смешивает public type, `StreamType`, read/seek orchestration, tests и HLS-facing delegation surface.
- `source.rs` содержит и core read contract, и HLS-specific capabilities.
- `backend.rs` совмещает task scheduling, control loop, downloader driving и shutdown behavior.
- `writer.rs` generic, но его API по умолчанию привязано к `NetError`.

### 3.2 Target tree

```text
crates/kithara-stream/src/
  lib.rs
  api/
    mod.rs
    stream.rs
    stream_type.rs
    source_core.rs
    source_capabilities.rs
  core/
    mod.rs
    demand.rs
    fetch.rs
    timeline.rs
    topology.rs
    layout.rs
    coordination.rs
    cursor.rs
    media.rs
  diagnostics/
    mod.rs
    context.rs
  runtime/
    mod.rs
    backend/
      mod.rs
      driver.rs
      loop.rs
      state.rs
    downloader.rs
    writer.rs
  error.rs
  internal.rs
  mock.rs
```

### 3.3 Responsibilities

- `api/stream.rs`
  - only `Stream<T>`
  - only `Read + Seek` orchestration
  - no diagnostics/event extraction
- `api/stream_type.rs`
  - only source construction trait
  - no higher-layer integration hooks
- `api/source_core.rs`
  - core source contract
- `api/source_capabilities.rs`
  - optional traits such as demand/seek/boundary/wakeup
- `core/*`
  - vocabulary and shared generic primitives
- `core/fetch.rs`
  - `Fetch<T>` and `EpochValidator`
  - belongs to shared decode/control vocabulary, not downloader runtime
- `diagnostics/context.rs`
  - optional context/observability adapters
  - not part of source construction core
- `runtime/backend/*`
  - downloader execution loop and state transitions
- `runtime/writer.rs`
  - pure byte-stream -> resource writing adapter without network-flavored defaults

### 3.4 Dependency rules

- `api/*` may depend on `core/*` and `error.rs`.
- `core/*` must not depend on `runtime/*`.
- `runtime/*` may depend on `api/*`, `core/*`, `error.rs`.
- `runtime/*` must not depend on `kithara-events`.
- `api/source_capabilities.rs` may mention generic source-specific concepts, but not `Hls`, `segment`, `variant` in type names.

### 3.5 Concrete extraction plan

1. Move `StreamType` from current `stream.rs` into `api/stream_type.rs`.
2. Split `Source` into `source_core.rs` and `source_capabilities.rs`.
3. Split `backend.rs` into:
   - `runtime/backend/driver.rs` for high-level orchestration;
   - `runtime/backend/loop.rs` for polling loop;
   - `runtime/backend/state.rs` for internal state and transitions.
4. Remove default `NetError` from `Writer` and keep error type explicit at call sites.
5. Keep stream context types under `diagnostics/*`, not `api/*`, because they are optional adapters rather than construction contracts.
6. Leave `Timeline`, `DemandSlot`, `LayoutIndex`, `Topology` in `core/*`.

## 4. `kithara-audio` target module design

### 4.1 Current pain points

- `Audio` is a god-object and mixes public API, consumer state, worker ownership, event integration and timeline commitment.
- `StreamAudioSource` is an even larger god-object: lock wrapper, decoder session, seek FSM, boundary handling, event emission, chunk accounting and worker contract.
- `track_fsm.rs` already carries a clean conceptual boundary, but it still lives as one large flat file and is not isolated from the rest of the pipeline.
- worker runtime and track domain are not separated cleanly.

### 4.2 Target tree

```text
crates/kithara-audio/src/
  lib.rs
  api/
    mod.rs
    audio.rs
    config.rs
    service_class.rs
    traits.rs
  processing/
    mod.rs
    builder.rs
    output_spec.rs
  runtime/
    mod.rs
    worker/
      mod.rs
      handle.rs
      registry.rs
      registration.rs
      slot.rs
      loop.rs
      channels.rs
      types.rs
      wake.rs
  track/
    mod.rs
    fsm/
      mod.rs
      state.rs
      seek.rs
      recreate.rs
      wait.rs
      consumer.rs
      failure.rs
    source/
      mod.rs
      contract.rs
      adapter.rs
      shared_stream.rs
      offset_reader.rs
      decoder_session.rs
      seek_controller.rs
      boundary_controller.rs
      event_adapter.rs
    decode.rs
    metrics.rs
  effects/
    mod.rs
    eq.rs
  render/
    mod.rs
    resampler/
      mod.rs
      params.rs
      processor.rs
      rubato.rs
  internal.rs
  mock.rs
```

### 4.3 Responsibilities

- `api/audio.rs`
  - thin facade
  - public methods only
  - no deep worker protocol code
- `api/service_class.rs`
  - public scheduling vocabulary exposed to callers and public traits
- `processing/*`
  - builds effect chain and expected output spec from config
  - isolates config DTOs from render/runtime wiring
- `runtime/worker/*`
  - track registration, command routing, background execution
  - no decoder policy
- `runtime/worker/registration.rs`
  - `WorkerCmd`, `TrackRegistration`
- `runtime/worker/slot.rs`
  - per-track runtime state such as `TrackSlot`
- `runtime/worker/types.rs`
  - `TrackId`, `TrackIdGen` and private runtime helper types
- `track/fsm/*`
  - pure track state machine vocabulary and transitions
  - no `Mutex<Stream<T>>`, no event bus, no thread wake internals
- `track/source/contract.rs`
  - `AudioWorkerSource` and other source-facing control contracts
  - bridge between track domain and worker runtime, without embedding concrete worker loop code
- `track/source/shared_stream.rs`
  - only shared stream locking adapter
- `track/source/decoder_session.rs`
  - only decoder + base offset + media info bundle
- `track/source/seek_controller.rs`
  - only source-side seek mapping/apply/commit logic
- `track/source/boundary_controller.rs`
  - only format-boundary protocol
- `track/source/event_adapter.rs`
  - only translation from track/runtime transitions to `AudioEvent`
- `render/resampler/*`
  - isolates rubato integration, parameter types and runtime processor state
  - keeps sample-rate policy out of the audio facade and worker registry

### 4.4 Dependency rules

- `api/*` may depend on `runtime/*`, `track/*`, `effects/*`, `render/*`.
- `api/config.rs` must stay a DTO/public builder surface; effect-chain construction and output-spec derivation move into `processing/*`.
- `runtime/worker/*` may depend on `track/*`, but `track/*` must not depend on runtime worker registry/handle types.
- `track/fsm/*` must not depend on `kithara_platform::Mutex`, ring buffers, or concrete worker channels.
- `track/source/*` may depend on `kithara-stream` capability traits and `kithara-decode`, but must not depend on HLS structs.
- `effects/*` and `render/*` must stay isolated from worker/seek state.

### 4.5 Concrete extraction plan

1. Move `AudioConfig` into `api/config.rs` as DTO/public builder surface only.
2. Move `create_effects()` and `expected_output_spec()` out of config into `processing/builder.rs` and `processing/output_spec.rs`.
3. Move `ServiceClass` into `api/service_class.rs`; keep only runtime-only identifiers such as `TrackId` in `runtime/worker/types.rs`.
4. Move `Audio` public facade into `api/audio.rs`; keep only buffering/consumer-facing fields there.
5. Split current `pipeline/audio.rs` into:
   - facade/public methods;
   - consumer buffering state;
   - event publishing helpers.
6. Split `pipeline/source.rs` into:
   - `shared_stream.rs`
   - `offset_reader.rs`
   - `decoder_session.rs`
   - `seek_controller.rs`
   - `boundary_controller.rs`
   - `adapter.rs` implementing `AudioWorkerSource`
7. Split `track_fsm.rs` by state families, keeping a single `mod.rs` barrel.
8. Move `AudioWorkerSource` into `track/source/contract.rs` so the worker runtime depends on a track contract instead of concrete source modules.
9. Split `audio_worker.rs` into registration/registry, slot, loop, handle, wake, channels and runtime types.
10. Split `resampler.rs` into parameter types, rubato wrapper and processor/runtime state so `render` stops being a 1k-line hidden subsystem.

## 5. `kithara-hls` target module design

### 5.1 Current pain points

- `inner.rs` is effectively a whole composition root stuffed into one function.
- `source.rs` mixes source read path, seek policy, layout policy, variant resolution, cache checks and builder wiring.
- `downloader.rs` is an oversized orchestration blob: planning, demand handling, fetch execution, commit, ABR and event emission.
- `fetch.rs` combines playlist cache, key fetching, segment loading, dedupe, asset store adapter and loader API.
- `playlist.rs` mixes immutable parsed topology with mutable size-estimation state.
- `download_state.rs` and `stream_index.rs` coexist, preserving architectural drift.

### 5.2 Target tree

```text
crates/kithara-hls/src/
  lib.rs
  api/
    mod.rs
    config.rs
    context.rs
    error.rs
    fetch.rs
    hls.rs
  model/
    mod.rs
    parsing/
      mod.rs
      master.rs
      media.rs
      keys.rs
      variants.rs
    topology/
      mod.rs
      catalog.rs
      variant.rs
      segment.rs
      offsets.rs
    layout/
      mod.rs
      stream_index.rs
      segment_data.rs
  runtime/
    mod.rs
    estimates/
      mod.rs
      variant_sizes.rs
    session/
      mod.rs
      builder.rs
      diagnostics.rs
      state.rs
      events.rs
    coord/
      mod.rs
      demand.rs
      signals.rs
      timeline.rs
    source/
      mod.rs
      reader.rs
      readiness.rs
      seek.rs
      boundary.rs
      handle.rs
    downloader/
      mod.rs
      planner.rs
      scheduler.rs
      commit.rs
      abr.rs
      handle.rs
    fetch/
      mod.rs
      client.rs
      playlists.rs
      segments.rs
      dedupe.rs
      loader.rs
    keys.rs
  internal.rs
```

### 5.3 Responsibilities

- `api/hls.rs`
  - only `Hls` marker and thin `StreamType` implementation
- `api/context.rs`
  - public re-export surface for `HlsStreamContext`
  - implementation may live in runtime diagnostics, but crate surface stays explicit
- `api/fetch.rs`
  - explicit home for the current fetch-facing public types during modularization
- `model/topology/*`
  - immutable playlist-derived model
  - no asset store, no bus, no cancellation
- `model/layout/*`
  - single committed byte layout model
  - replaces legacy `DownloadState`
- `runtime/estimates/*`
  - mutable sizing and offset-estimation state currently embedded in `PlaylistState`
  - consumed by layout/downloader without making topology mutable again
- `runtime/session/*`
  - assemble and own runtime state
  - own diagnostics adapters such as `HlsStreamContext`
- `runtime/coord/*`
  - wakeups, demand slot, stop/cancel signaling
- `runtime/source/*`
  - `Source` implementation logic only
- `runtime/downloader/*`
  - planning and fetch/commit orchestration only
- `runtime/fetch/*`
  - network/cache/key-backed loading primitives only
- `runtime/session/diagnostics.rs`
  - session-owned `StreamContext`/observability adapters that read runtime state but are not part of source construction contracts

### 5.4 Dependency rules

- `model/*` must not depend on `tokio`, `AssetStore`, `EventBus` or `HttpClient`.
- `runtime/estimates/*` may depend on immutable topology identifiers, but topology must not depend back on estimates.
- `runtime/fetch/*` may depend on `model/topology/*` and `runtime/keys`, but must not depend on source/downloader policy modules.
- `runtime/source/*` may depend on `runtime/coord/*`, `model/*`, `runtime/fetch/loader.rs`.
- `runtime/downloader/*` may depend on `runtime/coord/*`, `model/*`, `runtime/fetch/*`.
- only `runtime/session/*` may wire source, downloader, fetch, events and cancellation together.
- `api/*` must not directly instantiate `AssetStore`, `KeyManager`, `FetchManager`.

### 5.5 Concrete extraction plan

1. Introduce `runtime/session/builder.rs` and move most of `inner.rs::create()` there.
2. Introduce `runtime/estimates/variant_sizes.rs` and move mutable sizing/reconciliation logic there before splitting `PlaylistState`.
3. Replace `build_pair(...)` with session-owned handle construction.
4. Split `source.rs` into:
   - `reader.rs` for `read_at`;
   - `readiness.rs` for `wait_range`/phase logic;
   - `seek.rs` for anchor resolution and layout reset policy;
   - `boundary.rs` for variant fence / format boundary protocol;
   - `handle.rs` for the public `HlsSource` wrapper.
5. Split `downloader.rs` into:
   - `planner.rs` for “what should be downloaded next”;
   - `scheduler.rs` for loop and wake handling;
   - `commit.rs` for applying loaded segments into layout;
   - `abr.rs` for variant switching policy.
6. Split `fetch.rs` into:
   - `client.rs` for low-level cached fetches;
   - `playlists.rs` for master/media playlist cache;
   - `segments.rs` for media/init loading;
   - `dedupe.rs` for OnceCell-based in-flight de-duplication;
   - `loader.rs` for the trait exposed to other runtime modules.
7. Split `parsing.rs` into master/media/key/variant parsing modules so the parsed-manifest layer is isolated before runtime refactor starts.
8. Split `playlist.rs` into immutable `topology` plus runtime estimates/sizing state.
9. Keep the current fetch-facing public surface explicitly re-exported from `api/fetch.rs` during the split; any API contraction is a separate phase.
10. Move current `context.rs` implementation into `runtime/session/diagnostics.rs`, but preserve public access through `api/context.rs`.
11. Delete `download_state.rs` after `StreamIndex` becomes the sole layout model.

## 6. File-by-file sequencing

### Step 1. Reshape public barrels only

- create new directories and `mod.rs` files;
- keep old file contents temporarily re-exported from new module tree;
- no behavior changes yet.

### Step 2. Extract pure domain modules

- `kithara-stream`: capability traits, `StreamType`, core primitives;
- `kithara-audio`: `track_fsm` family, `DecoderSession`;
- `kithara-hls`: topology/layout/domain types.

### Step 3. Extract runtime adapters

- `kithara-audio`: shared stream adapter, seek controller, boundary controller;
- `kithara-hls`: source/downloader/fetch submodules;
- `kithara-stream`: backend loop/scheduler split.

### Step 4. Shrink entry points

- `kithara-audio::Audio` becomes thin facade;
- `kithara-hls::Hls::create()` becomes thin builder call;
- `kithara-stream::Stream<T>` becomes thin reader/seek wrapper.

### Step 5. Delete obsolete compatibility layers

- remove legacy flat modules once new tree is stable;
- remove `download_state.rs`;
- remove any temporary compatibility `pub use` that widened visibility only to ease migration.

### Step 6. Move integration-like fixtures out of `src/`

- `kithara-hls`: move substantial source/downloader fixture-style tests out of `src/source.rs` and `src/downloader.rs` into `crates/kithara-hls/tests/`;
- `kithara-audio`: move any larger multi-step pipeline/worker fixtures out of `src/internal.rs` and `src/pipeline/*` when they stop being compact unit tests;
- keep `src/` focused on production modules and truly local unit tests only.

## 7. Encapsulation rules to enforce after split

- No runtime module may reach into another module's fields directly when a domain method can be introduced instead.
- Shared mutable state must live behind dedicated owner structs (`HlsSessionState`, `TrackControllerState`, etc.).
- `internal.rs` may expose test-only wrappers, but must not dictate production visibility design.
- Any module larger than roughly 400-500 lines after refactor needs a second look; for HLS runtime modules, anything above ~700 lines is a strong smell.

## 8. Validation

### Validation pass 1 checklist

- Does each target module have one reason to change?
- Are dependency rules directional and implementable with `pub(crate)`?
- Do large current files have a concrete split plan, not just “extract helpers”?
- Are pure domain modules separated from runtime IO modules?

Status: complete.

Results:

- added explicit split plans for currently underspecified large files: `crates/kithara-audio/src/resampler.rs`, `crates/kithara-hls/src/parsing.rs`, `crates/kithara-hls/src/context.rs`;
- confirmed that optional stream context belongs under `kithara-stream` diagnostics, not under stream construction API;
- confirmed that `download_state.rs` removal and `StreamIndex` consolidation remain explicit;
- added a dedicated test/fixture extraction step because current large HLS modules also carry substantial in-file test weight and that would otherwise preserve poor encapsulation.

### Validation pass 2 checklist

- Is any proposed module still a disguised god-module under a new name?
- Does any target tree preserve `internal.rs` as the real architecture instead of a test helper?
- Can the split happen incrementally without breaking every import at once?
- Are duplicate state models and duplicate ownership paths explicitly removed?
- Are shared cross-cutting types (`Fetch<T>`, `EpochValidator`, `ServiceClass`, `AudioWorkerSource`) placed in the right layer rather than hidden inside runtime blobs?

Status: complete.

Results:

- moved `Fetch<T>` and `EpochValidator` into stream `core/*` in the target tree because they are shared playback vocabulary, not downloader runtime internals;
- added explicit homes for `ServiceClass`, `TrackId` and `AudioWorkerSource`, which were previously implied but not placed in the module graph;
- independent review forced dedicated modules for worker registration and per-track slots in `kithara-audio`;
- independent review also forced a dedicated `processing/*` seam in audio and a dedicated `runtime/estimates/*` seam plus explicit public `api/context.rs` and `api/fetch.rs` surfaces in HLS;
- confirmed that the target tree now covers all currently oversized subsystems that materially affect playback architecture: stream backend, audio facade/source/FSM/worker/processing/resampler, HLS source/downloader/fetch/parsing/context/sizing.
