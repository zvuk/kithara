# Legacy reference → Kithara porting backlog (ABR, DRM, Decode layering, Net generics)

Этот документ — **портируемый “reference pack”** для автономных агентов, которые **не имеют доступа** к репозиториям `stream-download-rs` и `decal`.

Цели:
1) зафиксировать, **что именно “не так”** в текущих реализациях `kithara-*` относительно твоих исходных идей;
2) дать агентам **конкретные артефакты**, на которые они могут опираться при рефакторинге/переписывании;
3) разложить работу на задачи, которые удовлетворяют правилу **“одно изменение — один крейт”** (`AGENTS.md`).

---

## 0.1) Локальные “копии legacy артефактов” внутри `kithara` (агенты должны читать это вместо внешних репо)

Чтобы агенты могли портировать поведение и тесты без доступа к другим репозиториям, в `kithara/docs/porting/` лежат копии ключевых legacy файлов:

- `docs/porting/legacy-stream-download-hls-lib.rs` — публичные ре-экспорты и модульная карта `stream-download-hls`.
- `docs/porting/legacy-stream-download-hls-tests.rs` — **полный** legacy-набор интеграционных тестов по HLS (VOD, base_url, ABR, DRM, seek, storage backends).
- `docs/porting/legacy-stream-download-lib.rs` — reference по “streaming/read/seek” контрактам `stream-download` (как устроен Read+Seek поверх async source).

Дополнительно: curated “кодовые сниппеты” (ключевые части кода, чтобы агент мог подсмотреть паттерны, не имея доступа к внешним репо):
- `docs/porting/legacy-snippets/README.md` — индекс: что где смотреть и что именно перенимать (без копипасты 1:1).

Decal (decode layering, Source/reader+hint):
- `docs/porting/legacy-snippets/decal-lib.rs`
- `docs/porting/legacy-snippets/decal-decoder-mod.rs`
- `docs/porting/legacy-snippets/decal-decoder-source.rs`

stream-download-hls ABR (estimator/controller split + EWMA):
- `docs/porting/legacy-snippets/stream-download-hls-abr-mod.rs`
- `docs/porting/legacy-snippets/stream-download-hls-abr-controller.rs`
- `docs/porting/legacy-snippets/stream-download-hls-abr-estimator.rs`
- `docs/porting/legacy-snippets/stream-download-hls-abr-ewma.rs`

stream-download-hls downloader (decorator/layered pattern: base → timeout → retry → cache):
- `docs/porting/legacy-snippets/stream-download-hls-downloader-mod.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-traits.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-types.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-builder.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-base.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-timeout.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-retry.rs`
- `docs/porting/legacy-snippets/stream-download-hls-downloader-cache.rs`

Как этим пользоваться (нормативно):
- Агент **не копирует** код 1-в-1, а переносит **контракты и поведение**, зафиксированные тестами.
- Любой перенос делается через TDD: сначала новый тест в `kithara-*`, затем реализация.
- Нельзя менять публичный контракт `kithara-*` “под legacy”. Мы переносим legacy-поведение **внутрь** уже заложенной архитектуры.

Доп. portable спецификации (это “истина” для kithara, а legacy — референс):
- `docs/porting/source-driver-reference.md` (driver loop, cancellation via drop, offline miss fatal)
- `docs/porting/hls-vod-basic-reference.md` (VOD basic playback: segment loop + EOS + counters; DRM учтён, но не блокер)
- `docs/porting/downloader-reference.md` (downloader/decorator pattern для fetch stack: base → timeout → retry → cache → transform; где должен жить cache и где decrypt)
- `docs/porting/decal-reference.md` (decal-style слойность decode: Source → Engine → Decoder → Pipeline; границы между source/driver и decode)
- `docs/porting/abr-reference.md`
- `docs/porting/drm-reference.md`
- `docs/porting/decode-reference.md`
- `docs/porting/net-reference.md`

