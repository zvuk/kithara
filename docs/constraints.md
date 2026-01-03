# Kithara: constraints & lessons learned

Этот документ фиксирует “конституцию” проекта: ограничения, принципы и ошибки, которые мы уже ловили. Цель — не повторять эти ошибки при разработке `kithara`.

## 1) Что такое `kithara` (границы)

`kithara` — это **networking + resource orchestration + decoding** библиотека, но **не полноценный плеер**:
- даёт возможность получить PCM (через слой декодирования),
- умеет работать с progressive HTTP (mp3 и т.п.) и HLS VOD,
- имеет обязательный persistent disk assets store (для offline),
- построена вокруг *ресурсной* модели данных: внутри `kithara` мы работаем не с “байтовой лентой”, а с **логическими ресурсами**, адресуемыми ключами.

Не входит в область ответственности:
- UI, плейлисты, управление устройствами вывода, микшер (это задача хост-приложения),
- HLS Live (пока не требуется, но дизайн не должен блокировать возможность добавить позже),
- “стоп-команда” как обязательный механизм остановки: stop = drop session.

### 1.1) Роли слоёв (нормативно)

- `kithara-net`: HTTP клиент (timeouts/retries/headers/range).
- `kithara-storage`: примитивы хранения **одного** ресурса:
  - `AtomicResource`: маленькие файлы (playlist, keys, metadata) — whole-object `read`/`write` с atomic replace (temp → rename),
  - `StreamingResource`: большие файлы/сегменты — random-access `write_at/read_at` + `wait_range` для ожидания доступности диапазонов.
- `kithara-assets`: менеджер **дерева ресурсов на диске**:
  - хранит всё под корнем кэша,
  - открывает ресурсы как `Atomic` или `Streaming` (в зависимости от контекста вызова),
  - ключи задаются внешними слоями (assets не “придумывает” пути).
- `kithara-io`: мост async→sync для декодера: предоставляет sync `Read+Seek` поверх `StreamingResource` через `wait_range` (без “ложного EOF”).
- `kithara-file` / `kithara-hls`: orchestration: какие ресурсы качать и как наполнять `StreamingResource` (range requests), как кэшировать маленькие объекты через `AtomicResource`.

## 2) Архитектурная причина “почему раньше было больно”

HLS по природе — это **дерево ресурсов**:
- master/media playlists,
- init segments,
- media segments,
- keys/DRM,
- variant switching (ABR/manual),
- discontinuity.

Попытки “натянуть” HLS на bytes-only модель (“один файл/один поток байт”) приводят к:
- потере семантики границ,
- сложному seek,
- трудно-тестируемым переходам variant/codec,
- нежелательным компромиссам.

**Вывод (нормативно):** в `kithara` HLS остаётся “ресурсным” внутри. “Поток байт” — это лишь представление поверх ресурсов (обычно через `kithara-io`), но публичный контракт для декодера — это sync `Read+Seek`, а не `Stream<Item=Bytes>`.

## 3) Критичный контракт `Read`: `Ok(0)` == EOF

Мы уже ловили баги, где `Read::read()` возвращал `Ok(0)` не как true EOF, а как “пока нет данных”.
Для подавляющего большинства потребителей (включая Symphonia) `Ok(0)` означает:
- конец файла/потока,
- дальнейшее чтение прекращается.

**Правило (нормативно):**
- `Read::read()` возвращает `Ok(0)` только при доказанном End-Of-Stream.
- Если данных ещё нет (а ресурс ещё будет дописан), `Read::read()` должен **блокироваться** на decode thread, а ожидание реализуется через `StreamingResource::wait_range` (с cancellation).

## 4) Async networking + sync decoding: разделение потоков

Сетевой слой должен быть async (обычно `tokio`), но Symphonia требует sync `Read + Seek`.
Нельзя выполнять сеть/оркестрацию в audio-thread.

**Правило (нормативно):**
- есть отдельный decode thread (или worker), который синхронно читает байты и декодирует,
- async задачи скачивают данные и **пишут** их в `StreamingResource` через `write_at` (в том числе out-of-order по range),
- sync чтение для декодера реализуется через `kithara-io`, который блокируется на `wait_range` (и никогда не выдаёт “ложный EOF”).

## 5) Events: “decision” ≠ “applied”

Out-of-band события (телеметрия) не гарантируют упорядоченность относительно данных.
Мы уже сталкивались с тем, что:
- `VariantChanged` мог означать решение ABR, но не факт применения в декодировании.

