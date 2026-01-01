# Kithara — Kanban plans (per subcrate)

## Rodio playback examples (обязательный “слышимый” sanity-check)

Помимо тестов, проект должен содержать runnable примеры, которые **реально воспроизводят звук** на машине разработчика.

### Где лежат примеры (фиксируем структуру workspace)
- Примеры живут в **отдельном крейте**: `crates/kithara-examples`.
- Каждый пример — это binary target (обычно `examples/*.rs` внутри `kithara-examples`).
- `kithara-examples` должен зависеть от публичных API `kithara-*` крейтов и не содержать “умной” логики: только wiring/CLI/логирование.

### Требования к примерам
- Примеры должны работать без внешней инфраструктуры, кроме доступа к указанному URL (или локального тестового сервера/ассетов, если пример сделан так).
- Примеры должны поднимать `tokio` runtime для сетевой части и выполнять декодирование в отдельном blocking потоке (как описано в этом документе).
- Примеры должны использовать `rodio` для вывода на системное аудиоустройство.
- Примеры должны демонстрировать два сценария:
  1) Progressive HTTP (например mp3 по URL) — слышимый звук до конца трека.
  2) HLS VOD по URL master playlist — слышимый звук, возможность задать manual variant, кэширование.

### Что должно быть в примерах (поведение)
- CLI принимает URL (обязательный или с дефолтом, если так удобнее).
- Для HLS пример должен принимать опционально:
  - manual variant override (или auto),
  - offline_mode (по желанию),
  - уровень логирования.
- Для обоих примеров:
  - создаётся источник (file/hls),
  - байты идут в `kithara-io` bridge,
  - декодирование выполняется `kithara-decode` (generic, но в примере можно фиксировать `f32`),
  - PCM подаётся в `rodio` и воспроизводится до EOS (или до Ctrl-C / Stop команды).

### Канбан-задачи для примеров (агент выполняет по одному чекбоксу за раз)
- [ ] Add `kithara-examples` crate (workspace member) with rodio + CLI wiring
- [ ] Example: Progressive HTTP playback через `rodio` (слышимый звук до конца)
- [ ] Example: HLS VOD playback через `rodio` (manual/auto variant, кэширование, слышимый звук)

### Ссылки для ориентира (агенту недоступна локальная копия)
- decal (подход к decoder + worker): https://github.com/aschey/decal
- stream-download-rs (подходы к downloader/streaming/retry): https://github.com/aschey/stream-download-rs

---

## Недосказанности и обязательные “player-grade” инварианты (прочитать перед реализацией)

Этот блок фиксирует поведение, которое в прошлых итерациях было источником регрессий/флапов/неочевидных багов. Если что-то из этого не отражено в конкретном сабкрейт-разделе ниже — этот блок имеет приоритет как часть контракта.

### A) Где живёт бесконечный цикл загрузки (driver loop)
- **Ровно один** основной async цикл “скачивай дальше” живёт в источнике:
  - `kithara-file` (FileDriver)
  - `kithara-hls` (HlsDriver)
- `kithara-net` не держит бесконечных циклов: он предоставляет примитивы fetch/stream/range.
- `kithara-cache` не держит бесконечных циклов: он хранит/эвиктит.
- `kithara-io` не держит бесконечных циклов загрузки: он только буфер/bridge.
- `kithara-decode` держит blocking decode loop, но он не скачивает данные.

### A.1) Стандартные транспорты между потоками/абстракциями (обязательный выбор)
- Основной транспорт для сообщений/команд/событий/чанков PCM между потоками и компонентами: `kanal`.
  - Если нужен bounded канал — используем bounded API `kanal`.
  - Если нужен blocking receive в decode/worker thread — используем blocking API `kanal`.
  - Не вводить параллельный “зоопарк” каналов без причины (например, не смешивать `std::mpsc`/`tokio::mpsc`/`flume` одновременно).
- Для аудио-очередей (PCM) в режиме “consumer в отдельном потоке” стандартный выбор: `ringbuf`:
  - если требуется async-API: использовать `async_ringbuf`
  - если требуется blocking-API: использовать `ringbuf-blocking`

### B) Backpressure обязательна
- Любой “driver loop” обязан уважать backpressure со стороны `kithara-io`.
- Нельзя бесконечно накапливать память: буфер bounded по bytes.
- Нельзя “ронять” медиаданные (drop bytes/samples) ради того, чтобы “продолжало играть” — это разрушает PCM/канальные границы и порождает артефакты.

### B.1) Где реализуется backpressure
- На bytes уровне:
  - `kithara-io` обеспечивает bounded buffering по bytes.
  - источники (`kithara-file`, `kithara-hls`) обязаны блокироваться/await на push, когда буфер заполнен.
- На PCM уровне (если/когда появится поток PCM между decode и хостом):
  - использовать `ringbuf` (async_ringbuf или ringbuf-blocking) для корректной очереди без drop семплов.
  - при переполнении — backpressure/ожидание, не drop.

### C) EOF семантика (критично)
- Для sync `Read`:
  - `Read::read()` **никогда** не возвращает `Ok(0)` до доказанного EOS.
  - `Ok(0)` допускается только после того, как источник завершился, `BridgeWriter::finish()` вызван и буфер полностью выдренажен.
- Никаких in-band magic bytes.
- При ошибке:
  - даже если data-plane возвращает `Err`, bridge всё равно должен завершаться предсказуемо (через `finish()`), чтобы decode loop не завис.

### D) Ошибки: различать recoverable и fatal
- Источники должны различать:
  - **recoverable** ошибки (временный сетевой сбой, retry/backoff) — driver продолжает работать.
  - **fatal** ошибки (offline miss в offline_mode, невозможный формат, некорректный ключ/DRM, постоянный 404 на обязательный ресурс) — driver завершает сессию.
- На fatal ошибке:
  - driver должен прекратить работу,
  - вызвать `BridgeWriter::finish()` (чтобы decode loop не зависал),
  - и отдать ошибку наружу предсказуемым образом.

### D.1) Контракт распространения ошибок (обязательный)
- Для источников (`kithara-file`, `kithara-hls`) основной контракт ошибок: **через data-plane stream**:
  - `stream: Stream<Item = Result<Bytes, SourceError>>`
  - при fatal ошибке stream должен вернуть `Err(...)` и затем завершиться (`None`).
  - после fatal ошибки driver обязан вызвать `BridgeWriter::finish()` (или эквивалент), чтобы decode не повис в ожидании данных.
- Для decode слоя (`kithara-decode`) основной контракт ошибок:
  - `Decoder::next()` возвращает `Result<Option<PcmChunk<T>>, DecodeError>`.
  - при fatal decode error: вернуть `Err` и завершить дальнейшую выдачу PCM предсказуемо (без зависаний).
- UI/telemetry события (если будут) не должны быть единственным источником истины об ошибке.

### E) Seek — “player-grade”, best-effort, абсолютный
- Seek инициируется пользователем/движком (DJ use-case):
  - Для `kithara-decode`: `seek(Duration)` best-effort, не должен приводить к зависаниям.
  - Для источников:
    - `kithara-file`: seek по байтам (range) как базовый механизм.
    - `kithara-hls`: seek по времени в рамках VOD (по playlist) как базовый механизм.
- При seek допускается reset/реинициализация decode state (это ожидаемо для плеера).

### E.1) Контракт seek (обязательные детали)
- Seek — **абсолютный** (не “относительный к текущей позиции”) и best-effort.
- Seek должен быть безопасен при повторных вызовах и при конкурентных командах (минимум: корректная сериализация команд в driver loop).
- Поведение при seek вне диапазона:
  - seek < 0: clamp to 0
  - seek > duration (если duration известен): clamp to end и завершить (или вернуть error) — выбрать один вариант и зафиксировать в реализации.
- Поведение data-plane при seek:
  - допустим “reset потока”: driver может сбросить незаконченные загрузки и начать эмитить bytes с новой позиции.
  - bridge должен корректно обработать смену потока данных (не выдавать ложный EOF).

### F) DRM / keys
- DRM (AES-128 baseline) обязателен как часть контракта HLS:
  - ключи могут быть “wrapped”, `key_processor_cb` может раскрывать их.
  - в кэш должны сохраняться **processed keys**, пригодные для offline расшифровки.
  - поддержать `key_query_params` и `key_request_headers`.