---

## 0) Контекст и правила (обязательно)

Перед любой работой агент читает:
- `AGENTS.md` (границы изменений, зависимости, TDD, imports)
- `docs/constraints.md` (EOF/backpressure, offline, ABR, DRM keys, identity)
- `docs/kanban.md` + board целевого крейта

Ключевые правила для всех задач ниже:
- **TDD-first**: тест(ы) → падение → код → зелёные тесты → рефактор.
- **DRM обязателен**: никаких feature-flag’ов для DRM. Если слой нужен — он всегда включён.
- **No unwrap/expect** в прод-коде.
- **Read::read() == Ok(0) только при истинном EOF**.

---

## 1) Что сейчас неверно / недостаточно близко к legacy

### 1.1 ABR (HLS)
Проблемы “хуже legacy” обычно выглядят так:
- ABR контроллер существует, но **не принимает решений** (заглушки или “return current”).
- Estimator/контроллер не разделены, нет “decision vs applied”.
- Нет чёткого контракта, что throughput обновляется **только по сетевым загрузкам** (см. `docs/constraints.md#6`).
- Нет детерминированной поверхности для тестов: невозможно “подать sample → получить decision → применить”.

**Ожидаемый legacy-подход** (концептуально):
- `Estimator`: EWMA (или аналог) по throughput sample’ам.
- `Controller`: принимает решение `AbrDecision` на основе:
  - throughput estimate * safety_factor,
  - текущего варианта + hysteresis,
  - min switch interval,
  - buffer level (или эвристика).
- ABR возвращает *решение* и *причину*, а применение происходит в worker/driver, после чего генерируется “applied event”.

### 1.2 DRM (HLS)
Проблемы:
- DRM не реализован полностью (или реализован условно через feature).
- Нет обязательной AES-128 расшифровки сегментов (baseline HLS).
- Ключи могут кэшироваться “как пришли”, а должны кэшироваться **после `key_processor_cb`** (“processed keys”), иначе offline ломается.

**Ожидаемый legacy-контракт**:
- ключ может быть “wrapped”, пользователь передаёт `key_processor_cb(raw_key, KeyContext) -> processed_key`.
- в кэш пишется **processed_key**.
- key request умеет добавлять headers/query.
- в offline: cache miss по ключу/сегменту/плейлисту = **fatal**.

### 1.3 Decode layering (decal-like)
Проблемы:
- Декодер может быть написан “в один слой”, без чёткой границы:
  - `Source` / `MediaSource` abstraction,
  - engine (Symphonia glue),
  - state machine (Decoder),
  - async pipeline (bounded queue, commands).
- Может отсутствовать “driver loop” на отдельном потоке/worker’е.

**Ожидаемая архитектура (слоями)**:
1) `Source`/`MediaSource` (sync `Read+Seek` + `file_ext()` hint).
2) `DecodeEngine`: низкоуровневое взаимодействие с Symphonia (open/read/seek/reset).
3) `Decoder<T>`: state machine, ошибки/EOF semantics, reset при codec changes.
4) `AudioStream`/pipeline: async consumer API + bounded queue + command channel.

### 1.4 Net generics (stream-download style)
Проблемы:
- Net клиент может быть “вроде layered”, но не даёт удобной композиции/расширяемости.
- Retry/timeout могут быть встроены в один слой вместо decorator’ов.
- Отсутствует минимальная типизированная поверхность, на которую HLS может опираться (stateless net; кэширование не в net).

**Ожидаемый подход**:
- trait `Net` + `NetExt`.
- `ReqwestNet` (base) + `TimeoutNet<N>` + `RetryNet<N,P>`.
- Retry только до начала body streaming (v1), нет mid-stream retry.
- Никаких stateful caches в net.