**Правило:**
- если событие используется для строгой логики (например, reinit декодера при смене codec/variant), оно должно быть связано с фактическим применением и иметь ясную семантику.
- телеметрия допускает out-of-band, но на неё нельзя строить строгие инварианты.

## 6) ABR: cache hits не должны “улучшать” throughput

Если обновлять estimator по cache-hit, ABR начинает думать, что сеть быстрая, и выбирает variant, которого нет в кэше. Это ломает offline.

**Правило:**
- throughput/bandwidth estimator обновляется только по реальным network downloads,
- cache-hit не влияет на ABR.

ABR политика должна быть параметризуемой (аналогично прежнему `stream-download-hls`).

## 7) Assets store: persistent, tree-friendly, crash-safe

Persistent disk storage обязателен и должен переживать перезапуски. HLS требует tree-like layout.

Требования (нормативно):
- persistent disk assets store (не “только в памяти”),
- кэшируем всё, что нужно для offline playback: playlists, segments, keys,
- хранение — это дерево файлов под корнем кэша:
  - путь формируется как `<cache_root>/<asset_root>/<rel_path>`,
  - `asset_root` и `rel_path` задаются внешними слоями (например: `<asset_hash>/<path-from-playlist>`),
  - assets слой обязан предотвращать path traversal (`..`, абсолютные пути).
- запись маленьких объектов (playlist, keys, metadata/index) должна быть crash-safe:
  - temp file → rename (атомарная замена) через `AtomicResource::write`.
- запись больших объектов/сегментов поддерживает immediate-read semantics:
  - `StreamingResource::write_at` + `wait_range` (без ожидания “скачать весь файл целиком”).
- eviction и lease:
  - лимит по общему размеру кэша (bytes),
  - LRU по asset,
  - активный asset pinned/leased (не подлежит eviction),
  - eviction не должен удалять ресурсы, которые находятся в процессе заполнения (in-progress).

## 8) Идентификация: `asset_id` без query, `resource_hash` с query

Причина: query может быть динамическим (особенно для ключей/DRM), но трек нужно считать одним.

**Правило:**
- `asset_id` строится из canonical URL **без query/fragment**,
- `resource_hash` (для конкретного ресурса: segment/playlist/key) строится из canonical URL **с query**, без fragment.

## 9) DRM keys: кэшируем processed keys

Для offline важно сохранить ключи в виде, пригодном к расшифровке:
- ключ может приходить “wrapped” и раскрываться через `key_processor_cb`.

**Правило:**
- в кэш сохраняются ключи после обработки `key_processor_cb` (processed keys),
- не логировать ключи/секреты (допустимы только хеш/фингерпринт).

## 10) Тесты (TDD): что тестировать и чего избегать

Мы уже видели, что некоторые тесты легко “перезелёные” или привязаны к случайному поведению.

**Правило (нормативно):**
- TDD-first: тесты пишем до реализации,
- тесты детерминированные, без внешней сети,
- распределять тесты по слоям ответственности:
  - `kithara-storage`:
    - `AtomicResource`: atomic replace semantics, read/write small objects,
    - `StreamingResource`: `wait_range` (с cancellation), `write_at/read_at`, EOF semantics после `commit(Some(final_len))`.
  - `kithara-assets`:
    - маппинг `<root>/<asset_root>/<rel_path>`, защита от traversal,
    - глобальный индекс `_index/state.json` как best-effort (можно удалить/потерять и восстановить по FS).
  - `kithara-io` / `kithara-decode`:
    - `Read`/EOF семантика (`Ok(0)==EOF`),
    - seek safety (произвольный seek во время проигрывания),
    - отсутствие deadlock при ожидании данных (`wait_range`).
  - `kithara-file` / `kithara-hls`:
    - orchestration: какие range запросы делаем, как наполняем ресурсы, cancellation via token/drop,
    - offline поведение поверх persistent assets store.
- избегать тестов, которые строят строгие инварианты на “нуле сетевых запросов” без явного контракта (revalidation/ttl). 

## 11) Комментарии и документация

Длинные комментарии в коде быстро устаревают.

**Правило:**
- комментарии в коде: максимум однострочные, по необходимости,
- архитектурные пояснения и контракты — в README соответствующего сабкрейта или в `docs/`.

## 12) Workspace-first dependencies

Все версии зависимостей задаются в workspace root `Cargo.toml` (`[workspace.dependencies]`),
а в сабкрейтах подключаются через `{ workspace = true }` (включая dev/build).

См. также `AGENTS.md`.