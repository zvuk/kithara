# Kanban — `kithara-hls`

> Доп. артефакты для портирования legacy-идей (агенты не имеют доступа к другим репо):
> - `docs/porting/legacy-reference.md` (общий backlog + мотивация)
> - `docs/porting/source-driver-reference.md` (portable: driver loop, cancellation via drop, offline miss semantics)
> - `docs/porting/hls-vod-basic-reference.md` (portable: VOD basic playback checklist + minimal test matrix; DRM учтён, но не блокер)
> - `docs/porting/abr-reference.md` (ABR: estimator/controller/decision-vs-applied/tests)
> - `docs/porting/drm-reference.md` (DRM: AES-128 decrypt, processed keys caching, offline rules, fixtures)


Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-hls`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-hls` — HLS VOD источник байтов с “player-grade” семантикой и обязательной интеграцией с persistent кэшем:

- поддержка **только VOD** (live не нужен, но дизайн не должен блокировать добавление позже),
- парсинг master/media playlists,
- выбор variant (manual/auto ABR),
- загрузка сегментов, init-сегментов и ключей (DRM),
- корректная семантика EOF/backpressure/ошибок,
- offline режим: кэшируем всё необходимое для воспроизведения без сети; cache miss в offline → **fatal**.

---

## Контракт / инварианты (обязательные)

- HLS — это дерево ресурсов (playlists/segments/keys/variants). Внутренне остаётся “ресурсным”, а “байтовая лента” — лишь представление.
- `Read::read() -> Ok(0)` означает **EOF** для потребителей. Нельзя возвращать `Ok(0)` пока поток реально не завершён (см. `docs/constraints.md`).
- Backpressure обязательна: потребление памяти ограничено; при заполнении буферов writer блокируется/дросселируется (через `kithara-io` и/или внутренние механизмы).
- Ошибки: различать recoverable vs fatal, и иметь **явный** контракт распространения ошибок.
- URL идентификация:
  - `asset_id`: canonical URL без query/fragment,
  - `resource_hash`: canonical URL с query, без fragment.
- DRM keys:
  - key может быть “wrapped”; применяется `key_processor_cb`,
  - **кэшируем processed keys** (после `key_processor_cb`),
  - не логировать секреты (ключи/токены/байты ключей).
- ABR:
  - throughput обновляется только по реальным network downloads,
  - cache hits не улучшают estimator (иначе offline ломается).
- В бесконечном цикле загрузки (driver loop) живёт оркестрация HLS (не в decode thread).

---

## Компоненты и воркеры (контракт) — напоминание

Ожидаемые подсистемы (структура может эволюционировать, но ответственность должна быть ясной):

- `playlist`: парсинг master/media playlists, извлечение сегментов/keys/init.
- `fetch`: получение ресурсов (через `kithara-net`) с учётом offline/timeout/retry политики.
- `keys`: загрузка ключей, применение `key_processor_cb`, кэширование processed keys.
- `drm` (может быть частью `fetch`/`keys`, но ответственность должна быть явной): AES-128 decrypt сегментов, ошибки decrypt — fatal.
- `abr`: политика выбора variant (manual/auto), throughput estimator, переключения.
- `driver/worker`: orchestration loop (VOD), выдача байтового потока и управление жизненным циклом.
- `events` (если экспонируется): best-effort события (например, segment start / variant changed) — **не** использовать для строгих инвариантов, если семантика не “applied”.

См. portable reference:
- Source/Driver contract: `docs/porting/source-driver-reference.md`
- VOD basic playback spec: `docs/porting/hls-vod-basic-reference.md`
- ABR: `docs/porting/abr-reference.md`
- DRM: `docs/porting/drm-reference.md`

---

## Публичный контракт (экспорт) — ориентир

Точная форма API фиксируется в коде `crates/kithara-hls`, но этот борд предполагает наличие:

- `HlsOptions`/`HlsSettings` (или эквивалент):
  - `base_url` override,
  - variant selector (manual/auto),
  - ABR тюнинг (включая `abr_initial_variant_index`),
  - key request headers/query params,
  - `key_processor_cb`,
  - offline_mode политика (cache miss => fatal).
- Session/handle, который даёт байтовый stream (async) + control plane (stop/seek, если предусмотрено контрактом).

---

## `kithara-hls` — VOD basic playback (segment loop, EOS, fixture counters, manual selection) — MUST HAVE

Reference: `docs/porting/hls-vod-basic-reference.md`

- [ ] Fixture supports deterministic VOD shape (mock mode):
  - master: `/master.m3u8`
  - media: `/v{variant}.m3u8`
  - segments: `/seg/v{variant}_{i}.bin` with plaintext prefix `V{variant}-SEG-{i}:`
  - request counters per path