- Никогда не логировать ключевые байты/секреты.
- Политика ошибок DRM:
  - если сегмент требует key, но key не доступен (offline miss / fetch failed / processor failed) — это **fatal** для текущей сессии.

### G) Кэш обязателен и “tree-friendly”
- Кэш должен переживать перезапуски.
- HLS кэширует **всё** (playlists, init, segments, keys) для offline.
- Eviction:
  - лимит по общему размеру (bytes),
  - LRU по asset,
  - может удалить несколько asset перед записью нового,
  - pinned asset не эвиктится.
- Политика cache miss:
  - в `offline_mode = true` любой miss на обязательный ресурс => **fatal error**
  - в online режиме miss => network fetch + cache put (если есть место, иначе eviction перед put).

### H) DJ/engine базовый use-case
- `kithara` — часть networking/decoding базы для DJ engine:
  - сеть и оркестрация async,
  - декодирование в отдельном blocking потоке,
  - аудиовывод/микшер вне `kithara`,
  - стабильное поведение при seek/stop/reopen и при повторном использовании кэша.

---

Этот документ — **contract-first** план реализации в стиле канбана. Он предназначен для автономных агентов, которые будут разрабатывать сабкрейты **параллельно и независимо**, при этом сохраняя совместимость через зафиксированные публичные контракты.

Перед началом работы над любой задачей агент **обязан прочитать**:
- `AGENTS.md` (правила разработки)
- `docs/constraints.md` (конституция проекта, уроки и ограничения)
- `README.md` (workspace-first deps и правила в кратком виде)

## Глобальные правила (обязательные для всех планов)

1) **Workspace-first dependencies**  
   Версии всех зависимостей добавляются только в корневой `Cargo.toml` (`[workspace.dependencies]`).  
   В сабкрейтах зависимости подключаются только как `dep = { workspace = true }`, включая `dev-dependencies` и `build-dependencies`.

2) **TDD-first**  
   Любая фича/фикс начинается с теста, который описывает желаемое поведение.  
   Тест должен падать до реализации и проходить после.

3) **Минимум комментариев в коде**  
   Максимум — однострочные. Длинные объяснения только в `README.md` соответствующего сабкрейта.

4) **Generics-first**  
   Расширяемость достигается дженериками/трейтовыми политиками/композицией.

5) **Слабая связанность**  
   Сабкрейты не должны “знать лишнего” друг о друге. Общение — через интерфейсы и структуры из контрактов.

6) **Примеры — отдельный крейт**  
   Все runnable примеры (rodio playback) находятся в `crates/kithara-examples` и зависят только от публичных API.

---

# Общая модель системы (абстракции, воркеры, многопоточность, потоки данных)

Эта секция отвечает на вопросы:
- какие компоненты существуют,
- кто создаёт какие воркеры,
- как данные текут из сети в декодер,
- как закрывается сессия и как обрабатываются ошибки.

## Термины: Asset vs Resource

- **Asset** — “трек/контент” как единица кэша и eviction (LRU).
  - Идентифицируется `AssetId`.
  - **Не зависит от query/fragment** URL.

- **Resource** — конкретный сетевой объект, который нужно получить/закэшировать:
  - playlist, segment, init, key, mp3 bytes, и т.д.
  - Идентифицируется `ResourceHash`.
  - **Включает query**, fragment игнорируется.

## Многопоточность и воркеры (high-level)

### Жизненный цикл сессии (в общих чертах)
- `*_Source::open(...)` создаёт:
  - control-plane (команды) для driver loop,
  - data-plane (bytes stream) для feeding `kithara-io`,
  - и запускает driver loop (см. ниже).
- `kithara-io` создаёт `BridgeWriter/BridgeReader`.
- `kithara-decode` запускается отдельным blocking worker (явно или через helper) и читает `BridgeReader`.

### Потоки/задачи
- **Tokio runtime threads**: async задачи сети и оркестрации (file/hls). Количество потоков задаёт хост (обычно multi-thread runtime).
- **Decode thread (blocking)**: отдельный поток (или `spawn_blocking`), который синхронно читает `Read + Seek` и декодирует через Symphonia.
- **Audio output thread** (вне `kithara`): в хосте/движке; читает PCM из очереди/канала. `kithara` не делает I/O с устройством.

### Воркеры и циклы обработки

#### Контракт: кто запускает loop
- `kithara-file::FileSource::open(...)` **обязан** запустить ровно один async `FileDriver` loop (через tokio runtime) и вернуть handle/session, через который можно:
  - посылать команды (Stop/SeekBytes),
  - получать bytes stream (для feeding bridge),
  - корректно завершить сессию (drop/Stop).
- `kithara-hls::HlsSource::open(...)` **обязан** запустить ровно один async `HlsDriver` loop и вернуть handle/session, через который можно:
  - посылать команды (Stop/SeekTime/SetVariant/ClearVariantOverride),
  - получать bytes stream.

`kithara-io` и `kithara-net` не владеют жизненным циклом этих циклов и не запускают их.

#### File worker
`kithara-file` поднимает один async воркер (условно `FileDriver`), который:
- получает команды (Stop/SeekBytes и т.п.),
- читает байты из cache (hit) или скачивает через `kithara-net` (miss),
- при необходимости пишет в cache,
- пушит `Bytes` чанки в `kithara-io::BridgeWriter`,
- завершает поток вызовом `BridgeWriter::finish()` (EOS).

#### HLS worker (VOD)
`kithara-hls` поднимает один async воркер (условно `HlsDriver`), который:
- загружает master/media playlists (cache hit или net->cache),
- выбирает variant (policy/ABR/manual),
- выполняет VOD loop по init/segments,
- для каждого ресурса делает `exists/open` из `kithara-cache` или `kithara-net` fetch + `cache.put_atomic`,
- для шифрованных сегментов загружает key, пропускает через `key_processor_cb` и кэширует **processed key**,
- пушит bytes в `kithara-io::BridgeWriter`,
- обрабатывает команды (Stop/SeekTime/SetVariant/ClearVariantOverride),
- завершает `BridgeWriter::finish()`.

#### Decode worker
`kithara-decode` выполняет blocking loop:
- читает из `kithara-io::BridgeReader` (`Read + Seek`),
- открывает Symphonia format/decoder,
- выдаёт PCM чанки в канал/стрим наружу,
- поддерживает `seek(Duration)` best-effort.

### Инварианты
- Никаких in-band magic bytes в медиапотоке.
- `Read::read()` не возвращает `Ok(0)` до доказанного EOS (только после `finish()` и полного дренажа).
- Backpressure обязателен: bounded буфер по bytes, никаких бесконтрольных аллокаций.
- Любой fatal error должен приводить к корректному завершению сессии (без зависаний decode loop).

## Потоки данных (end-to-end)

### File path (HTTP mp3 и т.п.)
1) `kithara-file::FileSource::open` создаёт `FileSession` и поднимает `FileDriver` (async).
2) `FileDriver` читает из `kithara-cache` (hit) или `kithara-net` (miss), пишет в `kithara-cache` (если включено), пушит в `BridgeWriter`.
3) `kithara-io::BridgeReader` читается `kithara-decode` синхронно.
4) `kithara-decode` декодирует и отдаёт PCM наружу.

Каналы:
- bytes: `FileDriver -> BridgeWriter -> BridgeReader -> Decoder`
- commands: host -> `FileSession.commands()` -> `FileDriver`

### HLS VOD path
1) `kithara-hls::HlsSource::open` создаёт `HlsSession` и поднимает `HlsDriver` (async).
2) `HlsDriver`:
   - master/media playlists: cache hit или net->cache
   - init/segments: cache hit или net->cache
   - keys: cache hit или net->fetch -> `key_processor_cb` -> cache(processed)
   - пушит bytes в `BridgeWriter`
3) decode как в file path.

Каналы:
- bytes: `HlsDriver -> BridgeWriter -> BridgeReader -> Decoder`
- commands: host -> `HlsSession.commands()` -> `HlsDriver`

---

# Контрактные интерфейсы (общие)

В целях независимой разработки фиксируем минимальный набор “общих” интерфейсов.

## Byte stream contract (async)

На уровне источников (`kithara-hls`, `kithara-file`) наружу отдаётся **асинхронный поток чанков**:

- `ByteChunk`: `bytes::Bytes`
- `ByteStream`: `Stream<Item = Result<Bytes, SourceError>>` (с `Send`)

