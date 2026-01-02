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

## `kithara-file` MVP

- [x] open session + `asset_id` from URL + tests
- [x] stream bytes from net + tests
- [x] cache-through write (optional) + offline reopen test
- [x] seek via Range (not cached) + tests

> Важно: текущее состояние может формально иметь “stream bytes from net”, но если это не гарантирует полную загрузку ресурса и корректное завершение (EOS), задачи ниже считаются обязательными, чтобы получить “реальную” реализацию.

---

## `kithara-file` — Реальная реализация driver loop + cancellation via drop + offline miss semantics (tests)

Reference: `docs/porting/source-driver-reference.md`

- [ ] Driver streams full content and closes (EOS):
  - local fixture serves deterministic bytes (можно chunked)
  - `session.stream()` выдаёт все байты в правильном порядке и **закрывается** после конца
  - тест: `file_stream_downloads_all_bytes_and_closes`
- [ ] Cancellation via drop (Stop не обязателен):
  - consumer читает 1 chunk и drop’ает stream/session
  - driver должен завершиться детерминированно (без зависаний)
  - тест: `file_receiver_drop_cancels_driver`
- [ ] Offline replay from cache works (если cache включён контрактом):
  - прогон online заполняет cache
  - повторный прогон в `offline_mode=true` читает **только из кэша** и выдаёт те же байты
  - тест: `file_offline_replays_from_cache`
- [ ] Offline miss is fatal (если `offline_mode` предусмотрен опциями/контрактом):
  - `offline_mode=true` и cache пуст
  - попытка чтения приводит к `OfflineMiss` (или эквивалент) и корректному завершению
  - тест: `file_offline_miss_is_fatal`

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

## `kithara-file` — Refactor (driver/session separation; keep public contract)

- [x] Ensure `FileDriver` loop, session handle, and options are in separate modules when the crate grows (no behavior change) + keep tests green
- [x] Isolate “range/seek policy” code into a dedicated module (no behavior change) + keep tests green
- [x] Add crate-level docs: seek contract + offline/cache interaction (short)

---

## Sanity check (после ужесточения fmt+clippy)

- [x] Remove new clippy warnings (e.g. `unused_imports`) introduced by stricter workspace lints
- [x] Run `cargo fmt` and ensure no formatting diffs remain
- [x] Run `cargo test -p kithara-file`
- [x] Run `cargo clippy -p kithara-file` and make it clean (no warnings/errors) under workspace lints

---