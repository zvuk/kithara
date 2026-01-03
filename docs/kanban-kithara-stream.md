# Kanban — `kithara-stream`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-stream`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-stream` — общий слой **оркестрации** для источников байтов (`kithara-file`, `kithara-hls` и будущих), который:

- инкапсулирует driver loop на базе `tokio::select!`,
- объединяет data-plane (поток байтов) и control-plane (команды),
- обеспечивает **cancellation via drop** (stop = drop consumer stream/session),
- выдаёт наружу **async byte stream** для последующей передачи в `kithara-io`/decoder,
- фиксирует контракт “seek пока может быть not supported” (через `SeekNotSupported`),
- **не** тащит в себя ответственность сети, HLS оркестрации, кэша и файлового layout.

---

## Инварианты (обязательные)

- **Stop-команды нет**: остановка driver loop происходит только через drop consumer stream/session.
- Драйвер **не зависит** от наличия handle:
  - если все handle’ы/command senders уничтожены, поток данных продолжает течь до EOF или drop consumer’а.
- Контракт EOF:
  - завершение byte stream (`None`) означает истинный EOS;
  - нельзя “закрывать” поток из-за отсутствия handle’а или из-за “idle” состояния.
- Backpressure:
  - внутренние каналы bounded (ограниченный буфер);
  - writer/driver дросселируется при медленном consumer’е (без бесконечного накопления памяти).
- Команды:
  - текущая команда: `SeekBytes(u64)`;
  - если source не поддерживает seek — возвращается `SeekNotSupported` (контракт фиксирован тестами).
- Ошибки:
  - `kithara-stream` использует **типизированную** ошибку `StreamError<E>` (generic по ошибке источника),
  - не прячет ошибки источника в `Box<dyn Error>` в публичном контракте.
- Никаких `unwrap/expect` в прод-коде.

---

## Контракт и ответственность (границы)

`kithara-stream` отвечает только за:
- orchestration loop, cancellation, command dispatch,
- выдачу `Stream<Item = Result<Bytes, StreamError<E>>>`.

`kithara-stream` **не отвечает** за:
- сеть/HTTP (это `kithara-net`),
- persistent cache (это `kithara-cache`),
- HLS orchestration (это `kithara-hls`),
- byte-stream → `Read+Seek` bridge (это `kithara-io`),
- decoding (это `kithara-decode`).

---

## Публичный контракт (актуально)

Ожидаемый public surface (именования могут быть минимальными, но смысл фиксируем тестами):

- `trait Source`:
  - `type Error: Error + Send + Sync + 'static`
  - `type Control: Send + 'static` (может быть `()`)
  - `open(params) -> Result<SourceStream<Control, Error>, StreamError<Error>>`
  - `seek_bytes(pos) -> Result<(), StreamError<Error>>` (по умолчанию `SeekNotSupported`)
  - `supports_seek() -> bool`

- `struct Stream<S: Source>`:
  - создаёт orchestration loop и возвращает async byte stream,
  - принимает команды через handle (best-effort),
  - stop = drop consumer stream.

- `StreamParams`:
  - `offline_mode: bool` присутствует как общий флаг для источников (реальное enforcement — ответственность `Source` реализации).

---

## `kithara-stream` — MUST HAVE (контракт + тесты)

- [ ] Контракт “stop = drop”:
  - drop consumer stream => loop завершает работу быстро и детерминированно + tests
- [ ] Контракт “handle не обязателен”:
  - если все handle’ы drop’нуты, stream продолжает выдавать байты до EOF + tests
- [ ] Команда `SeekBytes`:
  - если source поддерживает seek: после seek поток переоткрывается/переключается на новую позицию + tests (детерминированная фикстура)
  - если source не поддерживает seek: возвращается `SeekNotSupported` + tests
- [ ] Типизированные ошибки:
  - `StreamError<E>` корректно прокидывает `E` без boxing + tests
- [ ] Backpressure:
  - bounded channels, без неограниченного роста памяти + tests (как минимум smoke: small buffer + slow consumer)

---

## `kithara-stream` — Архитектурные уточнения (после MUST HAVE)

- [ ] Сообщения data/control:
  - если нужен multiplex: `Message<M> { Data(Bytes), Control(M) }`
  - определить, что именно отдаётся наружу (только bytes или bytes+control) и зафиксировать тестами
- [ ] Командный контракт:
  - зафиксировать best-effort semantics для отправки команд:
    - если stream уже завершён → `ChannelClosed` допустим
- [ ] Документация crate-level:
  - кратко описать responsibilities/invariants/stop semantics/seek contract
- [ ] `cargo clippy -p kithara-stream` без предупреждений под workspace lints

---

## Sanity check (после каждого чекбокса)

- [ ] отметить выполненный пункт в этом файле (`[ ]` → `[x]`)
- [ ] `cargo fmt`
- [ ] `cargo test -p kithara-stream`
- [ ] если `cargo` предлагает auto-fix — выполнить ровно предложенную команду, затем снова `cargo fmt` и `cargo test -p kithara-stream`