Сигнал EOS:
- источник завершает `Stream` (возвращает `None`), и/или эмитит `SourceControl::EndOfStream`.

> Примечание: “control в потоке” можно реализовать как отдельный канал управления, чтобы не смешивать медиаданные и сигналы. Но контракт ниже допускает оба подхода.

## Command contract (control plane)

Каждый источник должен поддерживать базовые команды:
- `Stop`:
  - завершает driver loop,
  - завершает bytes stream,
  - вызывает `BridgeWriter::finish()` (или эквивалентный путь завершения).
- `Seek` (best-effort, абсолютный):
  - `kithara-file`: `SeekBytes(u64)` как базовый механизм (Range).
  - `kithara-hls`: `SeekTime(Duration)` как базовый механизм (по playlist).
- расширение: HLS-specific команды (variant override) и т.п.

Команды должны быть “player-grade”:
- не приводить к дедлокам,
- не зависеть от out-of-band событий,
- быть безопасными при повторном вызове (best-effort idempotency).
- “source-specific commands” расширяем через enum/trait-политику.

---

# Kanban по сабкрейтам

Ниже — планы по каждому сабкрейту. Каждый план включает:
- ответственность
- публичный API/контракты (что экспортировать)
- тестовые сценарии (TDD)
- потоки данных
- зависимости

---

## 1) `kithara-core`

### Цель
`kithara-core` намеренно **минимален** и содержит только то, что нужно всем остальным сабкрейтам для согласованности идентичности.

**Владелец настроек:**
- `CacheOptions` и всё, что относится к дисковому кэшу, живёт в `kithara-cache`.
- `HlsOptions` и все ABR/variant настройки живут в `kithara-hls`.

`kithara-core` не должен становиться “свалкой общих настроек”. Его задача — общие идентификаторы и маленькие базовые типы/ошибки, без привязки к `tokio`/`reqwest`/`symphonia`.

### Публичный контракт (экспорт)

**Identity**
- `AssetId`
  - `fn from_url(url: &url::Url) -> AssetId`
  - canonicalization: URL без query и fragment
- `ResourceHash`
  - `fn from_url(url: &url::Url) -> ResourceHash`
  - canonicalization: URL с query, без fragment

**Ошибки**
- `CoreError` (или несколько узких ошибок):
  - invalid url canonicalization
  - invalid path component (если будет общая валидация)

**Опции**
- В `kithara-core` **нет** `CacheOptions`, `HlsOptions`, ABR/variant настроек и их контрактов.
- Все настройки кэша принадлежат `kithara-cache`.
- Все настройки HLS (включая ABR/variant selection/tuning) принадлежат `kithara-hls`.

### Тесты (TDD)
- `asset_id_ignores_query_and_fragment`
- `resource_hash_includes_query_but_ignores_fragment`
- canonicalization cases:
  - host/scheme case normalization
  - default port normalization (если включаем)

### Потоки данных
Нет (pure).

### Зависимости
- `url`, `thiserror` (workspace)

---

## 2) `kithara-cache`

### Цель
Persistent disk cache, tree-friendly, crash-safe writes, bytes-based global limit, LRU eviction, pin/lease.

---

## 2.a) `kithara-cache` — Layered architecture (decorators/generics; по мотивам `stream-download-storage-ext`)

### Цель
Расширить внутреннюю архитектуру `kithara-cache` так, чтобы:
- базовый слой умел минимум (FS layout + atomic write + open/exists),
- “умные” фичи (lease/pin, index/state.json, eviction/LRU, observability) добавлялись слоями через generics/decorator pattern,
- при этом **не ломать** существующий публичный контракт из раздела выше (`AssetCache`, `AssetHandle`, `CachePath`, `LeaseGuard`, `ensure_space`, `stats`).

Эта секция фиксирует план рефакторинга/расширения, вдохновлённый подходом из `stream-download-storage-ext`:
- базовые примитивы (`BlobCache`/`KVStore`)
- дерево ключей
- lease-aware слой
- компоновка через обобщённые типы

### Компоненты (контракт и ответственность)

#### `CachePath`
Остаётся публичным безопасным относительным путём. Используется всеми слоями как “ключ” внутри asset.

#### `Store` / `KV` базовый интерфейс (internal)
Внутренний (не обязательно публичный) минимальный контракт хранения blob’ов по ключу:
- `exists(asset, path) -> bool`
- `open(asset, path) -> Option<File>`
- `put_atomic(asset, path, bytes) -> PutResult`
- `remove_all(asset)`

Это “низ” (аналог `BlobCache`/`KVStore` идеи), который не знает про eviction, leases и индекс.

#### `FsStore` (base layer)
Базовая реализация на filesystem:
- tree-friendly layout внутри `assets/<asset_id>/...`
- `put_atomic` через temp+rename
- `exists/open` как прямые операции на FS (источник истины)
- не содержит LRU/eviction/lease/index логики

#### `LeaseStore<S>`
Декоратор над `S`:
- pin/lease семантика (RAII guard) для asset
- гарантирует “pinned assets are not evicted” на уровне higher-level policy
- сам по себе не решает eviction — только предоставляет механизм “is_pinned(asset)”

#### `IndexStore<S>`
Декоратор над `S`:
- ведёт `state.json` (atomic rewrite)
- поддерживает `total_bytes`, per-asset метаданные (`size_bytes`, `last_access_ms`, `created_ms`)
- синхронизирует индекс с FS (best-effort) и никогда не делает индекс источником истины для существования файла

#### `EvictingStore<S, P>`
Декоратор над `S`:
- `ensure_space(incoming_bytes, pinned)` реализует eviction loop по политике `P` (LRU)
- удаляет несколько asset’ов пока не влезем в `max_bytes`
- никогда не удаляет pinned (использует слой lease/pin, если он присутствует)

`P` — policy (generics-first):
- сортировка кандидатов (LRU по last_access)
- правила исключений (pinned)
- стратегия удаления (batch vs one-by-one)

#### Facade: `AssetCache`
Публичный `AssetCache` остаётся фасадом, который внутри собирает слои:
- base `FsStore`
- + индекс
- + lease/pin
- + eviction policy

Важно: внешнее API остаётся прежним, а расширяемость достигается заменой/композицией внутренних слоёв.

### Инварианты (обязательные)
- FS остаётся источником истины для “exists/open”.
- `put_atomic` crash-safe-like: tmp не считается hit.
- pin/lease не допускает eviction активного asset’а.
- eviction считает bytes глобально (на весь кэш), а не по-ресурсно.

### Тесты (TDD)
Дополняем существующие MVP тесты тестами на слои (unit):
- base layer: `put_atomic_is_crash_safe_like` (уже есть в контракте)
- index layer: “after put -> state.json updated”, “restart loads state”
- eviction layer: “evicts until fit”, “skips pinned”, “removes multiple assets”
- lease layer: “touch updates lease age”, “drop guard releases pin”

> Важно: тесты не должны зависеть от внешней сети и должны быть быстрыми (tempdir, маленькие файлы).

### Потоки данных
- `kithara-hls`/`kithara-file` используют cache как “KV по пути внутри asset”.
- слои не должны знать про HLS/HTTP/DRM — только про asset/path/bytes.

### Зависимости
- не добавлять тяжёлые зависимости без необходимости; придерживаться текущего набора (`kithara-core`, `uuid`, `thiserror`) и std.

### Публичный контракт (экспорт)

**Основные типы**
- `AssetCache`
  - `fn open(opts: CacheOptions) -> Result<AssetCache, CacheError>`
  - `fn asset(&self, asset: AssetId) -> AssetHandle`
  - `fn pin(&self, asset: AssetId) -> LeaseGuard`
  - `fn touch(&self, asset: AssetId) -> Result<(), CacheError>`
  - `fn ensure_space(&self, incoming_bytes: u64, pinned: Option<AssetId>) -> Result<(), CacheError>`
  - `fn stats(&self) -> CacheStats`

- `AssetHandle`
  - `fn exists(&self, rel: &CachePath) -> bool`
  - `fn open(&self, rel: &CachePath) -> Result<Option<std::fs::File>, CacheError>`
  - `fn put_atomic(&self, rel: &CachePath, bytes: &[u8]) -> Result<PutResult, CacheError>`
  - `fn remove_all(&self) -> Result<(), CacheError>`

