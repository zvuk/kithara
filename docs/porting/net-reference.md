# Net reference spec (portable) — layering, timeout/retry matrix, stateless design, tests

Этот документ — **самодостаточный reference** для реализации/проверки `kithara-net` агентами, которые **не имеют доступа** к legacy-коду (`stream-download-rs`).
Цель — получить сетевой клиент, который:
- компонуется слоями (generics-first / decorator pattern),
- **stateless** по умолчанию (кэширование — не в net),
- имеет понятный и тестируемый контракт по **timeouts** и **retries**,
- детерминированно тестируется локальными фикстурами (без внешней сети),
- даёт верхним слоям (`kithara-hls`, `kithara-file`) минимальную и стабильную поверхность.

Связанные документы:
- `AGENTS.md` (границы изменений, зависимости workspace-first, TDD, imports)
- `docs/constraints.md` (EOF/backpressure, offline, ABR правила)
- `docs/kanban-kithara-net.md`
- `docs/porting/legacy-reference.md`

---

## 0) Нормативные правила (обязательные)

1) **Stateless by default**  
   `kithara-net` не хранит persistent cache, не реализует HLS-специфичные “оптимизации”, не знает про asset_id/ресурсное дерево.  
   Допускается:
   - connection pooling / keep-alive на уровне HTTP клиента,
   - конфигурация таймаутов/ретраев,
   - метаданные для отладки/ошибок.

2) **Retries: only before body streaming (v1)**  
   Ретраи допускаются только до того, как потребитель начал получать body stream (до “first byte” в data-plane).  
   Mid-stream retry запрещён в v1 (сложно корректно, ломает семантику range/stream).

3) **HTTP статус != успех**  
   4xx/5xx должны маппиться в типизированную ошибку. Никакого “тихого успеха”.

4) **Timeout тестируемый и детерминированный**  
   Тесты таймаутов не должны зависеть от “sleep на глаз”. Нужна фикстура/сервер, который гарантированно держит соединение/не отвечает, и малые таймауты с запасом.

5) **No unwrap/expect** в прод-коде. Ошибки типизированы и содержат контекст: URL, стадию операции (connect/request/headers/body), статус.

6) Dependency hygiene  
   Новые зависимости добавлять только если нет в workspace. Сначала — в `kithara/Cargo.toml` `[workspace.dependencies]`, затем `{ workspace = true }`.

---

## 1) Слои (decorator architecture) — portable model

Рекомендуемый состав (имена могут отличаться, ответственность — нет):

- `Net` trait (core surface)
- `ReqwestNet` (или другой HTTP backend) — базовый слой
- `TimeoutNet<N>` — декоратор таймаутов
- `RetryNet<N, P>` — декоратор ретраев + `RetryPolicy`
- `NetBuilder` / `create_default_client()` — сборка дефолтной композиции

### 1.1 `Net` trait: минимальная поверхность

Операции, которые обычно нужны верхним слоям:

- `get_bytes(url) -> Bytes`  
  Удобно для маленьких объектов (playlists, keys), но важно: оно “буферизует всё”.

- `stream(url, headers) -> ByteStream`  
  Для progressive download / segments.

- `get_range(url, range, headers) -> ByteStream`  
  Для seek/read-ahead сценариев.

`ByteStream`:
- асинхронный поток чанков `Bytes` + `NetError` на элемент.

Headers:
- опциональные headers для каждого запроса (HLS key requests и т.п.).

Range:
- минимально: start/end (inclusive/exclusive — фиксируй контрактом и тестом).
- важно: корректно формировать `Range` header, и корректно обрабатывать ответы сервера (206/200/416).

---

## 2) Error model (portable)

`NetError` должен позволять различать:
- **retriable** vs **fatal** (для retry decorator),
- тип ошибки (status / timeout / connect / body read / invalid response),
- контекст: URL, stage.

Рекомендуемые категории:

- `Status { url, status, body_snippet? }`
- `Timeout { url, stage }`
- `Connect { url, source }`
- `Body { url, source }` (ошибка чтения body stream)
- `InvalidResponse { url, msg }`
- `Cancelled` (если предусмотрено)

`stage` (очень полезно для тестов и дебага):
- `Request` (connect + request send)
- `Headers` (ожидание ответа/заголовков)
- `Body` (в процессе чтения body)

---

## 3) Timeout semantics (portable contract)

### 3.1 Рекомендованный контракт таймаутов (v1)

- `get_bytes(url)`:
  - таймаут на **всю операцию** (request+headers+body), потому что иначе она может висеть бесконечно.

- `stream(url, headers)`:
  - таймаут на **request+headers** (получение ответа и заголовков),
  - **без** таймаута на body stream (или с отдельным “idle timeout” — но это отдельный контракт и отдельные тесты).

- `get_range(url, range, headers)`:
  - как `stream`: таймаут request+headers, body stream без таймаута (или явный отдельный).

Почему так:
- body stream по определению может быть долгим; таймаут на body без idle semantics будет ломать “медленную, но живую” сеть.
- если нужен idle timeout, его нужно формализовать (например “нет новых чанков X секунд”) и тестировать отдельно.

### 3.2 Timeout matrix (операции × стадия)

Нормативная матрица v1:

| Operation        | Request stage | Headers stage | Body stage |
|-----------------|---------------|---------------|------------|
| `get_bytes`     | timeout       | timeout       | timeout    |
| `stream`        | timeout       | timeout       | no-timeout |
| `get_range`     | timeout       | timeout       | no-timeout |

Если проект решит ввести body idle-timeout, добавь отдельную колонку/правило и тесты.

---

## 4) Retry semantics (portable contract)

### 4.1 Основное правило v1
- Retry выполняется **только** если body stream ещё не был “отдан наружу” или “потреблён”.
- Если ошибка произошла во время чтения body (mid-stream), то это:
  - либо fatal,
  - либо сигнал верхнему уровню (например HLS driver) перезапросить ресурс целиком/по range — но это уже orchestration, не net retry.

### 4.2 Что считается retriable

Рекомендуемая классификация (фиксируется тестами):

**Retriable:**
- network/connect errors (DNS, connect refused, reset, TLS handshake) — как `Connect`/`Request` errors
- `Timeout` на стадии request/headers
- HTTP статусы:
  - 5xx (500–599)
  - 429 (Too Many Requests)
  - 408 (Request Timeout) — решение должно быть тестом зафиксировано (допускается как retriable)

**Not retriable (fatal):**
- 4xx кроме 408/429 (например 400, 401, 403, 404)
- body read errors (stage=Body) — в v1 без mid-stream retry

### 4.3 Retry policy (portable)

`RetryPolicy` (параметры):
- `max_retries: u32`
- `base_delay: Duration` (например 100ms)
- `max_delay: Duration` (например 5s)
- `jitter: bool` (опционально; для тестов лучше детерминированный режим)
- `retry_timeout: Duration` (опционально: общий лимит на суммарное время ретраев)

Backoff:
- экспоненциальный: `delay = min(max_delay, base_delay * 2^attempt)`
- jitter:
  - для тестов: либо выключаем jitter, либо фиксируем seed/детерминированную функцию.

### 4.4 Retry matrix (ошибка × стадия)

| Error type / Stage              | Retriable? | Notes |
|--------------------------------|------------|-------|
| Connect/Request error          | yes        | до body |
| Timeout(Request/Headers)       | yes        | до body |
| HTTP 5xx                       | yes        | до body |
| HTTP 429                       | yes        | до body |
| HTTP 408                       | decide by test | обычно yes |
| HTTP 4xx (кроме 408/429)       | no         | fatal |
| Body read error (mid-stream)   | no (v1)    | без mid-stream retry |

---

## 5) Range semantics (portable)

`get_range(url, range)` должен:
- сформировать корректный `Range` header:
  - например `bytes=start-end` (если inclusive end) или `bytes=start-` (open-ended).