**Важно про orchestration (актуальная архитектура kithara):**
- общий driver loop / orchestration слой вынесен в `kithara-stream`;
- `kithara-file` и `kithara-hls` реализуют `kithara-stream::Source` (то есть описывают “что качать и в каком порядке”),
- а `kithara-stream::Stream` отвечает за lifecycle, cancellation via drop и обработку команд (на текущем этапе `SeekBytes` может быть `SeekNotSupported`).

---

## 2) Portable “reference spec” (то, что агент может реализовать без доступа к legacy)

### 2.1 ABR reference model (portable)
**Термины:**
- `Variant` имеет (как минимум) `bandwidth_bps` (или bitrate) и индекс.
- `ThroughputSample`: `bytes`, `duration`, `source` (Network/Cache), `ts`.

**Правила:**
- estimator обновляется **только** при `source == Network`.
- применяем safety factor: `effective_throughput = estimate / safety_factor` (или `estimate * (1/safety)`).
- hysteresis:
  - upswitch только если `effective_throughput > next_bandwidth * up_ratio`
  - downswitch если `effective_throughput < current_bandwidth * down_ratio`
- `min_switch_interval`: не принимать новое решение раньше.
- `buffer_level_secs`: upswitch допускается только при достаточном буфере; downswitch может быть агрессивнее.

**ABR output:**
- `AbrDecision`:
  - `target_variant_index`
  - `reason` (ManualOverride / UpSwitch / DownSwitch / NoData / MinInterval / BufferLow / etc.)
  - `require_init: bool` (если смена variant/codec требует init)

**Decision vs Applied:**
- `decide()` возвращает *decision*.
- `apply(decision)` в driver/worker меняет pipeline, и только тогда генерируется событие “VariantApplied”.

### 2.2 DRM reference model (portable)
**Supported baseline:**
- HLS AES-128 (METHOD=AES-128) CBC, ключ 16 bytes.
- IV:
  - если явно указан в playlist → использовать его,
  - если отсутствует → IV derived (часто sequence number; но для тестовых фикстур можно фиксировать IV).
- Decrypt сегмента:
  - получить key bytes (processed),
  - расшифровать AES-128-CBC,
  - отдавать plaintext downstream.

**Caching:**
- `processed_key` кэшируется отдельно (и используется для offline).
- В логах запрещены raw key bytes.

### 2.3 Decode layering reference model (portable)
**Interfaces:**
- `Source`/`MediaSource`:
  - `reader() -> Box<dyn Read + Seek + Send + Sync>`
  - `file_ext() -> Option<&str>`
- `Decoder<T>`:
  - `next() -> Result<Option<PcmChunk<T>>, DecodeError>`
  - `handle_command(Seek(Duration))`
- Pipeline:
  - bounded queue for `PcmChunk<T>`,
  - separate command channel,
  - worker thread: sync decoding loop; async front-end consumes.

**Critical invariant:**
- bridge к `Read` никогда не возвращает `Ok(0)` пока поток не завершён.

### 2.4 Net layering reference model (portable)
- `Net::stream(url, headers)` возвращает stream chunks `Bytes`.
- `Net::get_range(url, range, headers)` для seek.
- `TimeoutNet`:
  - `get_bytes`: таймаут всей операции,
  - `stream/get_range`: таймаут на request+headers, а body stream идёт без таймаута (или отдельный явный контракт).
- `RetryNet`:
  - retry только до начала body stream.
  - ретраится при: network errors, 5xx, 429 (408 — решается тестом).

---

## 3) Новые задачи для агентов (backlog по крейтам)

Ниже — **новые задачи**, которые стоит добавить/уточнить в соответствующих `docs/kanban-kithara-*.md`.
Каждый пункт должен стать чекбоксом `[ ]` и выполняться агентом строго в рамках одного крейта.

### 3.1 `kithara-hls` — ABR “как в legacy” (реализация + тесты)
1) **ABR decomposition**
   - [ ] Разделить ABR на `estimator` и `controller` (если сейчас слито) без изменения публичного API.
   - Артефакт: этот документ, раздел 2.1.

