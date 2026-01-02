# Kanban — `kithara-hls`

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
- `abr`: политика выбора variant (manual/auto), throughput estimator, переключения.
- `driver/worker`: orchestration loop (VOD), выдача байтового потока и управление жизненным циклом.
- `events` (если экспонируется): best-effort события (например, segment start / variant changed) — **не** использовать для строгих инвариантов, если семантика не “applied”.

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

## `kithara-hls` MVP

- [x] parse master/media playlists (VOD) + tests with local fixture
- [x] variant selection policy (parameter) + tests
- [x] resource caching (playlists/init/segments) + offline test
- [x] keys + processed keys caching + test scenario with wrapped key
- [x] stream bytes for a variant + basic read smoke test

---

## `kithara-hls` — Port legacy DRM scenarios (tests)

- [ ] AES-128 DRM decrypts media segments (smoke):
  - fixture serves encrypted segments + key endpoint
  - output bytes include expected plaintext prefixes for at least first segment
- [ ] AES-128 DRM with fixed zero IV decrypts correctly
- [ ] Key fetch applies query params + headers + key_processor:
  - server requires specific query params and headers to return a wrapped key
  - client applies `key_processor_cb` to unwrap
  - decrypted segment payload is correct
- [ ] Key is cached and not fetched per segment:
  - read enough bytes to span multiple segments
  - assert key endpoint request count is low (<=2)
- [ ] Required key headers missing => fatal error:
  - when server requires header and client does not send it, session fails deterministically
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

## `kithara-hls` — Port legacy ABR + switching scenarios (tests)

- [ ] ABR downswitch after low throughput sample:
  - feed throughput sample that should force downswitch
  - assert next descriptor targets lower variant and begins with init
- [ ] ABR upswitch continues from current segment index (no restart)
- [ ] Worker auto upswitches mid-stream without restarting:
  - observe VariantChanged + SegmentStart events and ensure sequence is non-decreasing across switch
- [ ] (Prereq if missing) Expose a deterministic ABR controller surface for tests:
  - feed throughput samples
  - obtain/apply decisions
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