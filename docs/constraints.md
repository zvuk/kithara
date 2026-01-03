# Kithara: constraints & lessons learned

Этот документ фиксирует “конституцию” проекта: ограничения, принципы и ошибки, которые мы уже ловили в попытках интегрировать HLS в bytes-only модели. Цель — не повторять эти ошибки при разработке `kithara`.

## 1) Что такое `kithara` (границы)

`kithara` — это **networking + orchestration + decoding** библиотека, но **не полноценный плеер**:
- даёт возможность получить PCM (через слой декодирования),
- умеет работать с progressive HTTP (mp3 и т.п.) и HLS VOD,
- имеет обязательный persistent disk cache для offline,
- содержит общий orchestration слой `kithara-stream`:
  - источники (`kithara-file`, `kithara-hls`) реализуют `kithara-stream::Source` (ресурсная логика: что качать и в каком порядке),
  - `kithara-stream::Stream` инкапсулирует общий driver loop (`tokio::select!`), cancellation via drop и command contract (на текущем этапе `SeekBytes` может быть `SeekNotSupported`).

Не входит в область ответственности:
- UI, плейлисты, управление устройствами вывода, микшер (это задача хост-приложения),
- HLS Live (пока не требуется, но дизайн не должен блокировать возможность добавить позже),
- “стоп-команда” как обязательный механизм остановки: stop = drop consumer stream/session.

## 2) Архитектурная причина “почему раньше было больно”

HLS по природе — это **дерево ресурсов**:
- master/media playlists,
- init segments,
- media segments,
- keys/DRM,
- variant switching (ABR/manual),
- discontinuity.

Попытки “натянуть” HLS на модель “один файл/один поток байт” приводят к:
- потере семантики границ,
- сложному seek,
- трудно-тестируемым переходам variant/codec,
- нежелательным компромиссам (in-band magic bytes, message-based API поверх bytes-only источника).

**Вывод:** в `kithara` HLS должен оставаться “ресурсным” внутри, а “байтовая лента” может быть только представлением.

Примечание про API:
- “байтовая лента” как публичный контракт остаётся `Stream<Item = Result<Bytes, ...>>`;
- при необходимости внутренний orchestration слой может иметь control-plane сообщения (например `Message<Data/Control>`), но это не означает “in-band magic bytes” и не превращает медиа-поток в “плейлисты+сегменты+команды” для потребителя.

## 3) Критичный контракт `Read`: `Ok(0)` == EOF

Мы уже ловили баги, где `Read::read()` возвращал `Ok(0)` не как true EOF, а как “пока нет данных”.
Для подавляющего большинства потребителей (включая Symphonia/rodio) `Ok(0)` означает:
- конец файла/потока,
- дальнейшее чтение прекращается.

**Правило:**
- `Read::read()` возвращает `Ok(0)` только при доказанном End-Of-Stream.
- Если данных ещё нет (а поток продолжится), мы:
  - либо блокируемся (допустимо на decode thread),
  - либо используем иной механизм (не через `Read`), но не симулируем EOF.

## 4) Async networking + sync decoding: разделение потоков

Сетевой слой должен быть async (обычно `tokio`), но Symphonia требует sync `Read + Seek`.
Нельзя выполнять сеть/оркестрацию в аудио-thread.

**Правило:**
- есть отдельный decode thread (или worker), который синхронно читает байты и декодирует,
- async задачи доставляют данные/ресурсы и управляют кэшем/ресурсной логикой источников (`kithara-file`, `kithara-hls`),
- общий orchestration loop (lifecycle, cancellation via drop, command dispatch) инкапсулирован в `kithara-stream`,
- между async источниками и sync decode — явный “bridge” (канал/буфер) с корректной семантикой EOF и backpressure.

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

## 7) Cache: persistent, tree-friendly, crash-safe

Кэш обязателен и должен переживать перезапуски. HLS требует tree-like layout.

Требования:
- persistent disk cache (не “только в памяти”),
- кэшируем всё, что нужно для offline playback: playlists, segments, keys,
- запись объектов должна быть crash-safe:
  - temp file -> rename (атомарная замена),
  - не полагаться на RAM-only метаданные для `exists/len`,
- eviction:
  - лимит по общему размеру кэша (bytes),
  - LRU по asset/track,
  - перед записью нового контента можно удалить несколько треков,
  - активный трек pinned/leased (не подлежит eviction).

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

**Правило:**
- TDD-first: тесты пишем до реализации,
- тесты детерминированные, без внешней сети,
- избегать тестов, которые жёстко требуют “ноль сетевых запросов” без явного контракта revalidation,
- распределять тесты по слоям ответственности:
  - `kithara-stream`: stop=drop, bounded backpressure, command contract (seek may be not supported), lifecycle/EOS как поведение orchestration loop,
  - `kithara-file` / `kithara-hls`: что именно качаем и в каком порядке (cache-first, offline miss fatal, HLS segment loop, DRM/ABR), корректное закрытие потока на EOS/ошибке,
  - `kithara-io` / `kithara-decode`: `Read`/EOF семантика (`Ok(0)==EOF`), seek safety и отсутствие deadlock,
- ABR тесты делать:
  - либо через deterministic manual switch,
  - либо через controlled network shaping (фикстура/сервер).

## 11) Комментарии и документация

Длинные комментарии в коде быстро устаревают.

**Правило:**
- комментарии в коде: максимум однострочные, по необходимости,
- архитектурные пояснения и контракты — в README соответствующего сабкрейта или в `docs/`.

## 12) Workspace-first dependencies

Все версии зависимостей задаются в workspace root `Cargo.toml` (`[workspace.dependencies]`),
а в сабкрейтах подключаются через `{ workspace = true }` (включая dev/build).

См. также `AGENTS.md`.