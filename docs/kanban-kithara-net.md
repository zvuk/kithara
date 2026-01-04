# Kanban — `kithara-net`

> Доп. артефакты для портирования legacy-идей (агенты не имеют доступа к другим репо):
> - `docs/porting/net-reference.md` (portable: layering, timeout/retry matrix, stateless design, tests)

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-net`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

Reference spec (portable): `docs/porting/net-reference.md`

---

## Цель

`kithara-net` — сетевой клиент для загрузки байтов, рассчитанный на использование верхними уровнями (`kithara-file`, `kithara-hls`):
- async I/O (tokio-ориентированный),
- streaming GET (byte stream),
- range GET (для seek),
- HEAD (для получения metadata: `Content-Length`/`Accept-Ranges`/`Content-Type`),
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

## Tasks

- [x] Implement `HEAD` support:
  - add `head(url, headers) -> Headers` to `Net` trait
  - implement for `HttpClient`
  - propagate through `TimeoutNet` and `RetryNet`
  - add local fixture test that verifies `Content-Length` is returned for a `HEAD` endpoint
