# Stream/Audio/HLS Interaction Refactor Plan
> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** убрать текущую архитектурную сцепку между `kithara-stream`, `kithara-audio` и `kithara-hls`, оставить у каждого крейта одну четкую ответственность и перевести их взаимодействие на узкие capability-oriented контракты без HLS-утечек в generic core.

**Architecture:** layered playback stack. `kithara-stream` становится transport/runtime core для синхронного byte-reading и shared timeline coordination. `kithara-audio` становится decode/control plane поверх `Read + Seek` и stream capabilities. `kithara-hls` становится конкретной segmented-source runtime-реализацией с собственной topology/fetch/scheduler моделью, скрытой за контрактами `kithara-stream`.

**Tech Stack:** Rust workspace crates `kithara-stream`, `kithara-audio`, `kithara-hls`, `kithara-events`, `kithara-assets`, `kithara-net`, `kithara-decode`, `kithara-storage`, `tokio`, existing `Timeline`, `Backend`, `Writer`, `EventBus`.

## 1. Problem Statement

Сейчас три крейта сцеплены в обе стороны:

- `kithara-stream` знает про `Audio::new()` через `StreamType::Events` и `StreamType::event_bus()`.
- `kithara-stream::Source` содержит HLS-specific методы: `current_segment_range`, `format_change_segment_range`, `clear_variant_fence`, `set_seek_epoch`, `demand_range`, `seek_time_anchor`, `commit_seek_landing`.
- `kithara-audio::Audio` одновременно является user-facing facade, consumer state machine host, worker/session owner и event integration point.
- `kithara-audio::StreamAudioSource` совмещает stream lock, decoder lifecycle, seek FSM, format-boundary handling и worker contract.
- `kithara-hls::Hls::create` собирает storage, DRM, fetch, playlist loading, event emission, source/downloader wiring и backend spawning в одном месте.
- `kithara-hls` держит две конкурирующие модели layout state: `download_state.rs` и `stream_index.rs`.
- `kithara-hls::PlaylistState` одновременно играет роль immutable topology и mutable runtime knowledge.

Следствие:

- generic stream layer заражен HLS semantics;
- audio pipeline обязан знать внутренности stream source;
- HLS runtime дублирует ownership и state transitions между `HlsCoord`, `HlsSource`, `HlsDownloader`, `FetchManager`, `PlaylistState`, `StreamIndex`;
- seek, ABR switch и decoder recreation не имеют единственного owner;
- инкапсуляция фактически отсутствует.

## 2. Current Interaction Map

### 2.1 Playback creation path

1. `Audio::<Stream<Hls>>::new(config)` создает `Stream<Hls>`.
2. `Stream::<Hls>::new()` вызывает `Hls::create(config)`.
3. `Hls::create()`:
   - строит `AssetStore`;
   - строит временный `FetchManager` для `KeyManager`;
   - строит основной `FetchManager`;
   - грузит master/media playlists;
   - строит `PlaylistState`;
   - выбирает variant;
   - создает/переиспользует `EventBus`;
   - вызывает `build_pair(...)`;
   - спавнит `Backend`.
4. `Audio::new()` снова вытаскивает bus через `StreamType::event_bus()` и строит собственный runtime вокруг `SharedStream`.

### 2.2 Playback data path

1. `Audio` consumer читает PCM chunks из worker.
2. Worker использует `StreamAudioSource`.
3. `StreamAudioSource` держит `SharedStream<T>`, который держит `Arc<Mutex<Stream<T>>>`.
4. `Stream<T>` делегирует чтение `Source::wait_range()` и `Source::read_at()`.
5. Для `HlsSource` это значит доступ к `HlsCoord`, `StreamIndex`, `FetchManager`, `PlaylistState` и event bus.
6. Downloader параллельно обновляет те же runtime structures.

### 2.3 Seek path

1. Audio инициирует seek через `Timeline`.
2. `StreamAudioSource` вызывает `seek_time_anchor()` и `commit_seek_landing()` через generic `Source`.
3. HLS source/resolution logic определяет segment boundary и variant/layout reset rules.
4. Decoder seek, source wake-up, pending demand, ABR fence clearing и seek completion reconciled в разных местах.