2) **Network-only samples**
   - [ ] Ввести тип `ThroughputSampleSource { Network, Cache }` или эквивалент.
   - [ ] Обновлять estimator только по `Network`.
   - Тест: “cache sample does not change estimate”.

3) **Deterministic ABR surface**
   - [ ] Сделать API для тестов: “подать sample → получить decision → применить”.
   - Тест: “downswitch after low throughput sample”.
   - Тест: “upswitch respects min_switch_interval”.

4) **Decision vs Applied event**
   - [ ] Ввести событие “VariantApplied” (или чётко документировать существующее), которое эмитится только после фактического применения.
   - Тест: “VariantChanged decision does not imply applied” (если есть телеметрия) или наоборот фиксируем правильную семантику.

5) **Hysteresis + safety factor**
   - [ ] Реализовать hysteresis правила из 2.1.
   - Тесты: up/down thresholds.

### 3.2 `kithara-hls` — DRM обязательный (AES-128 decrypt)
1) **Remove feature gating (если есть)**
   - [ ] Убедиться, что DRM слой всегда включён (никаких `cfg(feature=...)` на core DRM функциональность).
   - Если в текущем `kithara-hls` уже “без фич” — задача сводится к проверке/документации.

2) **Decrypt middleware**
   - [ ] Реализовать AES-128-CBC decrypt сегментов в pipeline fetch→output.
   - Тест: fixture с encrypted segment + key endpoint, проверяем plaintext prefix.

3) **Processed keys caching**
   - [ ] Гарантировать, что в кэш записываются processed keys (после `key_processor_cb`).
   - Тест: key endpoint вызывается 1 раз, затем offline воспроизведение проходит.

4) **Key request semantics**
   - [ ] Query params + headers применяются при key fetch.
   - Тест: сервер требует header+query, иначе 403/400.

### 3.3 `kithara-decode` — перенести многослойность “decal style” (с фиксацией контракта)
1) **Source abstraction alignment**
   - [ ] Если в `kithara-decode` до сих пор используется `MediaSource` в стиле “reader()”, привести к контракту “Source + file_ext hint” (или хотя бы документировать и довести до decal-like).
   - Артефакт: раздел 2.3.

2) **Engine/Decoder boundaries**
   - [ ] Убедиться, что `DecodeEngine` содержит Symphonia-детали, а `Decoder<T>` — state machine.
   - Тест: “seek does not deadlock”.
   - Тест: “codec reset required surfaces as error (or handled)”.

3) **Pipeline correctness**
   - [ ] bounded queue + command channel: нет утечек памяти, нет deadlock.
   - Тест: producer blocks when queue full (если тестируемо), или как минимум deterministic “buffer limit respected”.

### 3.4 `kithara-net` — generics-first клиент (если архитектурно “не дотянуто”)
По текущему board многие пункты уже отмечены как сделанные, но если тебе кажется, что реализация “не как legacy”, то это обычно про **эргономику и тестируемость**:
- [ ] Добавить/улучшить “contract tests” для retry classification по статусам.
- [ ] Уточнить документ `crates/kithara-net/README.md`: что именно таймаутится, что ретраится, и что нет mid-stream retry.

---

## 4) Артефакты, которые агентам нужны для реализации (внутри `kithara`)

Этот документ — “главный reference”. В `kithara/docs/porting/` есть два типа артефактов:

