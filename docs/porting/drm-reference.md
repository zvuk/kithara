# DRM reference spec (portable) — HLS AES-128, key processing, caching, offline rules, fixtures

Этот документ — **самодостаточный reference** для реализации DRM в `kithara-hls` агентами, которые **не имеют доступа** к legacy-коду (`stream-download-hls`).

Цель: обязательный DRM слой (без feature flags), который:
- корректно расшифровывает **HLS AES-128** сегменты (CBC),
- применяет `key_processor_cb` для “wrapped keys”,
- кэширует **processed keys** (после `key_processor_cb`) для offline,
- обеспечивает строгую политику ошибок (offline miss => fatal),
- детерминированно тестируется локальными фикстурами.

Связанные документы:
- `docs/constraints.md` (особенно “DRM keys: кэшируем processed keys”, “offline miss fatal”, “asset_id/resource_hash”)
- `docs/kanban-kithara-hls.md`
- `docs/porting/legacy-reference.md`
- `docs/porting/abr-reference.md` (для общей структуры “decision vs applied”, хотя DRM независим)

---

## 0) Нормативные правила (обязательные)

1) **DRM обязателен**: никакого `cfg(feature = "...")` для baseline DRM в `kithara-hls`.
2) Ключи:
   - `key_processor_cb` может “раскрывать” wrapped keys.
   - В кэш пишется **processed key** (результат `key_processor_cb`), а не raw key.
3) Offline:
   - если включён `offline_mode`, то **любой cache miss** по плейлисту/сегменту/init/key = **fatal**.
4) Безопасность:
   - **не логировать** raw key bytes и plaintext сегментов;
   - допустимы: длина, URL без секретов, хеш/фингерпринт (например первые 8 байт SHA-256, но не обязательно).
5) EOF/backpressure:
   - DRM слой не должен ломать контракты `docs/constraints.md` (особенно `Read::read() -> Ok(0) == EOF`).
6) Dependency hygiene:
   - перед добавлением новых зависимостей проверять `kithara/Cargo.toml` `[workspace.dependencies]` (см. `AGENTS.md`).
   - для AES желательно использовать уже имеющееся; если нет — добавить workspace-first и обосновать.

---

## 1) Что такое “DRM” в scope v1

В scope v1 под DRM понимается **HLS AES-128** (EXT-X-KEY METHOD=AES-128) для media segments.

Не в scope v1 (если отдельно не планируется):
- SAMPLE-AES / fMP4 CENC / Widevine/FairPlay/PlayReady.
- сложные KMS протоколы, лицензирование.
- mid-stream retry на уровне net.

---

## 2) Модель HLS encryption (portable)

### 2.1 EXT-X-KEY

Пример:
- `#EXT-X-KEY:METHOD=AES-128,URI="key.bin",IV=0x000102...0f`

Нормативно для v1:
- Поддерживать `METHOD=AES-128`.
- `URI` может быть относительным или абсолютным.
- `IV`:
  - если указан — использовать его;
  - если не указан — см. раздел 2.3 (IV derivation).

### 2.2 AES-128-CBC

- key length: **16 bytes**
- block size: 16 bytes
- mode: CBC
- padding:
  - HLS часто использует PKCS#7 padding, но для transport stream возможны “ровно по блокам”.
  - В тестовых фикстурах лучше делать payload кратным 16, чтобы убрать неоднозначность.
- output: plaintext bytes, которые дальше идут как обычный segment payload.

### 2.3 IV derivation (когда IV отсутствует)

В HLS спецификациях часто используется IV, derived from media sequence number (или segment number) как 128-bit big-endian integer.

Portable правило для v1 (рекомендуемое):
- если playlist key не содержит `IV`, то:
  - `iv = [0; 16]` с записью `sequence_number` (u64) в последние 8 байт big-endian
  - или в последние 4 байта, если используется u32 — но лучше u64.
- Важно: точный алгоритм должен быть **зафиксирован тестом**.

Чтобы сделать тесты проще:
- в большинстве fixture сценариев всегда задавай явный `IV` в playlist.

---

## 3) Key fetching + key processing + caching

### 3.1 Key identity (как кэшировать)

См. `docs/constraints.md`:
- `asset_id`: canonical URL **без query/fragment** (идентичность “трека”)
- `resource_hash`: canonical URL **с query**, без fragment (идентичность “ресурса”)