### 2.4 Format boundary / ABR path

1. `HlsSource::read_at()` поднимает variant fence.
2. `StreamAudioSource` отслеживает `media_info` и segment ranges.
3. При boundary audio pipeline сам решает, когда clear fence и recreate decoder.
4. HLS и audio jointly own codec-boundary transition, вместо одного explicit contract.

## 3. Target Responsibilities

### 3.1 `kithara-stream`

`kithara-stream` должен отвечать только за:

- shared playback timeline;
- byte-range readiness and sync read orchestration;
- generic downloader runtime primitives;
- generic topology/layout abstractions;
- capability-neutral source contracts.

`kithara-stream` не должен знать:

- что такое `EventBus`;
- как устроены HLS segment/variant transitions;
- как audio pipeline решает decoder recreation;
- какие source implementations segmented, а какие нет, кроме через optional capabilities.

### 3.2 `kithara-audio`

`kithara-audio` должен отвечать только за:

- decoder lifecycle;
- worker lifecycle and scheduling;
- track FSM;
- audio events;
- integration between decoder and stream capabilities.

`kithara-audio` не должен:

- зависеть от конкретного HLS type/fields;
- вытаскивать bus через `StreamType`;
- владеть source-specific wakeup/fence/layout logic как implicit knowledge.

### 3.3 `kithara-hls`

`kithara-hls` должен отвечать только за:

- HLS topology/model;
- fetch/cache/DRM/key resolution;
- HLS demand scheduling and ABR decisions;
- HLS seek mapping and layout maintenance;
- HLS source implementation поверх generic stream contracts.

`kithara-hls` не должен:

- протаскивать свои внутренности в `kithara-stream::Source`;
- делить ownership runtime state между несколькими равноправными объектами;
- держать параллельные модели layout/download state.

## 4. Target Cross-Crate Contracts

### 4.1 Replace one polluted `Source` with core + capabilities

`Source` нужно разрезать на:

- `ByteSourceCore`
  - `topology()`
  - `layout()`
  - `coord()`
  - `wait_range()`
  - `read_at()`
  - `phase_at()`
  - `len()`
  - `media_info()`
- `DemandDrivenSource`
  - `demand_range(range)`
- `SeekMappedSource`
  - `resolve_seek_anchor(position)`
  - `commit_seek_landing(anchor)`
- `FormatBoundarySource`
  - `current_segment_range()`
  - `format_boundary_range()`
  - `acknowledge_boundary()`
- `WakeableSource`
  - `notify_waiting()`
  - `make_notify_fn()`
- `SeekEpochSource`
  - `set_seek_epoch(epoch)`

Решение важно не ради “красоты”, а чтобы generic `Stream<T>` знал только про `ByteSourceCore`, а audio/HLS integration использовали дополнительные capability traits только там, где они реально нужны.

### 4.2 Narrow `StreamType`

`StreamType` должен оставить только:

- `Config`
- `Topology`
- `Layout`
- `Coord`
- `Demand`
- `Source`
- `Error`
- `create(config) -> Source`

Из `StreamType` нужно убрать:

- `Events`
- `event_bus(config)`
- `build_stream_context(source, timeline)`

Причина: это upward dependency на audio/UI diagnostics.

### 4.3 Move event bus ownership to composition root

Shared `EventBus` должен задаваться явно через config/composition root:

- `HlsConfig { bus: Option<EventBus> }` уже это умеет;
- `AudioConfig { bus: Option<EventBus> }` уже это умеет;
- если нужен общий bus, composition root передает один и тот же экземпляр в оба конфига;
- `Audio::new()` больше не должен извлекать bus из `StreamType`.

### 4.4 Replace `build_stream_context()` with explicit playback-context capability

`StreamContext` сейчас используется не только для diagnostics/UI, но и для корректного наполнения seek-related audio events. Поэтому простое удаление `StreamType::build_stream_context(...)` сломает текущую event semantics.

Нужен отдельный optional contract, например:

- `PlaybackContextSource`
  - `playback_context() -> SourcePlaybackContext`
