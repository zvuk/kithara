# Source/Driver reference spec (portable) — execution model + contract for `kithara-file` and `kithara-hls`

Этот документ — **самодостаточный reference** для автономных агентов, которые **не имеют доступа** к legacy репозиториям.
Он фиксирует **как должны работать** источники `kithara-file` и `kithara-hls` в рамках уже заложенной архитектуры `kithara`:
- источники производят **async stream байтов** (data-plane),
- декодирование происходит **синхронно** в отдельном потоке (через `kithara-io` bridge и `kithara-decode`),
- источники содержат **driver loop** (оркестрация/сеть/кэш),
- тесты должны быть **детерминированными**, без внешней сети.

Связанные документы:
- `AGENTS.md` (одно изменение — один крейт, TDD-first, dependency hygiene, imports)
- `docs/constraints.md` (EOF/backpressure, offline, ABR rules, asset_id/resource_hash)
- `docs/porting/net-reference.md` (retry/timeout matrix)
- `docs/porting/drm-reference.md` (AES-128 decrypt, processed keys caching)
- `docs/porting/abr-reference.md` (ABR decision/apply model)
- `docs/porting/decode-reference.md` (decode layering)

---

## 0) Нормативные правила (must)

### 0.1 Источник == ресурсный driver loop (не decode thread)
- Вся сеть/оркестрация (HTTP, HLS parsing, выбор сегментов, кэширование, DRM key fetch/decrypt) должна жить **внутри source crate** (`kithara-file` или `kithara-hls`) в driver loop.
- Decode thread (Symphonia) не должен “ходить в сеть”. Он синхронно читает из `Read+Seek` bridge.

### 0.2 Stop убираем из публичного контракта (семантика отмены)
- **Stop как отдельная команда не нужен**.
- “Остановить воспроизведение” — это когда consumer **прекратил чтение**:
  - drop stream / drop session / drop reader.
- Если внутри источника требуется cancel, это реализуется через:
  - drop receiver (data-plane),
  - cancellation token / closed channel detection,
  - завершение driver loop.

> Если текущий публичный API уже содержит `Stop` (как тип), удалять его можно только если это НЕ ломает публичный контракт, зафиксированный в board’ах. Если “Stop” уже часть контракта — оставь тип, но **не используй его** как обязательный механизм. Приоритет: “drop == stop”.

### 0.3 EOF семантика: `Ok(0)` только при true EOS
См. `docs/constraints.md`: `Read::read() -> Ok(0)` означает EOF.  
Driver обязан корректно завершить stream:
- по естественному EOS (`Ok(None)` на уровне higher-level, или закрытие byte stream),
- или по fatal error (error item, затем закрытие).

### 0.4 Backpressure обязательна
Источник не должен бесконечно накапливать байты в памяти.
- Data-plane канал/буфер должен быть bounded.
- При заполнении writer должен блокироваться/дросселироваться.

### 0.5 Offline mode
Если включён `offline_mode`:
- **любая** недостающая сущность (playlist/segment/init/key для HLS; byte range для File, если кэш обязателен) должна приводить к **fatal** `OfflineMiss`.
- Не должно быть попыток “вдруг сходить в сеть”.

---

## 1) Общая модель Source/Session/Driver

### 1.1 Термины
- **Source**: публичная точка входа (`FileSource::open`, `HlsSource::open`).
- **Session**: хэндл на “живую” сессию (хранит `asset_id`, опции, и предоставляет `stream()`).
- **Driver**: внутренняя async задача (tokio task), которая выполняет orchestration loop и пишет байты в data-plane канал.

### 1.2 Контракт уровня `Session`
Минимальный контракт, который должен быть у обоих источников:

- `asset_id()` — стабильный идентификатор без query/fragment (см. `docs/constraints.md`).
- `stream()` — возвращает `Stream<Item = Result<Bytes, Error>>`:
  - items идут последовательно и составляют байтовую “ленту” контента,
  - при fatal error stream выдаёт `Err(...)` и затем завершается,
  - при EOS stream завершается без ошибки (закрывается канал).

