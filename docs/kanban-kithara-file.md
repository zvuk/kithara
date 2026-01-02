# Kanban — `kithara-file`

> Доп. артефакты для портирования legacy-идей (агенты не имеют доступа к другим репо):
> - `docs/porting/source-driver-reference.md` (portable: как должен работать driver loop у источников, cancellation via drop, offline miss semantics)
> - `docs/porting/net-reference.md` (portable: timeout/retry contract, range semantics)
>
> Reference spec (portable): `docs/porting/source-driver-reference.md`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-file`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-file` — источник байтов для progressive ресурсов (HTTP mp3 и т.п.) с “player-grade” семантикой и **реальной** реализацией driver loop (не заглушки):

- async загрузка (через `kithara-net`),
- driver loop внутри `kithara-file` (оркестрация/сеть/кэш) и выдача байтов через async stream,
- sync consumption через bridge (через `kithara-io`, если используется в интеграции),
- корректный `Read`/EOF (см. `docs/constraints.md`),
- best-effort `Seek` (обычно через HTTP Range) — контракт должен быть зафиксирован тестами,
- **cancellation via drop**: “stop” это когда consumer прекратил чтение (drop stream/session); отдельная команда Stop не является обязательной семантикой,
- интеграция с кэшем (через `kithara-cache`) для offline/reopen (если включено в настройках/контракте).

---

## Инварианты (обязательные)

- `Read::read() -> Ok(0)` возвращается **только** при истинном EOS (см. `docs/constraints.md`).
- Seek должен быть **абсолютным**, best-effort; поведение вне доступного диапазона — **явно** определено и тестируется.
- **Cancellation via drop** обязательна:
  - если consumer прекратил чтение и drop’нул stream/session, driver loop должен завершиться быстро и детерминированно (без зависаний).
- Offline режим (если предусмотрен опциями/контрактом):
  - `offline_mode=true` + cache miss => **fatal** (никаких попыток “всё равно сходить в сеть”).
- Ошибки должны быть типизированы; различать recoverable и fatal в терминах публичного API (как минимум: “можно продолжать” vs “сессия завершена”).
- Тесты: детерминированные, без внешней сети (только локальная фикстура/сервер).

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