- `SourcePlaybackContext`
  - `variant_index`
  - `segment_index`
  - optional `segment_range`

Ключевые правила:

- `Audio` использует этот contract только для event metadata и seek reporting;
- `HlsStreamContext` больше не строится через `StreamType`;
- owner context/meta adapter должен жить в HLS runtime/session layer, потому что он читает runtime layout (`StreamIndex`) и текущий ABR variant;
- audio-side код может принимать optional handle, но не должен конструировать его через generic stream trait.

### 4.5 Define atomic seek protocol

Текущий HLS seek живет на ordering contract, который нельзя потерять при рефакторинге.

Target seek protocol:

1. audio increments/propagates seek epoch;
2. source invalidates stale demand from the previous epoch;
3. source resolves seek anchor;
4. source wakes blocked waiters;
5. source may enqueue demand hint for the required range;
6. audio applies decoder seek/recreation;
7. audio commits actual landing back to source.

Нормативное требование:

- invalidation, wake-up и anchor resolution должны остаться частью одного explicit protocol;
- ни `Audio`, ни `HLS` не должны silently reorder эти шаги в разных модулях;
- этот protocol должен быть покрыт отдельными integration tests на concurrent seek.

### 4.6 Event publication policy

После выноса ownership `EventBus` в composition root нужно зафиксировать policy для ранних событий:

- `VariantsDiscovered` не должен быть единственным источником topology state;
- поздние subscribers должны иметь способ получить current topology snapshot либо через retained session snapshot, либо через explicit query API;
- replay/buffering policy для topology events должен быть отдельным решением, а не неявным последствием refactor-а.

## 5. Target Runtime Ownership Model

### 5.1 One session owner per runtime

Для HLS нужен явный `HlsSession` или `HlsRuntime`, который владеет:

- topology;
- mutable layout/index;
- fetch services;
- demand coordination;
- event emitter;
- downloader task handle lifecycle.

`HlsSource` и `HlsDownloader` должны быть thin endpoints над shared session handles, а не самостоятельными центрами принятия решений.

Shutdown contract:

- runtime/session owns cancellation root;
- source/session handles keep the last-drop shutdown guard or equivalent ownership token;
- dropping the last relevant source/session handle must still cancel downloader deterministically, preserving current source-drop behavior.

### 5.2 One owner for seek lifecycle

Seek flow должен иметь единственного owner:

- `Audio` инициирует seek и управляет decoder-side application;
- `HLS` отвечает только за mapping target time -> byte/segment anchor и за commit landed position обратно в layout/runtime state;
- `Timeline` остается глобальным источником истины по byte/committed playback position;
- `Audio` не должен решать HLS layout policy;
- `HLS` не должен решать decoder recovery policy.

### 5.3 One owner for format boundary transitions

Format boundary protocol должен быть explicit:

1. source сигнализирует boundary;
2. audio pipeline recreates decoder;
3. audio вызывает explicit boundary acknowledgment;
4. source снимает блокировку и продолжает чтение.

Boundary state не должен быть размазан между `media_info()` polling, `variant_fence`, `current_segment_range()` и implicit conventions.

## 6. Proposed End-State Interaction Flows

### 6.1 Creation

1. Composition root создает общий `EventBus` при необходимости.
2. `Hls::create(config)` строит `HlsSessionBuilder`.
3. Builder загружает topology/fetch dependencies и возвращает:
   - `HlsSession`;
   - `HlsSourceHandle`;
   - `HlsDownloaderHandle`.
4. `Stream<Hls>` получает только `HlsSource`.
5. `Audio::new(config)` получает уже готовый `Stream<T>`.
6. `Audio` создает `TrackSourceAdapter` на базе source capabilities, не зная про конкретный HLS type.

### 6.2 Normal playback

1. Decoder читает bytes через `Read + Seek`.
2. Если source range not ready, audio worker видит `SourcePhase` и optional `DemandDrivenSource`.
3. Audio FSM может отправить demand hint, но не знает scheduler internals.
4. Stream core остается neutral к segmented/non-segmented policy.

### 6.3 Seek