- [ ] Driver streams **segments** end-to-end (not just playlists):
  - master → select variant → media playlist → (init?) → segment loop → bytes out → EOS
  - stream closes after last segment (consumer sees `None`)
  - test: `hls_vod_completes_and_stream_closes`
- [ ] Driver fetches **all** segments for selected variant:
  - assert fixture counters for every `/seg/v{variant}_{i}.bin` > 0
  - test: `hls_vod_fetches_all_segments_for_selected_variant`
- [ ] Manual variant selection outputs only selected variant bytes:
  - run across multiple variant indices (e.g. 0..3)
  - assert observed payload prefixes belong to selected variant
  - test: `hls_manual_variant_outputs_only_selected_variant_prefixes`
- [ ] Cancellation via drop (Stop не обязателен):
  - read N bytes then drop stream/session
  - driver stops promptly and stops making further requests
  - test: `hls_drop_cancels_driver_and_stops_requests`
- [ ] DRM is not a blocker for basic playback:
  - basic fixture is plaintext (no EXT-X-KEY) OR
  - if EXT-X-KEY is present and decrypt not implemented yet: fail deterministically with typed `Unimplemented`
  - add a small doc note in crate-level docs on this staged approach

---

## `kithara-hls` — Реальная реализация VOD driver loop (segment streaming) + EOS + cancellation via drop

Reference: `docs/porting/source-driver-reference.md`

- [ ] End-to-end VOD segment streaming (not just playlists):
  - driver: master → select variant → media playlist → segment loop → bytes out → EOS
  - stream must output segment bytes (deterministic prefixes), and close after last segment
  - test: `hls_vod_completes_and_stream_closes` (local fixture only)
- [ ] Segment loop fetches ALL segments for selected variant:
  - assert per-segment request counters >= 1 (fixture counts requests)
  - test: `hls_vod_fetches_all_segments_for_selected_variant`
- [ ] Cancellation via drop (Stop не обязателен):
  - consumer reads N bytes and drops stream/session
  - driver must terminate promptly (no hang) and stop making further requests
  - test: `hls_drop_cancels_driver_and_stops_requests`
- [ ] Offline mode: cache-first + miss is fatal:
  - offline_mode=true and any playlist/segment/key miss => fatal `OfflineMiss` (or equivalent) and stream closes
  - test: `hls_offline_miss_is_fatal`
- [ ] URL resolution contract is explicit + tested:
  - relative URLs resolved against playlist URL (and/or base_url override) exactly as contract defines
  - test: `hls_base_url_override_remaps_segments_keys_and_playlists`

---

## `kithara-hls` — DRM обязателен (без feature flags) + Port legacy DRM scenarios (tests)

Reference: `docs/porting/drm-reference.md`

- [ ] DRM is mandatory (no feature gating):
  - убедиться, что baseline DRM (AES-128) не спрятан за `cfg(feature=...)`
  - если код уже без фич — добавить crate-level docs/комментарий в README крейта или в `docs/` (в рамках крейта) и тесты ниже
- [ ] AES-128 decrypt pipeline is implemented end-to-end (smoke):
  - fixture serves encrypted segments + key endpoint
  - output bytes include expected plaintext prefixes for at least first segment
- [ ] AES-128 with explicit IV decrypts correctly (deterministic IV):
  - playlist задаёт `IV=0x...`
  - тест проверяет plaintext prefix (без зависимости от padding/TS деталей)
- [ ] Key fetch applies query params + headers + key_processor:
  - server requires specific query params and headers to return a wrapped key
  - client applies `key_processor_cb` to unwrap
  - decrypted segment payload is correct
- [ ] Processed key is cached (and raw wrapped key is NOT persisted):
  - first run online fills cache
  - second run offline uses cached processed key and decrypts successfully
- [ ] Key is cached and not fetched per segment:
  - read enough bytes to span multiple segments
  - assert key endpoint request count is low (target <= 1; допускается <= 2 только если контракт/гонка зафиксированы тестом)
- [ ] Required key headers missing => fatal error:
  - when server requires header and client does not send it, session fails deterministically
- [ ] Offline mode missing key => fatal error:
  - offline_mode=true, segments exist, processed key missing => `OfflineMiss` (или эквивалент) и корректное завершение
- [ ] (Prereq if missing) Ensure decrypted output can be validated deterministically:
  - segment payloads contain stable prefixes per (variant, segment)

---

## `kithara-hls` — Port legacy base_url and URL resolution scenarios (tests)

