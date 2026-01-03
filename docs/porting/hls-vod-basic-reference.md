# HLS VOD basic playback reference (legacy-inspired, DRM-aware but not a blocker)

> Актуально: orchestration loop вынесен в `kithara-stream`.
> - `kithara-hls` реализует `kithara-stream::Source` (playlists/segments/keys/ABR/DRM/offline правила).
> - `kithara-stream::Stream` инкапсулирует driver loop (`tokio::select!`), cancellation via drop и command-plane контракт (на текущем этапе `SeekBytes` может быть `SeekNotSupported`).
> - “Stop” как отдельная команда **не является** частью контракта: остановка = **drop** consumer stream/session.

Этот документ — portable reference для автономных агентов, которые реализуют **реально работающий** HLS VOD источник в `crates/kithara-hls` без доступа к legacy-репозиториям.

Цель: добиться “basic playback” для VOD:
- stream **выдаёт байты сегментов** (не только плейлисты),
- **закрывается** после последнего сегмента (EOS),
- детерминированные тесты подтверждают, что **все сегменты** выбранного варианта были запрошены,
- DRM **учтён архитектурно** (есть место в пайплайне + интерфейс), но “basic playback” не должен блокироваться на реализацию decrypt.

> Важно: этот документ **не меняет публичный контракт**. Он описывает, как дописать внутренние модули (`driver/fetch/playlist/keys/events/abr`) так, чтобы оно заработало.

Связанные документы:
- `docs/constraints.md` (EOF/backpressure, offline, ABR cache-hit rules, identity)
- `docs/porting/source-driver-reference.md` (общая модель driver loop, cancellation via drop)
- `docs/porting/drm-reference.md` (когда дойдём до decrypt)
- `docs/porting/abr-reference.md` (ABR decision/apply, позже)
- `docs/kanban-kithara-hls.md` (board и чекбоксы)

---

## 0) Минимальный “definition of done” для VOD basic playback

Считаем, что HLS VOD basic playback реализован, если выполняются тесты (локальная фикстура, без внешней сети):

1) **VOD completes and stream closes**
- driver прогрессирует по media playlist до конца,
- data stream закрывается (`Stream::next()` возвращает `None`) в разумный таймаут.

2) **Fetches all segments of selected variant**
- fixture считает запросы по каждому `/seg/...` пути,
- для выбранного варианта каждый сегмент был запрошен минимум один раз.

3) **Manual variant selection works**
- в manual режиме выбранный variant выдаёт байты, которые по детерминированному префиксу принадлежат этому варианту (см. раздел 5).

4) **DRM не блокер**
- если playlist содержит `EXT-X-KEY METHOD=AES-128`, но decrypt ещё не реализован:
  - поведение должно быть **явно определено**:
    - либо сразу fatal `Unimplemented` (строго и детерминированно),
    - либо “пропуск decrypt” допускается *только* если мы гарантируем, что fixture для basic playback **не включает** DRM.
- архитектура должна позволять добавить decrypt в пайплайн без переписывания driver loop (см. раздел 3.3).

---

## 1) Архитектура: какие модули и за что отвечают (в рамках `kithara-hls`)

Нормативное разбиение ответственности (названия могут отличаться, смысл должен совпасть):

- `playlist`:
  - fetch + parse master playlist,
  - resolve variant media playlist URL,
  - fetch + parse media playlist,
  - URL resolution rules (с учётом `base_url` override, если есть в контракте).

- `driver` (VOD orchestration loop):
  - выбирает variant (manual/ABR initial),
  - делает segment loop: init (если нужно) → сегменты,
  - пишет bytes в data-plane канал (bounded),
  - завершает EOS,
  - прекращает работу при drop receiver (cancellation via drop).

- `fetch`:
  - “resource fetcher”: cache-first, network fallback (если не offline),
  - выдаёт `Bytes` + метаданные (скачано из сети или из кэша),
  - не знает про ABR/playlist semantics.

- `keys` (может быть заглушкой на первом этапе basic playback, но интерфейс должен быть живым):
  - fetch key (headers/query),
  - apply `key_processor_cb`,
  - cache processed key.

- `events`:
  - best-effort телеметрия (`SegmentStart`, `VariantApplied`, `EndOfStream`, `FatalError`),
  - события не должны быть единственным механизмом управления.

- `abr`:
  - не обязателен для basic playback (manual selection достаточно),
  - но initial selection и “hooks” для будущих решений должны быть предусмотрены (см. раздел 4.3).

---

## 2) Контракт потока байтов (data-plane) и EOS

Важно про слои ответственности:
- `kithara-stream` отвечает за жизненный цикл loop/отмену/команды и выдачу async byte stream.
- `kithara-hls` отвечает за “что именно является медиа-байтами” и в каком порядке (init/segments), а также за offline/DRM/ABR правила.