1. Audio инициирует seek и обновляет `Timeline`.
2. Если source implements `SeekMappedSource`, audio просит seek anchor.
3. Source возвращает deterministic anchor и metadata для decoder recreation.
4. Audio применяет seek/recreate.
5. После реального `decoder.seek()` audio вызывает `commit_seek_landing(anchor_or_actual)`.
6. HLS session reconciles landed byte/segment/variant state.

### 6.4 ABR / codec change

1. HLS source публикует format-boundary state через `FormatBoundarySource`.
2. Audio не опрашивает произвольные HLS fields, а работает через boundary contract.
3. После decoder recreation audio делает explicit `acknowledge_boundary()`.
4. HLS source продолжает чтение уже из нового variant layout.

## 7. Phased Refactor Plan

### Phase 1. Stabilize stream contracts

Files:

- `crates/kithara-stream/src/source.rs`
- `crates/kithara-stream/src/stream.rs`
- `crates/kithara-stream/src/lib.rs`
- `crates/kithara-audio/src/pipeline/audio.rs`
- `crates/kithara-audio/src/pipeline/source.rs`
- `crates/kithara-hls/src/context.rs`

Tasks:

- do a coordinated `kithara-stream` + `kithara-audio` change set, because capability methods are currently surfaced through `Stream<T>` and consumed directly by audio adapters;
- split `Source` into core and capability traits without changing runtime behavior;
- move stream-only read path to depend only on core trait;
- add temporary adapters to keep `HlsSource` and `FileSource` compiling while audio migrates to capability-based adapters;
- remove `Events`, `event_bus`, `build_stream_context` from `StreamType`;
- update `Audio::new()` to resolve bus only from `AudioConfig`.
- add an explicit playback-context adapter seam so `HlsStreamContext` can migrate out of `StreamType` without breaking current event/seek semantics.

Exit criteria:

- `Stream<T>` compiles without any event or diagnostics knowledge;
- `Audio` still works with HLS and file sources through compatibility adapters.

### Phase 2. Introduce explicit HLS session owner

Files:

- `crates/kithara-hls/src/inner.rs`
- `crates/kithara-hls/src/source.rs`
- `crates/kithara-hls/src/downloader.rs`
- `crates/kithara-hls/src/fetch.rs`
- new `crates/kithara-hls/src/session.rs`

Tasks:

- introduce `HlsSession`/`HlsRuntime` as единственный owner runtime state;
- move create-time assembly out of `Hls::create()` into builder/session;
- make `HlsSource` and `HlsDownloader` hold narrow handles into session;
- move event emission into session-owned emitter or dedicated adapter;
- preserve current downloader shutdown-on-source-drop semantics via explicit session ownership rules;
- stop passing raw shared state pieces as peer-owned fields.

Exit criteria:

- `Hls::create()` becomes thin assembly entry point;
- `build_pair(...)` is removed or reduced to trivial handle construction;
- no duplicated ownership of demand/layout/fetch state.

### Phase 3. Make audio a true control plane

Files:

- `crates/kithara-audio/src/pipeline/audio.rs`
- `crates/kithara-audio/src/pipeline/source.rs`
- `crates/kithara-audio/src/pipeline/track_fsm.rs`
- `crates/kithara-audio/src/pipeline/audio_worker.rs`

Tasks:

- turn `Audio` into thin facade over worker/track controller;
- move source capability usage into adapter objects;
- reduce `StreamAudioSource` to composition of `SharedStreamReader`, `DecoderSession`, `SeekController`, `BoundaryController`;
- make worker contract speak in track-domain concepts, not raw HLS/stream implementation details.

Exit criteria:

- `Audio` no longer owns protocol details for HLS wakeups, fences, or diagnostics;
- `StreamAudioSource` no longer serves as a god-object.

### Phase 4. Remove obsolete state models and compatibility shims

Files:

- `crates/kithara-hls/src/download_state.rs`
- `crates/kithara-hls/src/playlist.rs`
- `crates/kithara-hls/src/stream_index.rs`
- compatibility shims added in Phase 1

Tasks:

- pick `StreamIndex` as the only committed layout state model;
- split `PlaylistState` into:
  - immutable `PlaylistTopology`;
  - mutable `VariantSizingState` or equivalent runtime estimate store;
