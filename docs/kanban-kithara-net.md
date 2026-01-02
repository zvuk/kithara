# Kanban — `kithara-net`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-net`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-net` — сетевой клиент для загрузки байтов, рассчитанный на использование верхними уровнями (`kithara-file`, `kithara-hls`):
- async I/O (tokio-ориентированный),
- streaming GET (byte stream),
- range GET (для seek),
- поддержка headers (в т.ч. для HLS key requests),
- предсказуемые таймауты и retry (в виде слоёв/декораторов),
- типизированные ошибки и явная классификация retriable vs fatal (как минимум на уровне net).

---

## Компоненты (контракт и ответственность) — напоминание

**Нормативно** (целевое разбиение, если/когда понадобится рефактор):
- `traits`: базовый трейт `Net` и композиция (`NetExt`), минимальная поверхность.
- `types`: алиасы/типы (`ByteStream`, request/response metadata).
- `base` (`ReqwestNet` или эквивалент): “голый” HTTP-клиент с явной обработкой статусов (без “тихого успеха” на 4xx/5xx).
- `timeout` (`TimeoutNet<N>`): декоратор таймаутов (контракт должен быть тестируемым и не флейковым).
- `retry` (`RetryNet<N, P>`): декоратор ретраев с `RetryPolicy`.
- `builder`: сборка “дефолтного” клиента из слоёв.

Важно: `kithara-net` по умолчанию **stateless**. Любые кэши/переиспользование ресурсов — ответственность верхних уровней.

---

## Инварианты (обязательные)

- Никаких `unwrap()`/`expect()` в прод-коде: все ошибки типизированы и несут контекст.
- HTTP статусы 4xx/5xx не должны трактоваться как “успех”.
- Retry:
  - только до начала streaming body (v1: **без** mid-stream retry),
  - классификация retriable фиксируется тестами (например: network errors, 5xx, 429; решение по 408 — через тест).
- Timeout:
  - тесты должны быть детерминированными (без “sleep на глаз”).
- Тесты только с локальной фикстурой/сервером (без внешней сети).
- Не добавлять новые зависимости, если функционал уже покрыт зависимостями workspace или stdlib (см. `AGENTS.md` и `kanban-instructions.md`).

---

## `kithara-net` MVP

- [x] `NetClient` wrapper + options
- [x] stream GET bytes + tests with local server
- [x] range GET + tests
- [x] header support (key requests) + tests

---

## `kithara-net` — HLS key request semantics (tests)

- [x] Headers passthrough for key fetches (client sends required headers; server validates and otherwise fails) + tests
- [x] Query params passthrough for key fetches (client appends required query params; server validates and otherwise fails) + tests
- [x] (Prereq if missing) Add a small local server fixture that can:
  - require specific headers/query params for a path
  - record per-path request counts for assertions

---

## `kithara-net` — Full client (retry/timeout; layered; generics-first)

- [x] Refactor: split `kithara-net` into modules `base/traits/types/retry/timeout/builder` without changing current behavior
- [x] Add `ByteStream` alias and core trait `Net` (+ `NetExt` for composition)
- [x] Implement `ReqwestNet` base layer with explicit status handling (no silent success on 4xx/5xx) + tests
- [x] Remove `expect/unwrap` from prod code: client construction returns `Result` with typed error + tests
- [x] Implement `TimeoutNet<N>` decorator:
  - `get_bytes`: whole-op timeout
  - `stream/get_range`: timeout only for request + response headers
  - deterministic tests (no flaky sleeps)
- [x] Implement `RetryNet<N, P>` decorator with `RetryPolicy`:
  - exponential backoff with cap
  - retry only before body streaming (no mid-stream retry in v1)
  - tests with local fixture: N failures then success
- [x] Define retry classification rules (network errors + HTTP 5xx + 429; decision about 408 фиксируется тестами) + tests per status
- [x] Provide `NetBuilder` / `create_default_client()` that composes base+retry+timeout into a convenient facade
- [x] Ensure existing behavior stays covered: header passthrough + range semantics (tests must remain green after refactor)
- [x] Add short crate-level docs (`crates/kithara-net/README.md`): layering, retry semantics, timeout semantics

---

## `kithara-net` — Refactor (layering-ready; keep public contract)

- [x] Split into `base/traits/types` modules without behavior change; keep tests green
- [x] Centralize error mapping (reqwest/status/timeout) into one place to avoid duplication + keep tests green
- [x] Add crate-level docs: what is considered retriable, what timeout means (contract-level)

---

## `kithara-net` — Port legacy “key cached / not fetched per segment” support (prereq tasks)

- [ ] Expose a way for upper layers (`kithara-hls`) to reuse fetched bytes without re-requesting:
  - define a minimal cache interface or hook point at the HLS layer (NOT inside net)
  - ensure net itself stays stateless by default
- [ ] Add server-side request counting utilities in tests so `kithara-hls` can assert “key requested <= N times” deterministically

---

## Sanity check (после ужесточения fmt+clippy)

- [ ] Remove new clippy warnings (e.g. `unused_imports`) introduced by stricter workspace lints
- [ ] Run `cargo fmt` and ensure no formatting diffs remain
- [ ] Run `cargo test -p kithara-net`
- [ ] Run `cargo clippy -p kithara-net` and make it clean (no warnings/errors) under workspace lints