### 2.1 Что выдаёт stream
Для выбранного варианта stream выдаёт:
- init segment bytes (если формат/плейлист содержит init) — **до** первого media segment,
- затем media segment bytes 0..N последовательно.

Важно:
- stream **не должен** выдавать “сырые плейлисты” как часть байтовой ленты медиа (если это не было частью исходного контракта). Плейлисты — это orchestration data, не media bytes.

### 2.2 EOS
Для VOD:
- после последнего сегмента stream **закрывается** (канал закрыт, `next()` → `None`).

Fatal error:
- stream выдаёт `Err(...)` (best-effort, один раз),
- затем закрывается.

Cancellation via drop:
- если consumer drop’нул stream/session:
  - orchestration loop прекращает работу (stop = drop),
  - `kithara-hls` прекращает запросы/фетчинг и завершает работу без hang.

---

## 3) VOD driver loop: обязательная последовательность шагов

Ниже — минимальная схема, которая должна быть реализована.

### 3.1 Fetch + parse master
1) Fetch master playlist:
- cache-first (если cache включён),
- offline_mode=true: cache miss => fatal `OfflineMiss`.

2) Parse master playlist:
- извлечь `variant_streams`,
- выбрать initial variant index:
  - manual selector (если задан),
  - иначе `abr_initial_variant_index` (если задан),
  - иначе 0.

### 3.2 Fetch + parse media playlist
1) Resolve media playlist URL:
- относительно master URL, либо с `base_url` override — *как зафиксировано контрактом*.
- (если контракт пока не формализован) — добавить тесты на resolution.

2) Fetch media playlist:
- cache-first,
- offline_mode=true: cache miss => fatal.

3) Parse media playlist:
- получить список сегментов в порядке воспроизведения,
- определить init segment (если есть),
- определить encryption state (EXT-X-KEY) — но для basic playback можно использовать fixture без key.

### 3.3 Segment loop (core)
Для каждого segment `i` выбранного варианта:

1) (Опционально) init segment:
- если в playlist указан init segment и он ещё не был отправлен для текущего “периода” (initial или после switch):
  - fetch init bytes (cache-first),
  - отправить в data-plane до первого media segment.

2) Fetch segment bytes:
- cache-first; при miss:
  - offline_mode=true: fatal OfflineMiss,
  - иначе `kithara-net` (stream/get_bytes) и записать в cache (если включён).

Примечание:
- В актуальной архитектуре `offline_mode` приходит сверху как параметр `kithara-stream::StreamParams`, но enforcement (cache-first и фатальность miss) — ответственность `kithara-hls::Source`.

3) DRM hook (не блокер на basic playback):
- если segment зашифрован (METHOD=AES-128):
  - если decrypt реализован: decrypt и выдавать plaintext,
  - если decrypt **не реализован**:
    - либо fixture для basic playback не включает DRM (рекомендовано),
    - либо немедленно вернуть `HlsError::Unimplemented` (или эквивалент) *детерминированно*.

4) Emit bytes:
- отправить bytes в data-plane bounded channel.

После последнего сегмента:
- закрыть data-plane (drop sender).

---

## 4) Variant selection и ABR: что нужно для basic playback

### 4.1 Manual selection — must-have
Manual selection должен позволять:
- на старте выбрать variant (селектор/индекс),
- получить в потоке bytes, принадлежащие именно этому варианту (в тесте это проверяется префиксами).

### 4.2 Auto selection (ABR) — не обязателен для basic playback
Для basic playback достаточно:
- если нет manual override — стартуем на variant 0 (или `abr_initial_variant_index` если он часть контракта).

### 4.3 ABR hooks — чтобы DRM/ABR не потребовали переписывания
Чтобы позже добавить ABR без переписывания VOD loop:
- решение ABR применяется **на границе сегмента** (перед выбором следующего сегмента),
- driver должен хранить:
  - `current_variant`,
  - `current_segment_index` (в рамках VOD),
  - флаг “нужно ли вставить init при смене variant”.

События:
- если есть события, важно различать `VariantDecision` vs `VariantApplied` (applied — строгий).

---

## 5) Как строить детерминированные фикстуры для basic playback

Рекомендация: сделать mock fixture режим (как в legacy), где:
- master: `/master.m3u8`
- media playlists: `/v{variant}.m3u8`
- segments: `/seg/v{variant}_{i}.bin`
- (опционально) init: `/init/v{variant}.bin`

### 5.1 Payload prefixes
Каждый segment должен начинаться с детерминированного префикса:

- `b"V{variant}-SEG-{i}:" + ...`

Тогда тест может:
- прочитать первые N байт,
- утверждать, что он читает сегменты нужного варианта.

### 5.2 Request counters
Фикстура должна считать:
- количество запросов `/master.m3u8`,
- количество запросов `/v{variant}.m3u8`,
- количество запросов каждого сегмента `/seg/v{variant}_{i}.bin`.

Это критично для теста “fetches all segments”.