- delete `DownloadState`;
- delete temporary trait bridges and deprecated methods.

Exit criteria:

- one committed layout model;
- one topology model;
- one demand path;
- no legacy compatibility layer kept “for safety”.

## 8. Hard Architectural Rules After Refactor

- `kithara-stream` must not depend on `kithara-events`.
- `kithara-stream` public traits must not mention HLS concepts directly.
- `kithara-audio` may depend on stream capability traits, but not on HLS implementation structs.
- `kithara-hls` may depend on `kithara-stream`, but never the opposite.
- diagnostics/context objects must be optional adapters, not mandatory core contracts.
- one runtime state owner per crate-level subsystem.
- one source of truth per lifecycle: timeline, layout, topology, seek state, boundary state.

## 9. Main Risks

- Temporary adapter layer can accidentally preserve old coupling forever.
- Removing `event_bus()` from `StreamType` will ripple into tests and helper constructors.
- If capability split is done poorly, audio may still take a concrete dependency on `Stream<T>` internals instead of traits.
- If `HlsSession` is introduced without deleting duplicate state ownership, complexity will increase instead of decreasing.
- If playback-context replacement is treated as “diagnostics only”, seek lifecycle events will lose variant/segment semantics.
- If seek protocol ordering is not made explicit, concurrent seek can regress under stale demand and sleeping waiters.
- If source-drop shutdown ownership is not preserved, HLS downloader lifetime will change in subtle and dangerous ways.

## 10. Verification Strategy

- Unit tests around capability traits in `kithara-stream`.
- Contract tests for `Audio` with both HLS and file sources through the same track adapter API.
- HLS integration tests for:
  - seek across variant boundary;
  - ABR switch without codec change;
  - ABR switch with codec change;
  - seek after EOF;
  - stale demand invalidation by seek epoch.
- Compile-time verification that `kithara-stream` no longer imports `kithara-events`.

## 11. Validation

### Validation pass 1 checklist

- Does each crate have exactly one primary responsibility in the target model?
- Are all current cross-crate couplings addressed by an explicit replacement contract?
- Is there a single owner for seek, boundary and layout lifecycle?
- Is migration ordered so the workspace can continue compiling between phases?

Status: complete.

Results:

- confirmed that the current upward couplings from `kithara-stream` are exactly `event_bus()` and `build_stream_context()` in `crates/kithara-stream/src/stream.rs`;
- confirmed that `HlsStreamContext` is not just observability sugar and must be replaced by an explicit playback-context/session-owned adapter because it reads `StreamIndex` and ABR variant state from `crates/kithara-hls/src/context.rs`;
- confirmed that the plan explicitly replaces all HLS-flavored `Source` methods currently defined in `crates/kithara-stream/src/source.rs`;
- adjusted Phase 1 to include a coordinated `stream` + `audio` migration and a playback-context seam so the contract split is implementable incrementally.

### Validation pass 2 checklist

- Does the plan accidentally preserve hidden upward dependencies from stream to audio/HLS?
- Does any proposed contract leak HLS vocabulary into generic core?
- Can the plan be implemented incrementally without a flag day rewrite?
- Are obsolete state models (`DownloadState`, event extraction via `StreamType`) explicitly removed?
- Is diagnostics ownership explicit enough that `HlsStreamContext` does not silently recreate the old `StreamType` coupling under a new name?

Status: complete.

Results:

- the plan remains incremental: Phase 1 isolates contracts first, Phase 2 introduces `HlsSession`, Phase 3 shrinks audio control plane, Phase 4 removes legacy state models;
- HLS vocabulary is confined to optional capability contracts and HLS session/runtime layers; generic stream core keeps only byte/timeline/layout abstractions;
- obsolete upward dependencies are explicitly removed: `StreamType::Events`, `event_bus()`, `build_stream_context()`;
- playback-context ownership is now explicit: HLS session owns context/meta creation, while audio consumes it only as an optional adapter;
- independent review also forced the plan to spell out seek ordering, source-drop shutdown ownership and the split of mutable sizing state out of `PlaylistState`.