- корректно интерпретировать ответы:
  - `206 Partial Content` — ожидаемый для range,
  - `200 OK` — некоторые сервера игнорируют range; это может быть fatal или accepted в зависимости от контракта (нужно зафиксировать тестом/доком),
  - `416 Range Not Satisfiable` — fatal (обычно), но важно выдать типизированную ошибку.

Headers:
- range requests должны поддерживать дополнительные headers (например auth).

---

## 6) Тестовая фикстура (portable)

Тесты должны быть без внешней сети, поэтому нужен локальный HTTP сервер/фикстура, умеющий:

1) **Streaming endpoint**:
- возвращает chunked body,
- может задерживать отправку заголовков,
- может “обрывать” соединение mid-stream (для body errors).

2) **Range endpoint**:
- отдаёт байты из фиксированного буфера,
- поддерживает `Range` header и возвращает 206,
- умеет возвращать 416 по некорректному диапазону.

3) **Fail-then-succeed endpoint** (для retry):
- первые N запросов: заданный status (например 500),
- затем 200 с корректным body,
- считает количество запросов.

4) **Header/query validation endpoint** (важно для HLS keys):
- возвращает 200 только если присутствуют нужные headers/query, иначе 403/400.

---

## 7) Обязательные тесты (portable test plan)

Ниже тесты, которые агент может реализовать/проверить в `kithara-net`.

### 7.1 Base layer status mapping
- `http_404_is_error_not_success`
- `http_500_is_error_not_success`

### 7.2 Retry classification (status)
- `retry_on_5xx`
- `retry_on_429`
- `no_retry_on_404`
- `retry_on_408_is_defined_by_test` (либо yes, либо no — но фиксируем)

### 7.3 Retry succeeds after N failures
- `retry_fail_then_success_get_bytes`
- `retry_fail_then_success_stream_headers` (ошибка/статус до body)

Assert:
- количество запросов == N+1
- итоговый результат корректен

### 7.4 No mid-stream retry (v1)
- `no_retry_on_body_error_stream`
Сценарий:
- сервер отдаёт headers и первый chunk, затем рвёт соединение
Assert:
- stream завершается ошибкой
- количество попыток не превышает 1 (или фиксированно 1)

### 7.5 Timeout semantics
- `timeout_applies_to_headers_for_stream`
Сценарий:
- endpoint держит соединение и не отправляет headers
- timeout маленький
Assert:
- получаем `NetError::Timeout(stage=Headers)` (или эквивалент)

- `get_bytes_times_out_on_body`
Сценарий:
- endpoint отправляет headers, но body никогда не завершает / зависает
Assert:
- timeout с stage=Body или общий timeout

### 7.6 Range semantics
- `range_returns_exact_bytes`
- `range_416_is_error`
- (опционально) `range_200_is_handled_as_contract` (либо error, либо accepted)

---

## 8) Definition of done (как понять, что “как задумано”)

Изменение считается “в духе legacy”, если:

- `kithara-net` компонуется слоями `Base → Timeout → Retry` через generics/decorators.
- Retry policy детерминированна в тестах (без флейка).
- Ясно документировано, какие таймауты применяются на каких стадиях (матрица).
- Mid-stream retry отсутствует и это зафиксировано тестом.
- Клиент stateless: кэширование/ресурсное дерево — не в net.
- Ошибки типизированы и содержат stage+url.

---

## 9) Checklist для агента (выполнять по шагам)

1) Прочитать `docs/kanban-kithara-net.md` и этот документ.
2) Зафиксировать контракт timeout’ов тестами (матрица из раздела 3.2).
3) Зафиксировать retry classification тестами (раздел 4.2).
4) Добавить тест “no mid-stream retry” (раздел 7.4).
5) Проверить range semantics тестами.
6) `cargo fmt`, `cargo test -p kithara-net`, `cargo clippy -p kithara-net`.
