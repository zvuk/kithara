# Kanban — `kithara-file`

> Доп. артефакты для портирования legacy-идей (агенты не имеют доступа к другим репо):
> - `docs/porting/source-driver-reference.md` (portable: driver loop semantics, cancellation via drop, offline miss semantics)
> - `docs/porting/net-reference.md` (portable: timeout/retry contract, range semantics)
>
> Reference spec (portable): `docs/porting/source-driver-reference.md`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-file`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-file` — источник байтов для progressive ресурсов (HTTP mp3 и т.п.) с “player-grade” семантикой.

Актуальная архитектура:

- **оркестрация driver loop вынесена в `kithara-stream`**:
  - `kithara-file` реализует `kithara-stream::Source` (конкретный источник: cache-first / network / offline);
  - `kithara-stream::Stream` инкапсулирует цикл `tokio::select!` (data-plane + control-plane), cancellation via drop и выдачу async byte stream.
- загрузка байтов (через `kithara-net`),
- интеграция с кэшем (через `kithara-cache`) для offline/reopen,
- sync consumption через bridge (через `kithara-io`, если используется в интеграции),
- корректный `Read`/EOF (см. `docs/constraints.md`),
- best-effort `Seek` (обычно через HTTP Range) — контракт фиксируется тестами и реализуется поэтапно.

Важно: “stop” как отдельная команда **не является** частью контракта. Остановка = **drop** consumer stream/session.

---

## Инварианты (обязательные)

- `Read::read() -> Ok(0)` возвращается **только** при истинном EOS (см. `docs/constraints.md`).
- Seek должен быть **абсолютным**, best-effort; поведение вне доступного диапазона — **явно** определено и тестируется.
- **Cancellation via drop** обязательна:
  - если consumer прекратил чтение и drop’нул stream/session, driver loop должен завершиться быстро и детерминированно (без зависаний).
- Offline режим:
  - `offline_mode=true` + cache miss => **fatal** (никаких попыток “всё равно сходить в сеть”).
- Ошибки должны быть типизированы; различать recoverable и fatal в терминах публичного API.
- Тесты: детерминированные, без внешней сети (только локальная фикстура/сервер).

---

## Публичный контракт (актуально)

- `kithara-file` предоставляет session/stream API поверх `kithara-stream`.
- Ошибки разделены:
  - `SourceError` (ошибки источника: сеть/кэш/offline miss),
  - `DriverError` (обёртка: `StreamError<SourceError>` + ошибки сессии/настроек).

---

## `kithara-file` — Port legacy “read/seek correctness” scenarios (tests)

- [ ] Seek roundtrip correctness:
  - read first N bytes, seek to 0, read again, bytes match reference (no corruption, no deadlock) + tests
- [ ] Seek variants:
  - `SeekFrom::Start`, `SeekFrom::Current`, `SeekFrom::End` produce expected slices on a known static resource + tests
- [ ] Cancel behavior (drop-driven):
  - after reading some bytes, **drop** session/stream and ensure termination promptly without hanging + tests
- [ ] (Prereq if missing) Define precise seek contract for `kithara-file` session:
  - what happens when seeking beyond cached/buffered range
  - what errors are returned (typed) + tests

---