- `CachePath`
  - безопасный относительный путь (без `..`, без абсолютных путей, без пустых сегментов)
  - конструктор из сегментов: `CachePath::new(Vec<String>) -> Result<CachePath, CacheError>`

- `LeaseGuard`
  - RAII guard; пока жив — asset pinned

**Eviction policy**
- kithara-cache сам поддерживает `max_bytes` и LRU:
  - удаляет **несколько** asset’ов при необходимости
  - никогда не удаляет pinned

**Формат индекса**
- `state.json` (atomic rewrite)
- минимум:
  - `max_bytes`
  - `total_bytes`
  - per-asset: `size_bytes`, `last_access_ms`, `created_ms`

**Файловые расширения**
- кэш хранит “имена файлов как в CachePath”.
- если у ресурса нет extension — файл без extension (это решает уровень выше, в HLS/file).

### Тесты (TDD)
- `put_atomic_is_crash_safe_like`:
  - “после put -> exists/open true”
  - “tmp files do not appear as hits”
- `asset_id_dir_layout_is_stable`
- `ensure_space_evicts_lru_until_fit`:
  - создаём N asset’ов, превышаем лимит -> должны удалиться самые старые
- `pin_prevents_eviction`
- `resource_exists_uses_fs`:
  - гарантия: перезапуск `AssetCache` (новый инстанс) видит существующие файлы

> Тесты должны быть быстрые: маленькие файлы, временная директория.

### Потоки данных
- `kithara-hls`/`kithara-file` используют cache как “KV по пути внутри asset”.

### Зависимости
- `kithara-core`, `uuid`, `thiserror`

---

## 3) `kithara-net`

### Цель
HTTP fetcher слой на reqwest/rustls: streaming + range. Без HLS и без кэша.

---

## 3.a) `kithara-net` — Full client (retry/timeout + слои, generics-first)

### Цель
Сделать из `kithara-net` полноценный клиент, построенный из независимых слоёв (decorator pattern), по аналогии с архитектурой `stream-download`:
- базовый HTTP слой (reqwest)
- `timeout` слой
- `retry` слой с политикой/backoff
- builder/facade для “дефолтной” сборки

### Компоненты (контракт и ответственность)

#### `traits`
- `Net` (core trait) — минимальный контракт клиента:
  - `get_bytes(...) -> NetResult<Bytes>`
  - `stream(...) -> NetResult<ByteStream>`
  - `get_range(...) -> NetResult<ByteStream>`
- `NetExt` — удобные методы композиции (например `.with_retry(...)`, `.with_timeout(...)`), без скрытой магии.

#### `types`
- `ByteStream = BoxStream<'static, NetResult<Bytes>>`
- типы запросов (минимальные, без избыточности):
  - `Headers` (map строка->строка или типизированный header map)
  - `RangeSpec` (start, end option)
- Никакого HLS-контекста внутри `kithara-net`.

#### `base` (`ReqwestNet`)
Базовая реализация `Net` на `reqwest`:
- сборка и отправка запроса
- единая обработка HTTP status:
  - успешные ответы -> bytes/stream
  - неуспешные -> типизированная ошибка, содержащая status и URL (без логирования секретов)
- поддержка headers passthrough
- поддержка Range запросов

#### `timeout` (`TimeoutNet<N>`)
Декоратор над `N: Net`:
- реализует timeout-ограничение операций:
  - для `get_bytes`: timeout на всю операцию
  - для `stream/get_range`: timeout на фазу “request + response headers” (mid-stream stall timeout не входит в v1)
- при срабатывании возвращает `NetError::Timeout`.

#### `retry` (`RetryNet<N, P>`)
Декоратор над `N: Net`:
- retries применяются только к операциям “до начала чтения body”:
  - retry при ошибке запроса/соединения
  - retry по статусам (например 5xx, 429; решение про 408 фиксируется тестами)
- не делает mid-stream retry в v1 (нельзя корректно без строгого range/offset контракта)
- использует `RetryPolicy`:
  - max retries
  - backoff strategy (exponential + cap; без jitter в тестах)

#### `builder`
- `NetBuilder` / `create_default_client()`:
  - собирает “base + retry + timeout” в удобный фасадный тип для потребителей (`kithara-hls`, `kithara-file`).

### Публичный контракт (экспорт)

- `NetClient`
  - `fn new(opts: NetOptions) -> NetClient`
- `NetOptions`
  - timeouts, retry policy (минимум)
- `async fn stream(url: Url, headers: HeaderMap?) -> Result<impl Stream<Item=Result<Bytes, NetError>>, NetError>`
- `async fn get_bytes(url: Url, headers: ...) -> Result<Bytes, NetError>`
- `async fn get_range(url: Url, range: (u64, u64?), headers: ...) -> Result<impl Stream<Item=Result<Bytes, NetError>>, NetError>`
- `NetError`

> Важно: заголовки для key requests/DRM должны поддерживаться (в отличие от старых ограничений stream-download). Мы делаем это сразу, потому что это “клиентская библиотека”.

### Тесты (TDD)
- локальный сервер (axum или tiny http server) в тестах:
  - stream returns expected bytes
  - range returns correct slice
  - timeout behavior (минимально, без flaky)

### Потоки данных
- отдаёт `Bytes` stream вверх (HLS/file).

### Зависимости
- `reqwest`, `tokio`, `bytes`, `url`, `thiserror` (и минимум остального)

---

## 4) `kithara-file` (раньше `kithara-progressive`)

### Цель
Источник “один файл по HTTP” (mp3) с lazy caching и возможностью seek через range.

### Компоненты и воркеры (контракт)
- `FileSource::open(...)`:
  - создаёт control-plane (sender/receiver команд),
  - создаёт bytes stream (data-plane),
  - запускает **ровно один** async `FileDriver` loop,
  - возвращает `FileSession` как handle на живой pipeline.
- `FileDriver` обязан:
  - уважать backpressure `kithara-io`,
  - на `Stop` завершать stream и вызывать `finish()`,
  - на fatal error завершать stream и вызывать `finish()` (без зависаний),
  - не возвращать “ложный EOF” через `Read` (это обязанность bridge + корректное завершение).

### Публичный контракт (экспорт)

- `FileSource`
  - `async fn open(url: Url, opts: FileSourceOptions, cache: Option<AssetCache>) -> Result<FileSession, FileError>`

- `FileSession`
  - `fn asset_id(&self) -> AssetId`
  - `fn commands(&self) -> CommandSender<FileCommand>` (или унифицированный `SourceCommand`)
  - `fn stream(&self) -> impl Stream<Item=Result<Bytes, FileError>>`

- `FileCommand`
  - `Stop`
  - `SeekBytes(u64)` (и/или `SeekTime(Duration)` если нужен перевод времени — обычно это на decode уровне)
  - `SetCachePolicy(...)` (опционально)

**Кэширование**
- если `cache` включён:
  - хранить в asset tree (например `file/body` без extension или с расширением из URL)
  - для начала можно “download-through cache” (сразу пишем, пока читаем)
  - seek через Range при cache miss; при hit можно читать локально (опционально на v1)

> Контракт допускает постепенное развитие: сначала caching whole-body; потом range caching.

### Тесты (TDD)
- `reopen_same_url_uses_cache_offline`:
  - 1-й прогон с сетью пишет в кэш
  - 2-й прогон без сети читает из кэша
- `seek_uses_range_when_not_cached` (локальный сервер)
- `asset_id_is_stable_without_query`

### Потоки данных
- bytes -> `kithara-io`.

### Зависимости
- `kithara-core`, `kithara-net`, `kithara-cache` (опционально), `tokio`, `bytes`

---

## 5) `kithara-hls`

### Цель
HLS VOD orchestration + caching everything (playlists, segments, keys, processed keys), ABR policy parameter.

### Компоненты и воркеры (контракт)
- `HlsSource::open(...)`:
  - создаёт control-plane (sender/receiver команд),
  - создаёт bytes stream (data-plane),
  - запускает **ровно один** async `HlsDriver` loop,
  - возвращает `HlsSession` как handle на живой pipeline.
- `HlsDriver` обязан:
  - уважать backpressure `kithara-io`,
  - корректно завершать сессию (`finish()`) при успехе и при fatal error,
  - поддерживать offline_mode (cache miss => fatal),
  - поддерживать DRM keys (query params, headers, processor) и caching processed keys.

### Публичный контракт (экспорт)