Опционально (если контрактом уже предусмотрено):
- control-plane (например `commands() -> Sender<Command>`).  
Но *если команда не нужна*, не добавляй её “на будущее”.

### 1.3 Контракт уровня `Driver`
Driver должен:

1) Стартовать с валидации/инициализации:
   - построить `asset_id`,
   - подготовить кэш (если включён),
   - подготовить net client (timeout/retry уже на уровне `kithara-net`).

2) Основной цикл:
   - определить “что читать дальше” (next resource descriptor),
   - получить bytes (cache-first, network optional),
   - опционально трансформировать bytes (DRM decrypt для HLS),
   - отправить bytes в data-plane канал (с backpressure).

3) Завершение:
   - EOS: закрыть data-plane (drop sender).
   - Fatal: отправить `Err(e)` (best-effort), затем закрыть.
   - Cancel: если receiver закрыт (consumer перестал читать), driver прекращает работу.

4) Инварианты:
   - никогда не “зависать” навсегда без прогресса при закрытом receiver,
   - не паниковать, не использовать unwrap/expect в прод-коде.

---

## 2) `kithara-file`: как должно работать (execution model)

### 2.1 Что такое `kithara-file` в `kithara`
`kithara-file` — это **progressive HTTP** источник байтов (mp3/aac/…):
- выдаёт байты файла последовательно от начала до конца,
- поддерживает range GET (для seek) при необходимости в рамках `kithara-io`/bridge,
- опционально использует `kithara-cache` как persistent disk cache для offline (если так зафиксировано контрактом).

### 2.2 Минимально рабочий driver loop (v1)
V1 реализация должна реально “качать весь файл” и завершаться:

- Driver выбирает стратегию чтения:
  - simplest: `net.stream(url)` и в цикле `next()` шлёт чанки в data-plane.
- Если включён cache:
  - cache-through-write: параллельно писать полученные bytes в asset cache (crash-safe: temp → rename).
  - после завершения — “commit” (если нужно), иначе кэш должен быть пригоден.
- EOS:
  - когда upstream stream завершился, driver закрывает data-plane.

> Это уже даст “реальную” работу, а не заглушки, и позволит декодировать mp3 через мост.

### 2.3 Range/Seek: где ответственность
Важно не перепутать уровни:
- `kithara-file` **может** предоставлять `get_range`/random access как отдельный API, если контрактом предусмотрено.
- Но seek на уровне Symphonia обычно будет реализован через `kithara-io` bridge:
  - bridge при seek даёт команду источнику/драйверу или открывает новый range.
- На v1 допустимо:
  - если seek ещё не реализован — зафиксировать контракт тестом: seek либо не поддерживается (ошибка), либо best-effort.

### 2.4 Тесты `kithara-file` (must-have)
Тесты должны быть локальные (фикстура HTTP сервер):

1) `file_stream_downloads_all_bytes_and_closes`
- сервер отдаёт фиксированный body (Bytes),
- session.stream() выдаёт ровно эти bytes и закрывается.

2) `file_receiver_drop_cancels_driver`
- consumer читает 1 chunk и drop’ает stream,
- driver должен завершиться (можно проверять косвенно: server request count, или через join handle если exposed test-only).

3) `file_cache_through_write_persists_and_offline_replays`
- прогон online → заполнить cache,
- затем offline_mode=true → проиграть без сети (или с сервером, который возвращает 500/не доступен),
- bytes совпадают.

4) `file_offline_miss_is_fatal`
- offline_mode=true и cache пуст,
- попытка открыть/стримить → `OfflineMiss` и завершение.

---

## 3) `kithara-hls`: как должно работать (execution model)

### 3.1 Что такое HLS в `kithara`
HLS — это **дерево ресурсов**:
- master playlist
- media playlist
- init segment (опционально)
- media segments
- key (DRM) resources

Источник `kithara-hls` должен:
- выбрать variant (manual/ABR),
- затем последовательно выдавать байты:
  - init segment (если есть и нужен),
  - segment0, segment1, ... до конца VOD
- поддерживать DRM AES-128 decrypt (обязателен),
- кэшировать ресурсы для offline.

### 3.2 Минимально рабочий driver loop (VOD, v1)
Ниже “must” последовательность.

