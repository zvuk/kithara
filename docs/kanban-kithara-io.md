# Kanban — `kithara-io`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-io`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-io` — bridge-слой между async источниками байт (сеть/HLS/file) и sync потребителями (`Read + Seek`) в decode thread.

Здесь живёт:
- bounded буферизация (по байтам),
- backpressure,
- корректная EOF семантика,
- понятный, тестируемый контракт `Seek` (best-effort или явно “unsupported”).

---

## Инварианты (обязательные)

- **EOF критично:** `Read::read() -> Ok(0)` означает истинный End-Of-Stream. Нельзя возвращать `Ok(0)` пока поток реально не завершён.
- **Backpressure обязательна:** если consumer не читает — producer должен блокироваться/дросселироваться; память не должна расти без ограничений.
- **Ошибки типизированы:** различать recoverable vs fatal на уровне контракта (по крайней мере “можно продолжать” vs “сессия завершена”).
- **Детерминированные тесты:** без внешней сети, без флейковых `sleep`, без зависаний.
- Bridge не должен скрывать “плохие” состояния:
  - seek вне поддерживаемого диапазона должен возвращать явную ошибку,
  - после fatal ошибки — потребитель должен получить ошибку и затем корректно завершиться.

См. детали и причины в `kithara/docs/constraints.md`.

---

## Компоненты и потоки (контракт) — ориентир

Ожидаемая структура (может отличаться по именам, но ответственность должна быть ясной):

- `bridge`: высокоуровневая сборка writer/reader половинок.
- `writer`: принимает чанки байт от async producer’а и пишет в bounded буфер.
- `reader`: реализует `Read` (и возможно `Seek`) поверх bounded буфера.
- `errors`: типизированные ошибки и “конечные состояния” потока (EOS vs fatal).
- (опционально) `metrics`: только если уже есть стандартный механизм телеметрии в workspace; не тянуть новые зависимости ради метрик.

---

## `kithara-io` MVP

- [x] Bounded bridge (writer/reader) + tests for EOF semantics
- [x] Backpressure behavior + tests
- [x] Initial seek contract decision (explicit errors if unsupported) + tests

---

## `kithara-io` — Port legacy backpressure/seek edge cases (tests)

- [x] Backpressure: writer blocks when buffer full; reader unblocks it by draining (bounded by bytes) + tests
- [x] Seek contract: seeking beyond available buffered data returns a typed error (no silent success) + tests
- [x] EOS correctness: `Read::read()` returns `Ok(0)` only after finish/EOS; never earlier + tests

---

## `kithara-io` — Refactor (small modules; keep public contract)

- [x] Split into `bridge`, `reader`, `writer`, `errors` modules when code grows (no behavior change) + keep tests green
- [x] Consolidate synchronization/backpressure primitives into one place to avoid duplicated invariants + keep tests green
- [x] Add crate-level docs: EOF semantics and seek contract (short, normative)

---

## Sanity check (после ужесточения fmt+clippy)

- [x] Remove new clippy warnings/errors introduced by stricter workspace lints
- [x] Run `cargo fmt` and ensure no formatting diffs remain
- [x] Run `cargo test -p kithara-io`
- [x] Run `cargo clippy -p kithara-io` and make it clean (no warnings/errors) under workspace lints