- `HlsSource`
  - `async fn open(url: Url, opts: HlsOptions, cache: AssetCache, net: NetClient) -> Result<HlsSession, HlsError>`

- `HlsSession`
  - `fn asset_id(&self) -> AssetId`
  - `fn commands(&self) -> CommandSender<HlsCommand>`
  - `fn stream(&self) -> impl Stream<Item=Result<Bytes, HlsError>>`

- `HlsCommand`
  - `Stop`
  - `SeekTime(Duration)` (best-effort, абсолютный)
  - `SetVariant(VariantId)` / `ClearVariantOverride`
  - (опционально) `PrefetchBestVariant` (не v1)

- `HlsOptions` (контракт зафиксирован на основе `stream-download-hls::HlsSettings`)
  - URL resolution
    - `base_url: Option<Url>`
      - base URL override used to form final URLs for:
        - media playlists (variant URIs in master playlist)
        - segments
        - encryption keys
      - When `None`, URL resolution falls back to the relevant playlist URL.
  - Variant selection / ABR
    - `variant_stream_selector: Option<Arc<dyn Fn(&MasterPlaylist) -> Option<VariantId> + Send + Sync>>`
      - returns `None` => AUTO (ABR-controlled)
      - returns `Some(id)` => MANUAL (locked to that variant)
    - `abr_initial_variant_index: Option<usize>`
      - AUTO startup hint (does NOT lock ABR into manual)
      - out of bounds => clamp to last available variant
    - ABR tuning:
      - `abr_min_buffer_for_up_switch: f32`
      - `abr_down_switch_buffer: f32`
      - `abr_throughput_safety_factor: f32`
      - `abr_up_hysteresis_ratio: f32`
      - `abr_down_hysteresis_ratio: f32`
      - `abr_min_switch_interval: Duration`
  - Network / retry behavior
    - `request_timeout: Duration` (single HTTP op)
    - `max_retries: u32`
    - `retry_base_delay: Duration`
    - `max_retry_delay: Duration`
    - `retry_timeout: Duration` (retrying stream ops when no new data is available)
  - HLS buffering
    - `prefetch_buffer_size: usize`
  - Live-related (VOD-only сейчас, но поле фиксируем в контракте без реализации)
    - `live_refresh_interval: Option<Duration>`
  - DRM / keys (обязательно предусмотреть в контракте)
    - `key_processor_cb: Option<Arc<dyn Fn(Bytes, KeyContext) -> Result<Bytes, KeyError> + Send + Sync>>`
      - used to post-process fetched keys (e.g., unwrap DRM)
      - processed key bytes MUST be cached for offline playback
    - `key_query_params: Option<HashMap<String, String>>`
      - appended to key fetch requests
    - `key_request_headers: Option<HashMap<String, String>>`
      - added to key fetch requests
  - Offline mode
    - `offline_mode: bool`
      - if `true`, any cache miss must fail (no network fetch)

**Кэш layout**
- Внутри `assets/<asset_id>/hls/...`:
  - playlists: master/media
  - segments: per variant, per seg_hash
  - keys:
    - raw (опционально)
    - processed (обязательно для offline расшифровки)
- Имена файлов:
  - если у URL был basename с extension — сохранить
  - если extension нет — файл без extension

**ResourceHash**
- для каждого ресурса используем hash URL с query (для динамики/DRM).
- segment key учитывает byterange (если понадобится).

**DRM keys (контракт)**
- поддержать:
  - `key_query_params` (добавляются к key URL запросам),
  - `key_request_headers` (добавляются к key requests),
  - `key_processor_cb` (unwrap/transform key bytes).
- кэшировать **processed keys** после `key_processor_cb` (именно они должны быть доступны offline для расшифровки).
- никогда не логировать key bytes/секреты (допустим только fingerprint/hash).

### Тесты (TDD)
- локальная HLS fixture (axum) без внешней сети:
  - `caches_all_resources_for_offline_playback`:
    - run online: load master+media+init+segments+keys
    - run offline: playback continues from cache (miss -> error only if not cached)
  - `can_upgrade_variant_on_second_run`:
    - run 1: variant A partially cached
    - run 2: choose better variant B, segments B cached; A remains
  - `processed_keys_are_cached`:
    - fixture выдаёт “wrapped key”, cb unwraps, offline run decrypts

> VOD only: playlist фиксированный, no live refresh loop.

### Потоки данных
- net/cached resources -> bytes stream -> `kithara-io`.

### Зависимости
- `kithara-core`, `kithara-cache`, `kithara-net`, `hls_m3u8`, `tokio`, `bytes`

---

## 6) `kithara-io`

### Цель
Bridge: async bytes source -> sync `Read+Seek` для decode.

### Компоненты и потоки (контракт)
- `BridgeWriter` используется источниками (async drivers) для записи bytes.
- `BridgeReader` используется decode worker (blocking) для чтения bytes.
- `kithara-io` отвечает за:
  - bounded buffering (backpressure),
  - корректную семантику EOF,
  - отсутствие “ложных EOF” (`Ok(0)` до EOS),
  - минимальную блокировку только там, где она допустима (decode thread).

### Публичный контракт (экспорт)

- `ByteSource` trait (унифицирует источники):
  - `type Error`
  - `fn stream(&mut self) -> impl Stream<Item=Result<Bytes, Self::Error>>`
  - `fn command_sender(&self) -> CommandSender<...>` (опционально)
  - или более простой контракт, если `kithara-io` получает уже `Stream` и `CommandSender` отдельно.

- `BridgeWriter` (async side)
  - `async fn push(&self, bytes: Bytes) -> Result<(), IoError>`
  - `async fn finish(&self) -> Result<(), IoError>` (EOS)
  - bounded по bytes (backpressure)

- `BridgeReader` (sync side)
  - `impl Read`
  - `impl Seek` (best-effort; минимум: поддержка seek на уровне “пересоздать источник”/epoch — зависит от источника)

- `Bridge` constructor:
  - `fn new(opts: BridgeOptions) -> (BridgeWriter, BridgeReader)`
- `BridgeOptions`
  - `max_buffer_bytes`
  - (опционально) watermark/metrics hooks

**Критичный инвариант**
- `Read::read()` никогда не возвращает `Ok(0)` до реального EOS.
- На “нет данных” reader блокируется (decode thread).

### Тесты (TDD)
- `read_blocks_until_data_then_reads`:
  - в отдельном потоке делаем `read()`, убеждаемся что не возвращает 0
  - пушим bytes, read возвращает >0
- `read_returns_0_only_after_finish`
- `backpressure_blocks_push_when_full` (тестируем bounded behavior)
- `seek_behavior_contract`:
  - если seek unsupported -> возвращаем `Err` (не “тихий успех”)
  - если supported -> корректное позиционирование по уже буферизованным данным (минимум)

### Потоки данных
- источники (HLS/file) пушат bytes в writer
- decoder читает из reader

### Зависимости
- `bytes`, `kanal` (или `tokio` + `kanal`), `thiserror`

---

## 7) `kithara-decode` (generic decoder, дизайн вдохновлён `decal`)

### Цель
Декодирование в PCM, generic по типу сэмпла `T`. Не привязано к HLS/HTTP.

### Компоненты и потоки (контракт)
- Decode выполняется в blocking worker (отдельный поток или `spawn_blocking`), чтобы:
  - не блокировать tokio runtime,
  - не лезть в audio thread хоста.
- Декодер читает `Read + Seek` из `kithara-io`.
- Ошибки decode делятся на:
  - recoverable (если применимо: например, “конец эпохи/сегмента” в будущем),
  - fatal (unsupported format/codec, повреждённый поток).
- На fatal decode error pipeline должен завершаться предсказуемо (без зависаний).

### Публичный контракт (экспорт)

**Decoder**
- `Decoder<T>`
  - `fn new(source: Box<dyn Source>, settings: DecoderSettings) -> Result<Decoder<T>, DecodeError>`
  - `fn seek(&mut self, pos: Duration) -> Result<(), DecodeError>` (best-effort)
  - `fn next(&mut self) -> Result<Option<PcmChunk<T>>, DecodeError>` (pull API)
  - (или `DecoderStream<T>` с каналом `kanal` — push API; выбрать один для v1)

**Source**
- абстракция “что Symphonia читает” (по мотивам `decal`):
  - предоставляет `MediaSource`/`MediaSourceStream` для Symphonia
  - `fn file_ext(&self) -> Option<&str>` как Hint для probe (важно для стабильного определения формата)