### 4.1 Portable specs (нормативно для kithara)
Эти документы задают “как должно быть” в `kithara` (не legacy):
- `abr-reference.md`: формулы/правила estimator/controller + таблица тест-кейсов.
- `drm-reference.md`: AES-128-CBC contract + fixture layout + правила IV + processed keys caching.
- `decode-reference.md`: слойность decode, последовательность вызовов, инварианты EOF/backpressure.
- `decal-reference.md`: пояснение “decal-style” слойности и границы интеграции: sources/drivers (async bytes) ↔ bridge (`kithara-io`) ↔ decode (`kithara-decode`).
- `net-reference.md`: retry/timeout matrix + no mid-stream retry v1.
- `downloader-reference.md`: reference по downloader/decorator pattern для resource fetching: base HTTP + timeout + retry + cache + optional transforms (DRM decrypt) с детерминированными тестами.
- `source-driver-reference.md`: execution model источников, cancellation via drop, offline miss semantics.
- `hls-vod-basic-reference.md`: минимальный “basic playback” для HLS VOD: segment loop + EOS + request counters; DRM учтён, но не блокер.

### 4.2 Copied legacy artifacts (референс поведения/тестов)
Эти файлы — “снимок” legacy, чтобы агенты могли портировать тестовые сценарии без доступа к внешним репо:
- `legacy-stream-download-hls-lib.rs`
- `legacy-stream-download-hls-tests.rs`
- `legacy-stream-download-lib.rs`

Нормативное правило использования legacy артефактов:
- legacy служит **источником сценариев и ожиданий** (особенно для тестов),
- но если portable spec и legacy расходятся — приоритет у portable spec и `docs/constraints.md`.

### 4.3 Как использовать “decal” и “downloader” артефакты (обязательная методология)
Эти два документа нужны, чтобы агенты не “достраивали архитектуру по наитию”, а попадали в исходные идеи:

- `docs/porting/decal-reference.md`:
  - используется при любых задачах в `kithara-decode` и `kithara-io`;
  - фиксирует границу: sources/drivers делают async orchestration и дают async bytes; decode потребляет sync `Read+Seek`;
  - подчёркивает критичный инвариант `Read::read() -> Ok(0)` == true EOF.

- `docs/porting/downloader-reference.md`:
  - используется при задачах в `kithara-hls` и `kithara-file` (реальная реализация segment/file streaming);
  - описывает “fetch stack” как слои/декораторы (base → timeout → retry → cache → transform);
  - объясняет, почему cache должен оставаться в source crates (а `kithara-net` быть stateless);
  - показывает, как встроить DRM hook так, чтобы DRM был учтён и не блокировал VOD basic playback (plaintext fixture + детерминированный `Unimplemented` если встретили EXT-X-KEY до реализации decrypt).

---

## 5) Мини-матрица тестов (portable)

### ABR
- [ ] `cache_hit_does_not_affect_throughput`
- [ ] `downswitch_on_low_throughput`
- [ ] `upswitch_requires_buffer_and_hysteresis`
- [ ] `min_switch_interval_prevents_oscillation`

### DRM
- [ ] `aes128_decrypt_smoke_plaintext_prefix`
- [ ] `key_processor_applied_and_cached_processed_key`
- [ ] `offline_mode_cache_miss_is_fatal`

### Decode
- [ ] `seek_no_deadlock`
- [ ] `end_of_stream_returns_none_not_error` (или фиксируем контракт)
- [ ] `pipeline_backpressure_does_not_return_read_ok0_early` (если тестируется через bridge)

### Net
- [ ] `retry_on_5xx_and_429_not_on_4xx`
- [ ] `timeout_applies_to_headers_not_body_stream` (если это контракт)

---

## 6) Notes для ревью (как понять, что “стало как legacy”)

ABR:
- код разделён на estimator/controller, есть `AbrDecision`.
- throughput estimator **не меняется** на cache hit.
- тесты не требуют внешней сети, decision/apply разделены.

DRM:
- AES-128 decrypt реально включён всегда.
- processed keys кэшируются, offline работает.
- ключи не логируются.

Decode:
- чёткая слойность: source → engine → decoder → pipeline.
- нет `Ok(0)` до настоящего EOF в bridge.

Net:
- чистая композиция слоёв, понятный контракт в README, deterministic tests.

---