- [ ] base_url override:
  - when segments are remapped under a path prefix, default resolution fails (404)
  - configuring base_url makes it succeed for varying prefix depths (`a/`, `a/b/`, `a/b/c/`)
- [ ] (Prereq if missing) Ensure URL resolution rules are explicitly testable:
  - base URL override applied for variant playlists, segments, and keys as per contract

---

## `kithara-hls` — Port legacy VOD completion + caching surface scenarios (tests)

- [ ] VOD completes and closes stream:
  - drain until stream ends; assert all segments for selected variant were requested at least once
- [ ] Manual variant selection emits only selected variant data:
  - start in manual mode for different variant ids
  - verify output “belongs” to that variant (by deterministic payload prefixes)
- [ ] AUTO startup begins on variant 0 by default
- [ ] AUTO startup respects `abr_initial_variant_index`

---

## `kithara-hls` — ABR “как в legacy” (estimator/controller; decision vs applied) + switching scenarios (tests)

Reference: `docs/porting/abr-reference.md`

### Unit-ish (ABR in isolation)
- [ ] Estimator ignores cache hits:
  - introduce `ThroughputSampleSource { Network, Cache }` (или эквивалент)
  - update estimator only on `Network`
  - test: `cache_hit_does_not_affect_throughput`
- [ ] Controller downswitch after low throughput sample:
  - feed throughput estimate/sample that should force downswitch
  - assert decision targets lower variant, reason фиксируется
- [ ] Controller upswitch respects buffer gating + hysteresis:
  - low buffer => no upswitch
  - sufficient buffer + enough throughput => upswitch
- [ ] Controller min_switch_interval prevents oscillation:
  - decision suppressed when interval not elapsed

### Integration-ish (ABR apply in worker)
- [ ] Decision vs applied semantics are explicit:
  - expose/emit `VariantApplied` (или эквивалент) only after actual switch is applied
  - tests must not rely on out-of-band telemetry ordering
- [ ] ABR upswitch continues from current segment index (no restart)
- [ ] ABR downswitch begins correctly (init segment behavior fixed by contract):
  - if switching requires init, ensure next output begins with init for new variant
- [ ] Worker auto switches mid-stream without restarting:
  - observe events + deterministic payload prefixes and ensure segment index sequence is non-decreasing across switch
- [ ] (Prereq if missing) Expose a deterministic ABR controller surface for tests:
  - feed throughput samples (explicit source Network/Cache)
  - obtain decisions
  - apply decisions in worker at segment boundaries
  - observe selected variant and next segment descriptors

---

## `kithara-hls` — Port legacy seek scenarios (tests)

- [ ] Seek correctness (best-effort, absolute) across segment boundary:
  - seek to offset that straddles end of segment N and beginning of segment N+1
  - read contiguous bytes and assert they match expected concatenation
- [ ] Seek known offsets return expected bytes:
  - after a warmup read, seek to known offsets and read exact prefixes (init/segment slices)
- [ ] (Prereq if missing) Define/confirm seek contract for HLS:
  - absolute seek by time or bytes (as per current public API)
  - behavior across segment boundaries is contiguous and deterministic within fixture constraints

---

## `kithara-hls` — Prerequisite tasks for ported scenarios (only if missing in plan)

- [ ] Add test fixture utilities for HLS:
  - local server that can serve master/media playlists, init segments, media segments
  - deterministic payload prefixes per variant/segment (e.g. `V{v}-SEG-{i}`)
  - request counters per path
- [ ] Add event surface needed by tests:
  - expose best-effort `SegmentStart` / `VariantChanged` events (or equivalent) in a stable way for tests
- [ ] Ensure `HlsOptions` includes:
  - `base_url` override
  - `abr_initial_variant_index`
  - key request headers/query params and `key_processor_cb`
  - offline_mode behavior (cache miss => fatal)

---

## `kithara-hls` — Refactor (manager/worker/policies; keep public contract)

- [x] Split into modules for `fixture` (test-only), `playlist`, `fetch`, `keys`, `abr`, `driver/worker`, `events` as size grows (no behavior change) + keep tests green
- [ ] Ensure ordered control-plane/data-plane semantics remain explicit (avoid out-of-band surprises) + add focused tests
- [ ] Add crate-level docs: URL resolution rules (`base_url`), DRM key processing/caching, ABR switching invariants

---

## Sanity check (после ужесточения fmt+clippy)

- [ ] Remove new clippy warnings/errors introduced by stricter workspace lints
- [ ] Run `cargo fmt` and ensure no formatting diffs remain
- [ ] Run `cargo test -p kithara-hls`
- [ ] Run `cargo clippy -p kithara-hls` and make it clean (no warnings/errors) under workspace lints