> Это ключ: `kithara-io` предоставляет sync `Read+Seek`, а `kithara-decode` потребляет его через `Source` без знания про HLS/HTTP.

**PCM**
- `PcmChunk<T>`
  - interleaved samples
  - `spec: AudioSpec` (sample_rate, channels)
  - `frames: usize`

**Generic**
- `T` — generic sample type:
  - поддерживаем минимум `f32`, `i16`
  - можно взять подход `decal` (dasp Sample + ConvertibleSample), но это решаем на этапе реализации.

### Тесты (TDD)
- unit tests без сети:
  - декодирование mp3/aac/flac тестового маленького ассета (можно встроить как bytes fixture)
- integration tests (позже) через `kithara-io`:
  - push bytes -> decode produces samples
- `seek_best_effort_does_not_deadlock`:
  - seek на середину -> decode continues

### Потоки данных
- sync Read+Seek -> Symphonia -> PCM chunks.

### Зависимости
- `symphonia`, `kanal`, `thiserror` (+ возможно `dasp` если выберем такой trait-bound, как в `decal`)

---

## 7.a) `kithara-decode` — Audio pipeline (generic base + bounded PCM queue; по мотивам `stream-download-audio`)

### Цель
Добавить поверх декодера “аудио-пайплайн” уровня плеера:
- bounded backpressure на уровне PCM (producer waits; consumer non-blocking)
- стабильный контракт chunk’ов (frame-aligned, interleaved)
- командный канал (seek; а HLS-специфичные команды остаются на уровне source/driver и не “протекают” в decoder)

Эта секция фиксирует план и контракт, вдохновлённый `stream-download-audio` и общей идеологией `decal` (generic + слабая связанность).

### Компоненты (контракт и ответственность)

#### `PcmSpec` / `PcmChunk<T>`
- `PcmSpec { sample_rate: u32, channels: u16 }`
- `PcmChunk<T> { spec: PcmSpec, pcm: Vec<T> }`
  - interleaved
  - `pcm.len() % channels == 0` (frame aligned)
  - `T` для v1: минимум `f32`, опционально `i16`

#### `DecodeCommand`
- `Seek(Duration)`
- (дальше по мере надобности) `Stop` если потребуется “раньше EOS” на decode-уровне.
> HLS-variant switching не часть `kithara-decode`: это source/driver concern.

#### `AudioSource<T>` (синхронный trait)
Синхронный интерфейс, который можно гонять в worker’е (как в `stream-download-audio`):
- `fn output_spec(&self) -> Option<PcmSpec>`
- `fn next_chunk(&mut self) -> Result<Option<PcmChunk<T>>, DecodeError>`
- `fn handle_command(&mut self, cmd: DecodeCommand) -> Result<(), DecodeError>`

Это позволяет:
- пересоздавать внутренний Symphonia decoder при “смене эпохи/формата”
- реализовать seek как best-effort без привязки к runtime.

#### `DecodeEngine` (Symphonia glue)
Внутренний компонент (не обязательно публичный), который:
- открывает `FormatReader + Decoder` по `MediaSource`
- читает packets, декодирует в PCM
- умеет `reset/reopen` (для смены codec/track/epoch)
- реализует time-based `seek(Duration)` через Symphonia, если доступно.

#### `AudioStream<T>` (high-level, async consumer API)
Async-стрим PCM chunks с bounded queue:
- producer (worker) блокируется при переполнении (no drops)
- consumer non-blocking: если нет данных, `poll_next => Pending`
- EOS: `Ready(None)`
- Fatal: `Some(Err(..))` и завершение потока

Очередь/каналы:
- использовать `kanal` (как принято в workspace) для bounded semantics.

#### Worker model
- один worker на `AudioStream`:
  - либо dedicated thread
  - либо `tokio::task::spawn_blocking`
- выбор реализации не влияет на контракт: важно, что decode не блокирует tokio runtime.

### Инварианты (обязательные)
- Никаких silent drops PCM из-за переполнения: producer ждёт.
- Chunk’и всегда frame-aligned.
- `output_spec()` может меняться при смене decoder’а/формата; каждый `PcmChunk` несёт актуальный `spec`.
- Ошибки различать:
  - `EndOfStream` (или `Ok(None)`) как нормальное завершение
  - fatal decode/io как ошибка и затем завершение.

### Тесты (TDD)
- `pcm_chunks_are_frame_aligned`
- `producer_waits_when_queue_full` (bounded backpressure)
- `consumer_is_non_blocking_when_empty` (Pending, без busy loop)
- `fatal_error_terminates_stream_after_error_item`
- `seek_best_effort_does_not_deadlock`
- unit: маленький встроенный аудио asset (mp3/aac/flac) → produces >0 frames

### Потоки данных
- `kithara-io::BridgeReader (Read+Seek)` -> `kithara-decode::DecodeEngine` -> bounded PCM queue -> consumer (rodio/examples)

### Зависимости
- `symphonia`, `kanal`, `thiserror`
- (опционально) `dasp` / sample-traits если решим унифицировать `T` как в `decal`

---

## 7.b) `kithara-decode` — Decoder upgrade (generic `Decoder<T>` как в `decal` + contracts из `stream-download-audio`)

### Цель
Усилить декодерный слой до “базы”, похожей на `decal::Decoder<T>`:
- generic выходной sample type `T` (минимум `f32`, опционально `i16`)
- корректная multi-channel обработка (не только mono/stereo)
- явный `Source` контракт (media source + file extension hint), чтобы Symphonia probe был стабильным и тестируемым
- предсказуемые семантики seek/reset (codec-switch-safe)

### Компоненты (контракт и ответственность)

#### `Source` (по мотивам `decal::decoder::source`)
Абстракция над входом Symphonia:
- отдаёт `MediaSource` (или `Read+Seek` через `MediaSourceStream`)
- предоставляет `file_ext()` / hint для probe (в отличие от старого `Hint::new()` без extension)

Контракт:
- `kithara-io` даёт `Read+Seek` reader; `kithara-decode` не знает про HLS/HTTP.

#### `DecoderSettings`
Минимальный набор настроек Symphonia:
- `enable_gapless: bool` (как в decal)
- (опционально позже) дополнительные knobs

#### `Decoder<T>`
State machine вокруг Symphonia:
- `new(source, settings)`:
  - строит `Hint` с extension (если есть)
  - делает probe -> `FormatReader`
  - выбирает audio track
  - создаёт `AudioDecoder`
  - извлекает `sample_rate`, `channels`, `time_base`, `num_frames` (если есть)
- `next()`:
  - читает packet’ы до нужного track_id
  - декодирует
  - конвертирует sample format в `T`
  - возвращает `PcmChunk<T>` (interleaved, frame-aligned)
- `seek(Duration)`:
  - `FormatReader::seek(SeekMode::Accurate, SeekTo::Time{...})`
  - `AudioDecoder::reset()`
- `reset/reopen`:
  - сбрасывает decoder/reader и переинициализирует, чтобы переживать смену формата/кодека (codec-switch-safe)

#### `AudioStream<T>`
Высокоуровневый async API остаётся как в 7.a:
- bounded queue
- consumer non-blocking
- команды (минимум seek) прокидываются в worker, который вызывает `Decoder<T>::seek`

### Инварианты (обязательные)
- `PcmChunk<T>` всегда interleaved и frame-aligned.
- Не допускать “stereo-only” логики: корректно обрабатывать N каналов.
- Ошибки:
  - recoverable (если Symphonia возвращает recoverable) не должны валить весь поток без причины
  - fatal => один `Err` item и завершение.
- Seek best-effort: не deadlock, не “тихий успех” при невозможности (возвращаем ошибку).

### Тесты (TDD)
- `probe_uses_file_extension_hint` (через fake Source, где extension влияет на hint)
- `decodes_multichannel_frame_alignment` (на fixture с >2 каналов, либо синтетический формат если доступен)
- `decode_produces_pcm_f32` (минимум)
- `seek_resets_decoder_and_continues`
- `stream_contract_pending_when_empty` (на уровне AudioStream)
- integration: `kithara-io` bridge -> decode produces samples (маленький встроенный mp3/aac)

### Зависимости
- `symphonia`, `thiserror`, `kanal`
- `dasp` (если берём sample conversion/bounds как в `decal`)

