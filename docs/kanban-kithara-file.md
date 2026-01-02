# Kanban — `kithara-file`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-file`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-file` — источник байтов для progressive ресурсов (HTTP mp3 и т.п.) с “player-grade” семантикой:

- async загрузка (через `kithara-net`),
- sync consumption через bridge (через `kithara-io`, если используется в интеграции),
- корректный `Read`/EOF,
- best-effort `Seek` (обычно через HTTP Range),
- детерминированное завершение/stop,
- интеграция с кэшем (через `kithara-cache`) по необходимости для offline/reopen.

---

## Инварианты (обязательные)

- `Read::read() -> Ok(0)` возвращается **только** при истинном EOS (см. `docs/constraints.md`).
- Seek должен быть **абсолютным**, best-effort; поведение вне доступного диапазона — **явно** определено и тестируется.
- Любая отмена (drop/Stop) должна завершать сессию **без зависаний**.
- Ошибки должны быть типизированы; различать recoverable и fatal в терминах публичного API (как минимум: “можно продолжать” vs “сессия завершена”).
- Тесты: детерминированные, без внешней сети (только локальная фикстура/сервер).

---

## `kithara-file` MVP

- [x] open session + `asset_id` from URL + tests
- [x] stream bytes from net + tests
- [x] cache-through write (optional) + offline reopen test
- [x] seek via Range (not cached) + tests

---

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