**Нормативно:**
- для ключей в кэше ключом должен быть `resource_hash` (потому что query может содержать токены и реально менять ключ).
- но “папка”/пространство имён должно быть привязано к `asset_id` (чтобы key cache жил внутри asset tree).

### 3.2 Key request customization

Поддержать два механизма (обычно уже есть в `HlsOptions`):
- `key_query_params: HashMap<String,String>`
- `key_request_headers: HashMap<String,String>`

Правило:
- params/headers применяются **только** к key fetch (не ко всем сегментам), если так зафиксировано в API.
- порядок добавления query params не должен влиять на identity:
  - canonicalization должна упорядочить параметры или использовать `Url` canonicalization, но это может быть сложным.
  - Минимум: тесты должны учитывать текущие правила, чтобы избежать флейка.

### 3.3 Key processing callback

API:
- `key_processor_cb: Fn(raw_key: Bytes, ctx: KeyContext) -> Result<Bytes, HlsError>`

`KeyContext` (примерно):
- `url: Url` (key URL)
- `iv: Option<[u8;16]>` (IV из playlist, если есть)
- (опционально) `asset_id`, `variant`, `segment_index`, но только если нужно.

Правила:
- callback может:
  - “unwrap” ключ (пример: XOR, base64 decode, decrypt wrapper, etc.),
  - валидировать длину,
  - возвращать error (fatal для сессии).
- результат callback — это **processed key**, и именно его надо использовать для decrypt и положить в кэш.

### 3.4 Processed key caching contract

Нормативно:
- `processed_key` сохраняется в persistent cache.
- при последующих запросах ключа:
  - если processed key есть в кэше — используем его, **не делаем network fetch**.
  - throughput estimator (ABR) не должен обновляться на cache-hit key (обычно keys не учитывают в ABR вообще).

Политика кэширования:
- key cache layout должен быть tree-friendly: `hls/keys/...`.
- имя файла не должно раскрывать секреты:
  - не пишем raw key в имя,
  - лучше использовать `resource_hash` (или его hex) как имя файла.

---

## 4) Decrypt pipeline integration (где должен жить код)

Рекомендуемая схема ответственности внутри `kithara-hls`:

- `keys` (KeyManager):
  - fetch raw key (через `kithara-net`),
  - apply query/headers,
  - apply `key_processor_cb`,
  - cache processed key,
  - return processed key bytes.

- `fetch`/`driver`:
  - fetch init/segment bytes (network/cache),
  - для каждого segment:
    - определить encryption state (есть ли текущий key, какой IV),
    - запросить key у KeyManager (async),
    - decrypt segment bytes,
    - отдать plaintext downstream.

- `events`:
  - опционально: `KeyFetchStarted/KeyFetchFinished` (telemetry-only),
  - но **никаких** событий с сырьём ключа.

Где **не** должен быть decrypt:
- не в `kithara-net` (net должен быть stateless и не знать про HLS/DRM).
- не в decode thread (DRM относится к байтам HLS, это оркестрация источника).

---

## 5) Error taxonomy (portable)

Рекомендуемые ошибки (как минимум концептуально):

- `UnsupportedKeyMethod(method)` → fatal
- `KeyFetchFailed(url, net_error)` → fatal (возможно retriable до начала body, но это уже net-layer)
- `KeyInvalidLength(len)` → fatal
- `KeyProcessingFailed(msg)` → fatal
- `DecryptFailed(msg)` → fatal
- `OfflineMiss(resource)` → fatal (в offline_mode)

Важный контракт:
- если ключ не найден/не удалось расшифровать сегмент — **стрим должен завершиться ошибкой** (и downstream должен увидеть “fatal” и корректное окончание).

---

## 6) Offline mode requirements (portable)

Если `offline_mode == true`:
- Любой fetch (playlist/segment/init/key) должен:
  - сначала проверять кэш,
  - если в кэше нет — возвращать `HlsError::OfflineMiss` (или эквивалент) и завершать сессию.

Тесты offline должны проверять минимум:
- при полностью заполненном кэше playback проходит без сети,
- при missing key (processed key отсутствует) — ошибка fatal и **нет попыток** дернуть сеть (если контракт такой),
- при missing segment/init/playlist — аналогично fatal.

---

## 7) Fixture reference (детерминированные тесты без внешней сети)

### 7.1 Требования к test server

Локальный сервер/фикстура должен уметь:
- отдавать master/media playlists,
- отдавать init segment (если нужен),
- отдавать media segments,
- отдавать key endpoint, который:
  - может требовать header/query params,
  - может отдавать “wrapped key” (для проверки processor),
  - считает количество запросов (per-path counter),