---

# Канбан-борды (задачи) по сабкрейтам

Ниже — примерная структура колонок. Агенты могут вести задачи по этому документу.

Колонки:
- **Backlog**
- **Ready**
- **In Progress**
- **Review**
- **Done**

Для каждого сабкрейта минимальный MVP:

## `kithara-core` MVP
- [ ] Define `AssetId` and canonicalization rules (+ tests)
- [ ] Define `ResourceHash` rules (+ tests)
- [ ] Define shared error scaffolding

## `kithara-core` — Port legacy identity scenarios (tests)
- [ ] Canonicalization invariants for `AssetId` (URL without query/fragment) + tests:
  - ignores query
  - ignores fragment
  - stable across repeated parsing/formatting
- [ ] Canonicalization invariants for `ResourceHash` (URL with query, without fragment) + tests:
  - includes query
  - ignores fragment
  - differs when query differs (same base URL)
- [ ] (Prereq if missing) Add explicit error type(s) for invalid canonicalization inputs + tests:
  - invalid/unsupported URL shapes produce typed errors (no panics)

## `kithara-core` — Refactor (module boundaries; keep public contract)
- [ ] Split into small modules if file grows: `asset_id`, `resource_hash`, `errors` (no behavior change) + keep tests green
- [ ] Ensure no “settings creep”: options must remain outside `kithara-core` + add compile-time/doc guard (short comment or doc test)
- [ ] Add crate-level docs: “what belongs here / what must not” (short, contract-level)

## `kithara-cache` MVP
- [ ] FS layout + safe `CachePath`
- [ ] `put_atomic` temp+rename + tests
- [ ] `exists/open` via metadata + tests
- [ ] `state.json` index load/save atomic + tests
- [ ] `max_bytes` eviction loop (multi-asset) + tests
- [ ] pin/lease + tests

## `kithara-cache` — Port legacy HLS persistence semantics (tests)
- [ ] Persistent cache warmup: after first run that reads at least N bytes from an HLS VOD session, the persistent cache root is non-empty; after second run (same root), it remains non-empty (do NOT assert “no network refetch”) + tests
- [ ] Persistent mode should create on-disk files after reading some bytes (smoke: read >0 then assert cache root has files) + tests
- [ ] Memory/offline-only resource caching should NOT create many on-disk segment files:
  - after reading >0 bytes, number of files on disk stays within a small bound (playlists/keys allowed, but not segment explosion) + tests
- [ ] (Prereq if missing) Add test utilities to count files recursively and assert non-empty dirs (test-only helpers) + tests

## `kithara-cache` — Layered architecture (decorators/generics; keep public contract)
- [ ] Refactor internals into explicit layers/modules (base/index/lease/evict/policy) without changing the public API
- [ ] Define internal minimal “store” contract for blob ops (exists/open/put_atomic/remove_all) + unit tests with a temp dir
- [ ] Implement `FsStore` base layer (tree-friendly layout + atomic write) and make it pass the existing MVP tests
- [ ] Implement `IndexStore<S>`: `state.json` atomic load/save + total_bytes/per-asset metadata; ensure FS remains source of truth
- [ ] Implement `LeaseStore<S>`: `LeaseGuard`/pin semantics and lease file/touch behavior + tests
- [ ] Implement `EvictingStore<S, P>` with policy trait `P` (LRU default) + tests:
  - evicts multiple assets until fit
  - never evicts pinned
  - updates index consistently
- [ ] Wire `AssetCache` facade to compose layers (Fs + Index + Lease + Evict) while preserving the existing public contract and tests
- [ ] Add crate-level docs (`crates/kithara-cache/README.md`): layering, invariants, and what is source of truth

## `kithara-cache` — Refactor (layering-ready; keep public contract)
- [ ] Split internal modules to match responsibilities (base/index/lease/evict/policy) without changing public API + keep tests green
- [ ] Remove “misc utils dumping-ground”: move helpers into focused modules (e.g. `fs_layout`, `atomic_write`, `lru_index`) + keep tests green
- [ ] Add crate-level docs: invariants + “FS is source of truth” + what is stored in `state.json`

## `kithara-net` MVP
- [ ] `NetClient` wrapper + options
- [ ] stream GET bytes + tests with local server
- [ ] range GET + tests
- [ ] header support (key requests) + tests

## `kithara-net` — HLS key request semantics (tests)
- [ ] Headers passthrough for key fetches (client sends required headers; server validates and otherwise fails) + tests
- [ ] Query params passthrough for key fetches (client appends required query params; server validates and otherwise fails) + tests
- [ ] (Prereq if missing) Add a small local server fixture that can:
  - require specific headers/query params for a path
  - record per-path request counts for assertions

## `kithara-net` — Full client (retry/timeout; layered; generics-first)
- [ ] Refactor: split `kithara-net` into modules `base/traits/types/retry/timeout/builder` without changing current behavior
- [ ] Add `ByteStream` alias and core trait `Net` (+ `NetExt` for composition)
- [ ] Implement `ReqwestNet` base layer with explicit status handling (no silent success on 4xx/5xx) + tests
- [ ] Remove `expect/unwrap` from prod code: client construction returns `Result` with typed error + tests
- [ ] Implement `TimeoutNet<N>` decorator:
  - `get_bytes`: whole-op timeout
  - `stream/get_range`: timeout only for request + response headers
  - deterministic tests (no flaky sleeps)
- [ ] Implement `RetryNet<N, P>` decorator with `RetryPolicy`:
  - exponential backoff with cap
  - retry only before body streaming (no mid-stream retry in v1)
  - tests with local axum fixture: N failures then success
- [ ] Define retry classification rules (network errors + HTTP 5xx + 429; decision about 408 фиксируется тестами) + tests per status
- [ ] Provide `NetBuilder` / `create_default_client()` that composes base+retry+timeout into a convenient facade
- [ ] Ensure existing behavior stays covered: header passthrough + range semantics (tests must remain green after refactor)
- [ ] Add short crate-level docs (`crates/kithara-net/README.md`): layering, retry semantics, timeout semantics

## `kithara-net` — Refactor (layering-ready; keep public contract)
- [ ] Split into `base/traits/types` modules without behavior change; keep tests green
- [ ] Centralize error mapping (reqwest/status/timeout) into one place to avoid duplication + keep tests green
- [ ] Add crate-level docs: what is considered retriable, what timeout means (contract-level)

## `kithara-net` — Port legacy “key cached / not fetched per segment” support (prereq tasks)
- [ ] Expose a way for upper layers (`kithara-hls`) to reuse fetched bytes without re-requesting:
  - define a minimal cache interface or hook point at the HLS layer (NOT inside net)
  - ensure net itself stays stateless by default
- [ ] Add server-side request counting utilities in tests so `kithara-hls` can assert “key requested <= N times” deterministically

## `kithara-file` MVP
- [ ] open session + asset_id from URL + tests
- [ ] stream bytes from net + tests
- [ ] cache-through write (optional) + offline reopen test
- [ ] seek via Range (not cached) + tests

## `kithara-file` — Port legacy “read/seek correctness” scenarios (tests)
- [ ] Seek roundtrip correctness:
  - read first N bytes, seek to 0, read again, bytes match reference (no corruption, no deadlock) + tests
- [ ] Seek variants:
  - `SeekFrom::Start`, `SeekFrom::Current`, `SeekFrom::End` produce expected slices on a known static resource + tests
- [ ] Cancel/stop behavior:
  - after reading some bytes, send `Stop` (or drop session) and ensure the stream terminates promptly without hanging + tests
- [ ] (Prereq if missing) Define precise seek contract for `kithara-file` session:
  - what happens when seeking beyond cached/buffered range
  - what errors are returned (typed) + tests

## `kithara-file` — Refactor (driver/session separation; keep public contract)
- [ ] Ensure `FileDriver` loop, session handle, and options are in separate modules when the crate grows (no behavior change) + keep tests green
- [ ] Isolate “range/seek policy” code into a dedicated module (no behavior change) + keep tests green
- [ ] Add crate-level docs: seek contract + offline/cache interaction (short)

## `kithara-hls` MVP
- [ ] parse master/media playlists (VOD) + tests with local fixture
- [ ] variant selection policy (parameter) + tests
- [ ] resource caching (playlists/init/segments) + offline test
- [ ] keys + processed keys caching + test scenario with wrapped key
- [ ] stream bytes for a variant + basic read smoke test

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