#### Stage A — Fetch + parse master
1) Fetch master playlist (cache-first):
   - offline_mode:
     - только cache, иначе fatal OfflineMiss
   - online:
     - cache → if hit, use
     - else net.get_bytes + cache put_atomic
2) Parse master playlist.
3) Выбрать initial variant:
   - manual selector (если задан),
   - иначе ABR initial index (или 0).

#### Stage B — Fetch + parse media playlist (выбранного variant)
1) Resolve variant media playlist URL:
   - учитывать base_url override (если опции таковы),
   - иначе относительно master URL / текущего playlist URL (фиксировать правила тестами).
2) Fetch media playlist (cache-first) + parse.

> Для VOD можно считать media playlist статичной. Live не требуется, но дизайн не должен блокировать.

#### Stage C — Segment loop
Для каждого сегмента `i`:

1) (Опционально) Ensure init segment:
   - если media playlist содержит init segment (например fMP4), то:
     - загрузить init bytes (cache-first)
     - отправить init bytes в data-plane **перед** первым сегментом (или перед сегментом после switch/discontinuity)
   - Для TS без init — пропуск.

2) Determine encryption state for this segment:
   - если активен EXT-X-KEY METHOD=AES-128:
     - вычислить key URL (resolve),
     - IV: использовать явный IV или derived rule (см. `docs/porting/drm-reference.md`)
     - получить processed key через KeyManager (cache-first, processor, persist processed)
   - если нет — plaintext.

3) Fetch segment bytes (cache-first, затем net):
   - обязательно сохранять в cache (если cache включён и это часть контракта offline).
   - важно: ABR throughput estimator обновлять **только по network fetch**, и **не обновлять** на cache hit.

4) Decrypt (если нужно):
   - decrypt AES-128-CBC ciphertext → plaintext bytes
   - ошибки decrypt => fatal
   - downstream получает plaintext

5) Emit data:
   - отправить bytes в data-plane канал (bounded).

#### Stage D — EOS
- После последнего сегмента закрыть data-plane (drop sender).

### 3.3 Control plane (manual variant / ABR) — только на границе сегмента
Если текущий контракт предусматривает команды (`SetVariant`, etc.), то:
- применение переключения variant должно происходить **на границе сегмента**,
- при switch:
  - не “перезапускать” VOD с нуля,
  - продолжать с текущего segment index, если это контрактом закреплено (см. legacy tests),
  - при необходимости вставить init segment нового variant.

**Decision vs Applied:**
- ABR выдаёт decision,
- worker применяет,
- событие `VariantApplied` эмитится только после применения.

### 3.4 DRM — обязательный
DRM требования см. `docs/porting/drm-reference.md`:
- AES-128 decrypt должен реально быть в pipeline.
- processed keys caching обязателен.

### 3.5 Что именно было “плохо” в текущем состоянии (типичный анти-паттерн)
Если драйвер:
- загрузил master,
- загрузил media playlist,
- и “на этом остановился” — это значит, что нет Stage C (segment loop) или он не соединён с data-plane.

**Definition of done** для “реальной реализации”:
- тест “VOD completes and fetches all segments” должен быть зелёным,
- stream должен выдавать bytes от сегментов, а не только плейлисты,
- и корректно закрываться.

---

## 4) Тестовая стратегия и фикстуры (общие для File и HLS)

### 4.1 Общие требования к локальному серверу
Фикстура должна уметь:
- отдавать bytes по путям,
- уметь отдавать master/media playlists,
- уметь отдавать сегменты с детерминированным payload (prefix),
- считать количество запросов per-path (для “key fetched once”, “segment fetched all”),
- симулировать 404/403/500 и задержки заголовков (для timeout/retry/negative cases).

### 4.2 Детерминированные payload prefixes
Для HLS важно уметь “проверить что это действительно сегменты выбранного variant”.

Рекомендуемый формат plaintext segment payload:
- `b"V{variant}-SEG-{i}:" + payload...`

Тогда тест может:
- прочитать первые N байт из stream,
- проверить что prefix соответствует variant/segment.