- симулировать ошибки (403/404/500) для негативных тестов.

### 7.2 Payload strategy (детерминированные префиксы)

Для проверки decrypt достаточно, чтобы plaintext содержал фиксированный префикс.

Рекомендуемый plaintext сегмента:
- `b"V{variant}-SEG-{i}:" + payload_bytes...`

Затем encrypt AES-128-CBC и сервер отдаёт ciphertext bytes.

Проверка в тесте:
- stream выдаёт plaintext,
- первые N байт совпадают с ожидаемым префиксом.

### 7.3 Key wrapping strategy (для processor test)

Сценарий:
- истинный key `K` (16 bytes).
- сервер отдаёт wrapped key `W`, например:
  - `W = b"WRAP:" + K`
  - или `W = xor(K, 0xAA)`
- `key_processor_cb`:
  - удаляет префикс / выполняет XOR,
  - валидирует длину 16,
  - возвращает processed key `K`.

Важно:
- В кэш должен попасть именно `K`, а не `W`.

---

## 8) Обязательные тесты (portable test plan)

Тесты должны жить в `kithara-hls` и выполняться без внешней сети.

### 8.1 AES-128 decrypt smoke

`aes128_decrypt_smoke_plaintext_prefix`
- arrange:
  - master/media playlist с `EXT-X-KEY METHOD=AES-128 URI=... IV=...`
  - один encrypted segment
- act:
  - открыть сессию, прочитать первые bytes
- assert:
  - plaintext начинается с ожидаемого префикса

### 8.2 Key fetch applies headers + query + processor

`key_fetch_applies_headers_query_and_processor`
- arrange:
  - key endpoint требует header `X-Auth=...` и query `token=...`, иначе 403/400
  - key endpoint отдаёт wrapped key
  - `key_processor_cb` unwrap → 16 bytes
- act:
  - воспроизведение первого сегмента
- assert:
  - plaintext корректен
  - key endpoint был вызван (1 раз)

### 8.3 Processed key cached

`processed_key_cached_not_fetched_per_segment`
- arrange:
  - playlist с несколькими сегментами, использующими один и тот же key URI
- act:
  - прочитать bytes, охватывающие 2+ сегмента
- assert:
  - key endpoint request count <= 1 (или <= 2 если есть гонка/рефетч, но лучше <=1)
  - (если есть контракты revalidation) — фиксируем точное ожидаемое поведение

### 8.4 Offline playback works with processed key cached

`offline_mode_uses_cached_processed_key`
- arrange:
  - прогнать один раз online, чтобы заполнить cache (включая processed key)
  - затем выключить сервер (или запретить сеть) и включить `offline_mode=true`
- act:
  - открыть снова и читать
- assert:
  - playback succeeds до EOS
  - нет сетевых запросов (если это можно проверить детерминированно в фикстуре)

### 8.5 Offline miss is fatal (key)

`offline_mode_missing_key_is_fatal`
- arrange:
  - offline_mode=true
  - сегменты есть в кэше, но processed key отсутствует
- act:
  - открыть и начать чтение
- assert:
  - получаем `OfflineMiss` (или эквивалент) и корректное завершение

---

## 9) Implementation checklist для агента (по шагам)

1) Убедиться, что DRM функциональность **не feature-gated**.
2) KeyManager:
   - query params + headers,
   - key_processor_cb,
   - validation длины processed key (16 bytes),
   - persistent cache write/read processed key.
3) Decrypt:
   - AES-128-CBC decrypt сегментов с IV,
   - error mapping (decrypt fail => fatal).
4) Тестовые фикстуры:
   - encrypted segment + key endpoint,
   - wrapped key + processor.
5) Offline tests:
   - offline success with cached processed key,
   - offline miss fatal.
6) `cargo fmt`, `cargo test -p kithara-hls`, `cargo clippy -p kithara-hls`.

---

## 10) Notes / pitfalls

- Не обновляй ABR throughput по key download (обычно ключи малы и искажают throughput).
- Не делай “best effort decrypt” (типа “если не смогли — отдаём ciphertext”): это ломает декодирование и скрывает ошибки.
- Не делай `unwrap()` в чтении/записи ключей из кэша: ошибки диска должны всплывать как `CacheError`/`HlsError`.
- Не храни “raw key” в кэше: только processed.
- В тестах избегай случайных sleep’ов; используй детерминированный server/fixtures.