### 5.3 Без DRM в basic fixture (рекомендовано)
Чтобы DRM “не был блокером”, базовая fixture для basic playback должна быть **plaintext**.
DRM тесты (AES-128) добавляются отдельным набором позже, используя тот же сервер.

---

## 6) Минимальная матрица тестов (must-have) для `kithara-hls` basic playback

Ниже тесты, которые должны появиться в `crates/kithara-hls/tests/` или `src/...` (как принято в крейте).
Все тесты без внешней сети (локальная фикстура).

### 6.1 `hls_vod_completes_and_stream_closes`
Arrange:
- fixture: variant_count=2, segments_per_variant=12, segment_delay=0,
- manual startup variant=0 (для детерминизма).

Act:
- открыть session,
- drain stream до `None` (с таймаутом).

Assert:
- channel closed (stream ended),
- `/master.m3u8` fetched >= 1,
- `/v0.m3u8` fetched >= 1,
- каждый `/seg/v0_{i}.bin` fetched >= 1.

### 6.2 `hls_manual_variant_outputs_only_selected_variant_prefixes`
Arrange:
- fixture: variant_count=4, segments_per_variant>=3,
- manual selection variant_idx ∈ {0,1,2,3}.

Act:
- read first K bytes (или K “chunk starts”, если есть events; но не полагаться на ordering телеметрии),
- достаточно прочитать bytes, чтобы захватить префиксы нескольких сегментов.

Assert:
- все видимые префиксы соответствуют выбранному variant.

### 6.3 `hls_base_url_override_remaps_segments` (если base_url часть контракта)
Arrange:
- fixture remaps segment paths под другой prefix (например `/a/seg/...`),
- без base_url — 404,
- с base_url override — OK.

Act/Assert:
- без override — воспроизведение падает детерминированно,
- с override — basic playback проходит (хотя бы первые сегменты, или до EOS).

### 6.4 `hls_drop_cancels_driver_and_stops_requests`
Arrange:
- fixture segment_delay небольшой (чтобы driver не успел всё скачать мгновенно),
- открыть session, прочитать 1 chunk/сегмент.

Act:
- drop stream/session.

Assert:
- через небольшой таймаут request counters для “дальних сегментов” остаются 0 (или не растут),
- driver не висит (если доступен join handle в test-only; если нет — косвенно через counters + отсутствие фоновых запросов).

### 6.5 `hls_offline_miss_is_fatal` (если offline_mode есть в контракте)
Arrange:
- offline_mode=true,
- cache пуст,
- fixture сервер доступен (или недоступен — не важно, сеть использоваться не должна).

Act:
- открыть session и попытаться читать.

Assert:
- получаем `OfflineMiss` (или эквивалент),
- stream закрывается.

---

## 7) “DRM учтён, но не блокер”: конкретные требования к реализации на этапе basic playback

Чтобы DRM потом не сломал базовую архитектуру:

1) В segment loop должен быть явный “encryption hook”:
- определить, есть ли active key для сегмента,
- если есть — путь через `KeyManager` и decrypt function,
- если нет — passthrough.

2) KeyManager/keys модуль может оставаться “неиспользуемым” в basic fixture, но:
- должен быть интегрируемым без изменения интерфейсов driver/fetch,
- настройка headers/query/processor должна быть частью `HlsOptions` (если она уже есть).

3) Ошибка “DRM encountered but not implemented” должна быть:
- детерминированной,
- типизированной,
- и приводить к корректному завершению stream.

---

## 8) Приоритеты реализации (чтобы быстро “оживить” kithara-hls)

Рекомендуемый порядок работ для агента (внутри `crates/kithara-hls`):

1) Сделать end-to-end VOD segment loop, используя fixture plaintext.
2) Добавить тест `hls_vod_completes_and_stream_closes` + request counters.
3) Добавить manual selection test (prefix-based).
4) Добавить cancellation via drop test.
5) Только после этого — base_url tests (если пока ломает path resolution).
6) После того, как basic playback стабилен — добавить DRM decrypt tests и реализацию по `docs/porting/drm-reference.md`.

---

## 9) Anti-patterns (что НЕ делать)

- Не “симулировать работу” только загрузкой плейлистов.
- Не возвращать `Ok(0)` в `Read`-bridge пока данные “просто не пришли”.
- Не делать сеть/оркестрацию в decode thread.
- Не полагаться на события как на строгий механизм управления.
- Не добавлять feature flag для DRM: DRM обязателен (просто basic fixture может его не задевать).

---

## 10) Checklist для ревью basic playback

Считаем PR годным для basic playback, если:

- Реально читаются **segment bytes**, а не только playlists.
- Stream закрывается после последнего сегмента (EOS) и это проверяется тестом.
- Тест подтверждает, что **все** сегменты выбранного варианта были запрошены.
- Manual selection детерминированно выбирает вариант (prefix test).
- Cancellation via drop работает.
- DRM “не мешает” (plaintext fixture) и архитектурный hook присутствует.

---