## `kithara-hls` — Port legacy base_url and URL resolution scenarios (tests)
- [ ] base_url override:
  - when segments are remapped under a path prefix, default resolution fails (404)
  - configuring base_url makes it succeed for varying prefix depths (`a/`, `a/b/`, `a/b/c/`)
- [ ] (Prereq if missing) Ensure URL resolution rules are explicitly testable:
  - base URL override applied for variant playlists, segments, and keys as per contract

## `kithara-hls` — Port legacy VOD completion + caching surface scenarios (tests)
- [ ] VOD completes and closes stream:
  - drain until stream ends; assert all segments for selected variant were requested at least once
- [ ] Manual variant selection emits only selected variant data:
  - start in manual mode for different variant ids
  - verify output “belongs” to that variant (by deterministic payload prefixes)
- [ ] AUTO startup begins on variant 0 by default
- [ ] AUTO startup respects `abr_initial_variant_index`

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

## `kithara-hls` — Port legacy seek scenarios (tests)
- [ ] Seek correctness (best-effort, absolute) across segment boundary:
  - seek to offset that straddles end of segment N and beginning of segment N+1
  - read contiguous bytes and assert they match expected concatenation
- [ ] Seek known offsets return expected bytes:
  - after a warmup read, seek to known offsets and read exact prefixes (init/segment slices)
- [ ] (Prereq if missing) Define/confirm seek contract for HLS:
  - absolute seek by time or bytes (as per current public API)
  - behavior across segment boundaries is contiguous and deterministic within fixture constraints

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

## `kithara-hls` — Refactor (manager/worker/policies; keep public contract)
- [ ] Split into modules for `fixture` (test-only), `playlist`, `fetch`, `keys`, `abr`, `driver/worker`, `events` as size grows (no behavior change) + keep tests green
- [ ] Ensure ordered control-plane/data-plane semantics remain explicit (avoid out-of-band surprises) + add focused tests
- [ ] Add crate-level docs: URL resolution rules (`base_url`), DRM key processing/caching, ABR switching invariants

## `kithara-io` MVP
- [ ] bounded bridge (writer/reader) + tests for EOF semantics
- [ ] backpressure behavior + tests
- [ ] initial seek contract decision (explicit errors if unsupported) + tests

## `kithara-io` — Port legacy backpressure/seek edge cases (tests)
- [ ] Backpressure: writer blocks when buffer full; reader unblocks it by draining (bounded by bytes) + tests
- [ ] Seek contract: seeking beyond available buffered data returns a typed error (no silent success) + tests
- [ ] EOS correctness: `Read::read()` returns `Ok(0)` only after finish/EOS; never earlier + tests

## `kithara-io` — Refactor (small modules; keep public contract)
- [ ] Split into `bridge`, `reader`, `writer`, `errors` modules when code grows (no behavior change) + keep tests green
- [ ] Consolidate synchronization/backpressure primitives into one place to avoid duplicated invariants + keep tests green
- [ ] Add crate-level docs: EOF semantics and seek contract (short, normative)

## `kithara-decode` MVP
- [ ] generic sample plumbing (choose trait bounds, like decal) + unit tests
- [ ] minimal Symphonia wrapper: open -> decode -> PCM chunk
- [ ] seek(Duration) best-effort
- [ ] integration test: bridge->decode for a small audio asset

## `kithara-decode` — Port legacy audio pipeline scenarios (tests)
- [ ] PCM invariants:
  - emitted `PcmChunk<T>` is interleaved and frame-aligned (`len % channels == 0`)
  - `channels > 0`, sample_rate > 0 + tests
- [ ] Full drain closes stream:
  - decode a finite HTTP MP3-like asset and ensure PCM stream terminates (EOS) + tests
- [ ] HLS VOD decode drains sequentially without repeats (variant-independent):
  - run with manual variant selection (or AUTO) and ensure PCM drain ends + tests
- [ ] Codec switch reinitializes decoder and PCM continues:
  - fixture switches codec between segments/variants
  - decoder resets/reopens and continues producing PCM + tests
- [ ] Seek scrubbing:
  - multiple forward/backward `Seek(Duration)` commands do not deadlock and result in continued PCM production + tests
- [ ] Ordered boundaries (best-effort):
  - optionally expose best-effort “init boundary” / “segment boundary” events to validate ordering + tests

## `kithara-decode` — Prerequisites for ported audio scenarios (only if missing in plan)
- [ ] Ensure `kithara-decode` exposes a bounded PCM queue stream API (`AudioStream<T>`):
  - producer waits, consumer non-blocking
  - fatal error emits one Err item then terminates
- [ ] Ensure decode worker can accept commands (`DecodeCommand::Seek`) and apply them best-effort
- [ ] Provide deterministic local fixtures for decode tests (no external network):
  - tiny MP3/AAC test assets embedded or served by local server

## `kithara-decode` — Audio pipeline (bounded PCM queue; stream-download-audio-inspired)
- [ ] Define PCM public types: `PcmSpec`, `PcmChunk<T>` (interleaved, frame-aligned) + tests
- [ ] Define `DecodeCommand` (at least `Seek(Duration)`) and `AudioSource<T>` synchronous trait + tests with a fake source
- [ ] Implement `DecodeEngine` (Symphonia glue) with “reset/reopen” hooks to be codec-switch-safe + unit tests
- [ ] Implement `AudioStream<T>`:
  - producer waits when queue full (bounded backpressure)
  - consumer non-blocking (`poll_next` => Pending when empty)
  - EOS and fatal error termination semantics
- [ ] Implement worker loop that drives `AudioSource<T>` and pushes chunks into bounded queue (thread or `spawn_blocking`) + tests
- [ ] Add seek plumbing: `AudioStream` forwards `DecodeCommand::Seek` into worker/source; ensure no deadlocks + tests
- [ ] (Optional, later) Add `rodio` adapter in `kithara-examples` crate (not inside `kithara-decode`) to keep decode crate minimal

## `kithara-decode` — Decoder upgrade (decal-style `Decoder<T>` + Source contract)
- [ ] Add `Source` trait (MediaSource + `file_ext()` hint) + tests
- [ ] Add `DecoderSettings` (gapless toggle as baseline) + tests
- [ ] Implement `Decoder<T>` core state machine:
  - probe with hint/extension
  - pick default audio track
  - decode packets for selected track id
  - convert to interleaved `PcmChunk<T>`
- [ ] Fix multi-channel support (not stereo-only) + tests
- [ ] Implement `seek(Duration)` via Symphonia seek + `decoder.reset()` + tests
- [ ] Add reset/reopen hooks to be codec-switch-safe (format/codec changes) + tests
- [ ] Wire `Decoder<T>` into `AudioStream<T>` worker (bounded queue semantics preserved) + integration tests

## `kithara-decode` — Refactor (decoder vs pipeline; keep public contract)
- [ ] Separate low-level `Decoder<T>` (state machine) from high-level `AudioStream<T>` pipeline modules (no behavior change) + keep tests green
- [ ] Centralize Symphonia glue (probe/track selection/seek/reset) to avoid duplicated logic + keep tests green
- [ ] Add crate-level docs: sample type `T`, invariants on `PcmChunk`, and command semantics

---

# Инструкции для агентов (обязательная преамбула к каждой задаче)

Перед началом любой задачи агент:
1) читает `AGENTS.md`
2) читает `docs/constraints.md`
3) читает раздел релевантного сабкрейта в этом файле (`docs/kanban.md`)
4) начинает с TDD: добавляет тест, который падает, затем реализует минимально

Если агенту нужна новая зависимость:
- сначала добавить её в workspace root `Cargo.toml` (`[workspace.dependencies]`),
- затем подключить в сабкрейте через `{ workspace = true }`.

---

# Примечания по расхождениям с decal

`decal` использует Symphonia probe (`get_probe().probe(...)`) и Hint по extension.
В `kithara-decode` мы можем:
- использовать Hint по `file_ext()` как у decal,
- но избегать “probe как отдельный внешний этап”: он должен быть внутренней деталью `Decoder::new()`.

При необходимости “декодировать сразу через декодеры”:
- это будет оформлено в `kithara-decode` как стратегия выбора формата/декодера,
- но внешний контракт для пользователя остаётся простым: `Decoder::new(source)`.

---