Для File:
- payload может быть просто фиксированным буфером, или “chunked” версия того же.

---

## 5) Backlog задач для агентов (по крейтам, без изменения публичного контракта)

Ниже — задачи, которые нужно добавить в соответствующие boards (`docs/kanban-kithara-file.md`, `docs/kanban-kithara-hls.md`).
Каждая задача выполняется в рамках **одного крейта**.

### 5.1 `kithara-file` — “реальная” реализация driver + тесты
- [ ] Implement driver loop that streams the entire resource and closes on EOS:
  - no “placeholder” tasks
  - receiver drop cancels the loop
- [ ] Implement cache-through-write (if cache enabled by contract):
  - crash-safe writes (temp → rename)
  - offline replay works
- [ ] Add deterministic fixture server utilities:
  - fixed payload endpoint
  - request counters
- [ ] Add tests:
  - downloads all bytes and closes
  - receiver drop cancels driver
  - offline replay from cache
  - offline miss fatal

### 5.2 `kithara-hls` — “реальная” VOD сегментная реализация + тесты
- [ ] Wire driver loop end-to-end:
  - master → media playlist → segment loop → bytes out → EOS
- [ ] Implement segment URL resolution rules (base_url override + relative resolution) with tests
- [ ] Implement segment fetching cache-first + offline fatal behavior with tests
- [ ] Implement DRM AES-128 decrypt pipeline + processed key caching (см. `drm-reference.md`) with tests
- [ ] Implement ABR surface (см. `abr-reference.md`) but **do not block basic playback**:
  - playback must work with manual variant selection first
- [ ] Add core integration tests:
  - VOD completes; all segments fetched at least once; stream closes
  - manual variant outputs only selected variant prefixes
  - base_url override works
  - offline replay works
  - DRM smoke decrypt works

### 5.3 “Stop removal” task (если применимо и не ломает контракт)
- [ ] Remove `Stop` command usage from drivers:
  - stopping is achieved by dropping the stream/session
  - internal cancel triggered by closed channel detection
- [ ] Ensure tests cover cancellation via drop

> Если `Stop` пока часть публичного API — не удалять тип без согласования, но перестать делать его “обязательным”.

---

## 6) Вопрос: выносить ли общий source/driver в отдельный сабкрейт?

Короткий ответ: **сейчас нет** — по правилам проекта и по риску расползания.

Причины:
1) Правило `AGENTS.md`: “одно изменение — один крейт”. Создание нового общего крейта почти гарантированно потребует правок минимум в `kithara-file` и `kithara-hls` (и возможно `kithara-io`), то есть нарушит правило.
2) Общность пока “поверхностная”:
   - да, есть driver loop и lifecycle,
   - но HLS — дерево ресурсов + ABR + DRM, а File — линейный ресурс; ошибки, кэш-ключи и события отличаются.
3) Риск преждевременной абстракции:
   - общий “Driver” API быстро станет “god trait” или будет тянуть HLS-специфику в File.

Когда есть смысл выделять общий слой:
- когда оба крейта имеют **устойчиво одинаковые** примитивы:
  - “bounded byte pipe”,
  - “driver cancellation on receiver drop”,
  - “uniform event sink (telemetry)”
  - “cache-through-write primitive”
- и это подтверждено тестами и повторяющимся кодом (не на уровне “похоже”, а реально одинаковые блоки).

Рекомендованный компромисс, совместимый с правилами:
- сначала довести `kithara-file` и `kithara-hls` до **работающего** состояния + тесты,
- затем, отдельной задачей, сделать “extract common primitives” (возможно как `kithara-source` или модуль в `kithara-io`, но это отдельный kanban item и отдельное согласование).

---

## 7) Checklist “как агенту не сломать контракт” (must follow)

1) Не менять публичные типы/имена без проверки board’ов (kanban = контракт).
2) Любая “реальная реализация” должна быть покрыта тестами:
   - VOD segments реально стримятся,
   - File реально стримится и закрывается,
   - offline deterministic,
   - cancellation via drop.
3) Никаких `unwrap/expect` в прод-коде.
4) `cargo fmt` после каждого чекбокса, затем `cargo test -p <crate>`.

---