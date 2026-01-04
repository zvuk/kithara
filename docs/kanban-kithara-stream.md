# Kanban — `kithara-stream`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-stream`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-stream` — вспомогательный слой для **оркестрации byte-stream сценариев**, который может использоваться точечно там, где действительно нужен `Stream<Item = Bytes>` + control-plane.

В актуальной ресурсной архитектуре `kithara` основные источники (`kithara-file`, `kithara-hls`) работают через:

- `kithara-assets`: открытие persistent disk ресурсов по ключам,
- `kithara-storage`: `StreamingResource` (range `write_at/read_at` + `wait_range`) и `AtomicResource` (whole-object `read/write`),
- `kithara-io`: sync `Read+Seek` поверх `StreamingResource` (ожидание через `wait_range`, без “ложного EOF”).

`kithara-stream` остаётся как отдельный крейт, но **не является обязательным ядром проигрывания** и не определяет seek-поведение плеера.

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
  - текущая команда: `SeekBytes(u64)` (best-effort для byte-stream источников),
  - если source не поддерживает seek — возвращается `SeekNotSupported`.
- Ошибки:
  - `kithara-stream` использует **типизированную** ошибку `StreamError<E>` (generic по ошибке источника),
  - не прячет ошибки источника в `Box<dyn Error>` в публичном контракте.
- Никаких `unwrap/expect` в прод-коде.

Примечание: эти инварианты относятся только к byte-stream оркестрации. В плеерном режиме (seek в любой момент) основной контракт реализуется через `StreamingResource` + `kithara-io` (`Read+Seek` + `wait_range`).

---

## Контракт и ответственность (границы)

`kithara-stream` отвечает только за:
- orchestration loop, cancellation, command dispatch,
- выдачу `Stream<Item = Result<Bytes, StreamError<E>>>`.

`kithara-stream` **не отвечает** за:
- сеть/HTTP (это `kithara-net`),
- persistent assets store (это `kithara-assets` + `kithara-storage`),
- HLS orchestration (это `kithara-hls`),
- async resource → sync `Read+Seek` bridge (это `kithara-io`),
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
