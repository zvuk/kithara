# Kithara — Kanban plans (per subcrate)

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

---

# Общая модель системы (абстракции, воркеры, многопоточность, потоки данных)

## Термины: Asset vs Resource

- **Asset** — “трек/контент” как единица кэша и eviction (LRU).
  - Идентифицируется `AssetId`.
  - **Не зависит от query/fragment** URL.

- **Resource** — конкретный сетевой объект, который нужно получить/закэшировать:
  - playlist, segment, init, key, mp3 bytes, и т.д.
  - Идентифицируется `ResourceHash`.
  - **Включает query**, fragment игнорируется.

## Многопоточность и воркеры (high-level)

### Потоки/задачи
- **Tokio runtime threads**: async задачи сети и оркестрации (file/hls). Количество потоков задаёт хост (обычно multi-thread runtime).
- **Decode thread (blocking)**: отдельный поток (или `spawn_blocking`), который синхронно читает `Read + Seek` и декодирует через Symphonia.
- **Audio output thread** (вне `kithara`): в хосте/движке; читает PCM из очереди/канала. `kithara` не делает I/O с устройством.

### Воркеры и циклы обработки

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
- `Stop`
- `Seek` (best-effort, абсолютный)
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

**DRM keys**
- кэшировать processed keys после `key_processor_cb`.
- никогда не логировать key bytes.

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

### Публичный контракт (экспорт)

**Decoder**
- `Decoder<T>`
  - `fn new(source: Box<dyn MediaSource>, settings: DecoderSettings) -> Result<Decoder<T>, DecodeError>`
  - `fn seek(&mut self, pos: Duration) -> Result<(), DecodeError>` (best-effort)
  - `fn next(&mut self) -> Result<Option<PcmChunk<T>>, DecodeError>` (pull API)
  - (или `DecoderStream<T>` с каналом `kanal` — push API; выбрать один для v1)

**MediaSource**
- абстракция “что Symphonia читает”:
  - `fn reader(&self) -> Box<dyn Read + Seek + Send>` (или другой контракт)
  - `fn file_ext(&self) -> Option<&str>` (как Hint, как у decal `get_file_ext()`)

> Это ключ: `kithara-io` предоставляет sync reader, а `kithara-decode` потребляет его без знания про HLS/HTTP.

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

## `kithara-cache` MVP
- [ ] FS layout + safe `CachePath`
- [ ] `put_atomic` temp+rename + tests
- [ ] `exists/open` via metadata + tests
- [ ] `state.json` index load/save atomic + tests
- [ ] `max_bytes` eviction loop (multi-asset) + tests
- [ ] pin/lease + tests

## `kithara-net` MVP
- [ ] `NetClient` wrapper + options
- [ ] stream GET bytes + tests with local server
- [ ] range GET + tests
- [ ] header support (key requests) + tests

## `kithara-file` MVP
- [ ] open session + asset_id from URL + tests
- [ ] stream bytes from net + tests
- [ ] cache-through write (optional) + offline reopen test
- [ ] seek via Range (not cached) + tests

## `kithara-hls` MVP
- [ ] parse master/media playlists (VOD) + tests with local fixture
- [ ] variant selection policy (parameter) + tests
- [ ] resource caching (playlists/init/segments) + offline test
- [ ] keys + processed keys caching + test scenario with wrapped key
- [ ] stream bytes for a variant + basic read smoke test

## `kithara-io` MVP
- [ ] bounded bridge (writer/reader) + tests for EOF semantics
- [ ] backpressure behavior + tests
- [ ] initial seek contract decision (explicit errors if unsupported) + tests

## `kithara-decode` MVP
- [ ] generic sample plumbing (choose trait bounds, like decal) + unit tests
- [ ] minimal Symphonia wrapper: open -> decode -> PCM chunk
- [ ] seek(Duration) best-effort
- [ ] integration test: bridge->decode for a small audio asset

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
