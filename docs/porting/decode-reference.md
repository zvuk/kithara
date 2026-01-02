# Decode reference spec (portable) — decal-style layering, responsibilities, invariants, tests

Этот документ — **самодостаточный reference** для реализации/рефакторинга `kithara-decode` (и связанного bridge в `kithara-io`) агентами, которые **не имеют доступа** к репозиториям `decal` и `stream-download-*`.

Цель: получить многослойную, тестируемую архитектуру декодирования, совместимую с ограничениями `kithara`:
- async networking / resource orchestration снаружи,
- **sync decoding** (Symphonia) внутри отдельного worker thread,
- корректная семантика `Read`/EOF/backpressure,
- чёткие границы ответственности: `Source` → `Engine` → `Decoder` → `Pipeline`.

Связанные документы:
- `AGENTS.md` (границы изменений, workspace-first deps, TDD, imports)
- `docs/constraints.md` (особенно EOF/backpressure, “async net + sync decode”, ошибки)
- `docs/kanban-kithara-decode.md`
- `docs/kanban-kithara-io.md` (bridge async→sync)

---

## 0) Нормативные правила (обязательные)

1) **Sync decoding**: Symphonia требует `Read + Seek` синхронно. Сеть/оркестрация не должна выполняться в decode thread.
2) **EOF семантика**: `Read::read() -> Ok(0)` означает **истинный EOF**. Нельзя возвращать `Ok(0)` пока поток “просто ждёт данных”. Это критично (Symphonia/rodio прекращают чтение).
3) **Backpressure обязательна**: буферы должны быть bounded; writer должен блокироваться/дросселироваться, а не наращивать память бесконечно.
4) **No unwrap/expect** в прод-коде: ошибки типизированы и несут контекст.
5) **Одна задача — один крейт**: рефактор в `kithara-decode` не должен “заодно” править соседние крейты (кроме workspace-first deps в корневом `Cargo.toml`, если без этого никак).

---

## 1) Слои и их ответственность (decal-style)

### 1.1 `Source` / `MediaSource` layer (I/O abstraction)

Назначение: абстрагировать “откуда читаем байты” так, чтобы decode слой:
- не знал про HLS/HTTP/кэш,
- имел sync `Read + Seek`,
- имел hint для формат-пробинга (по расширению), если он известен.

**Контракт (portable):**
- `reader() -> Box<dyn Read + Seek + Send + Sync>`
- `file_ext() -> Option<&str>` (пример: `"mp3"`, `"m4a"`, `"aac"`, `"ts"`)

Примечания:
- `file_ext()` — это hint, не гарантия.
- `reader()` создаёт новый reader, либо возвращает доступ к уже созданному reader — главное, чтобы `Decoder` мог владеть reader’ом на время декодирования.
- Источник должен быть **seekable**. Если он не seekable, контракт должен это явно фиксировать (но для `kithara` seek — важен; если нужен non-seekable режим, это отдельная задача).

**Критическая связь с `kithara-io`:**
- Для HLS/progressive HTTP часто используется async delivery bytes → sync `Read+Seek` bridge.
- Bridge должен реализовывать `Read` так, чтобы **никогда не возвращать `Ok(0)` до истинного EOF**.

---

### 1.2 `Engine` layer (low-level Symphonia glue)

Назначение: полностью инкапсулировать Symphonia:
- probing формата,
- выбор аудио-трека,
- декодирование packet → audio buffer,
- преобразование sample format (в `T`),
- seek/reset,
- извлечение spec (sample_rate/channels),
- обработка Symphonia ошибок.

**Что Engine должен уметь:**
- `open(source)` / `ensure_open`:
  - создать `MediaSourceStream`,
  - probe format reader,
  - выбрать трек,
  - инициализировать decoder.
- `next_packet()` / `decode_next()`:
  - возвращать PCM chunk (raw или уже converted).
- `seek(time)`:
  - выполнить seek на уровне format reader,
  - при необходимости re-init decoder/track state.
- `reset_if_needed()`:
  - при смене кодека/дисконтиниуити (или после seek, если Symphonia так требует).

**Что Engine НЕ должен делать:**
- не управлять очередями/каналами/backpressure,
- не держать async API,
- не решать “когда декодировать” в смысле orchestration (это ответственность выше).

---

### 1.3 `Decoder<T>` layer (state machine + contract)

Назначение: сделать “плеер-грейд” state machine вокруг Engine:
- единый публичный интерфейс `next() -> Result<Option<PcmChunk<T>>, DecodeError>`,
- корректное завершение (EOS),
- обработка recoverable/fatal ошибок,
- управление командами (`Seek`, потенциально `Stop`, `Reset`),
- **контракт инвариантов** `PcmChunk<T>` и `DecodeError`.

**Типичный контракт:**
- `Ok(Some(chunk))` — следующий кусок PCM.
- `Ok(None)` — **EOS** (stream завершён естественно).
- `Err(e)` — ошибка (обычно fatal для данной сессии).

**Рекомендация по ошибкам:**
- отделять “EOS” от ошибок:
  - либо через `Ok(None)`,
  - либо через `Err(EndOfStream)` (хуже для ergonomic API).
- `seek`:
  - “best effort absolute seek” по `Duration`,
  - должен быть **без deadlock/hang**,
  - после seek следующий `next()` отдаёт данные с новой позиции (насколько возможно).

---

### 1.4 `Pipeline` layer (async front-end + bounded queue + worker thread)

Назначение: безопасно соединить:
- sync `Decoder<T>` (который читает sync `Read+Seek`),
- async потребителя (например приложение, которое читает PCM chunks async),
- bounded queue и control-plane команды.

**Состав:**
- worker thread:
  - владеет `Decoder<T>`,
  - циклически читает `next()` и пушит `PcmChunk<T>` в bounded queue,
  - принимает команды (seek/stop).
- bounded queue:
  - ограничивает память (например по количеству `PcmChunk`),
  - writer блокируется, если очередь заполнена.
- command channel:
  - отдельный канал команд, чтобы consumer мог вызвать `seek`/`stop`,
  - команды должны обрабатываться без дедлоков (например при заполненной очереди).

**Ключевой invariant:**
- pipeline не должен порождать “ложный EOF” ни на уровне `Read`, ни на уровне `next_chunk`.

---

## 2) API surfaces (portable, ориентир)

Этот раздел не требует точного соответствия имён — главное, чтобы ответственность совпадала.

### 2.1 Public types

- `PcmSpec { sample_rate: u32, channels: u16 }`
- `PcmChunk<T> { spec: PcmSpec, pcm: Vec<T> }`
  - invariant: `pcm.len() % channels == 0` (frame aligned)
- `DecodeCommand`:
  - минимум `Seek(Duration)`
  - опционально `Stop`, `Flush`, `Reset`
- `DecodeError`:
  - `Io`, `Symphonia`, `NoAudioTrack`, `SeekError`, `CodecResetRequired` и т.п.

### 2.2 Public objects

- `Decoder<T>::new(source, settings) -> Result<Decoder<T>, DecodeError>`
- `Decoder<T>::next() -> Result<Option<PcmChunk<T>>, DecodeError>`
- `Decoder<T>::handle_command(cmd) -> Result<(), DecodeError>`

- `AudioStream<T>` / `Pipeline<T>`:
  - `new(source, settings, buffer_len) -> Result<Self, DecodeError>`
  - `next_chunk().await -> Result<Option<PcmChunk<T>>, DecodeError>`
  - `commands() -> Sender<DecodeCommand>`

---

## 3) Инварианты и “грабли” (то, что ломает плеер)

### 3.1 `Read::read()` и ложный EOF

Симптом:
- Symphonia/rodio внезапно завершают воспроизведение “в середине”, особенно на HLS.

Причина:
- bridge или reader вернул `Ok(0)` при “пока нет данных”.

Нормативно:
- `Ok(0)` допускается только когда:
  - известно, что upstream завершён (EOS),
  - и все буферы исчерпаны.

Если данных нет, но они будут:
- `read()` должен **блокироваться** (на decode thread это допустимо),
- либо возвращать `WouldBlock`/`Interrupted` только если consumer правильно это обрабатывает (обычно Symphonia не ожидает would-block).

### 3.2 Backpressure и deadlocks

Типичный deadlock:
- worker пытается отправить chunk в заполненную очередь и блокируется,
- одновременно consumer отправил `Seek`, но worker не может прочитать команду, т.к. заблокирован на отправке.

Решения (portable):
- разделить data-plane и control-plane;
- при блокирующей очереди предусмотреть:
  - приоритет обработки команд,
  - или “select”/poll, если структура каналов позволяет,
  - или неблокирующий push с периодической проверкой команд.

Ключ:
- тестировать, что `seek` не зависает при заполненной очереди.

### 3.3 Chunk sizing

Слишком большие chunk’и:
- повышают latency seek/stop,
- увеличивают память и задержки.

Слишком маленькие chunk’и:
- overhead на каналах/alloc,
- ухудшение производительности.

Portable guideline:
- размер chunk в пределах десятков миллисекунд аудио (например 20–100ms), но это может быть параметром.

---

## 4) Тест-план (portable, детерминированный)

Тесты делятся на:
- unit tests для типов/инвариантов,
- unit/prop tests для state machine,
- integration-ish tests для pipeline (без внешней сети).

### 4.1 Unit: `PcmChunk` invariants

1) `pcm_chunk_frame_alignment_ok`
- given: `channels=2`, `pcm.len()` кратен 2 → ok.
2) `pcm_chunk_frame_alignment_err`
- given: `channels=2`, `pcm.len()` некратен 2 → error.

### 4.2 Unit: `Decoder` EOS semantics

`decoder_returns_none_on_end_of_stream`
- используйте тестовый in-memory источник (fixture), который завершится.
- assert: decoder eventually returns `Ok(None)`.

Важно:
- не должно быть “вечного ожидания” на EOS.

### 4.3 Unit: `seek_no_deadlock`

`decoder_seek_does_not_deadlock`
- arrange: decoder с небольшим источником
- act: несколько раз `handle_command(Seek(...))` + `next()`
- assert: тест завершается быстро и детерминированно.

Если Symphonia seek для конкретного формата нестабилен, используйте формат/fixture, где seek поддерживается (или ограничьте контракт “best effort”, но **не допускайте hang**).

### 4.4 Pipeline: bounded queue semantics

`pipeline_respects_backpressure`
- arrange: pipeline с маленькой очередью (например 1–2 chunk’а)
- act:
  - consumer намеренно не читает некоторое время,
  - затем читает,
- assert:
  - producer не “убегает” по памяти (это сложно измерить напрямую),
  - но можно проверять косвенно:
    - количество произведённых сообщений не превышает capacity,
    - worker thread не падает,
    - seek/stop остаётся работоспособным.

`pipeline_seek_works_when_queue_full`
- arrange: заполняем очередь (не читаем).
- act: отправляем `Seek`.
- assert:
  - команда применяется (например через event/side-effect),
  - после начала чтения consumer получает данные с новой позиции,
  - тест не висит.

### 4.5 Bridge test (если тестируется в decode, либо отдельно в `kithara-io`)

`bridge_never_returns_ok0_before_eos`
- arrange: bridge, который получает bytes пачками с задержкой/сигналом.
- act: выполнить `Read::read()` из отдельного потока/таска.
- assert:
  - до EOS `read()` либо блокируется, либо возвращает >0,
  - `Ok(0)` случается только после явного EOS.

---

## 5) “Definition of done” для портинга decal-style layering

Изменение считается близким к “как задумано”, если:

1) Есть ясные границы:
- `types` (данные, ошибки, трейты),
- `symphonia_glue` (всё специфичное для Symphonia),
- `engine` (низкий уровень),
- `decoder` (state machine),
- `pipeline` (async front-end + worker + queue).

2) Тестами зафиксированы:
- EOS semantics (`Ok(None)`/эквивалент),
- отсутствие deadlock на seek,
- базовые backpressure сценарии.

3) Нигде не нарушен контракт `Read::read() == Ok(0) только при EOF` (см. `docs/constraints.md`).

---

## 6) Частые ошибки при “упрощении” архитектуры (анти-паттерны)

- Делать `Decoder` async и тащить tokio внутрь Symphonia слоя.
- Пытаться “стримить HLS” прямо из decode thread (сеть/кэш/оркестрация внутри `read()`).
- Возвращать `Ok(0)` как “нет данных”.
- Писать события (events) как “strict ordering” сигнал без документирования applied semantics.
- Добавлять новые зависимости для мелочей, не проверив workspace deps.

---

## 7) Checklist для агента (выполнять по шагам)

1) Прочитать `docs/constraints.md` (EOF/backpressure) и board `docs/kanban-kithara-decode.md`.
2) Зафиксировать контракты типов (`PcmChunk`, `DecodeError`, `DecodeCommand`) тестами.
3) Разделить/укрепить границы `Engine` vs `Decoder`.
4) Укрепить pipeline:
   - bounded queue,
   - command channel,
   - отсутствие deadlock (тест).
5) Если затрагивается bridge семантика `Read`, добавить отдельные тесты (в соответствующем крейте).
6) `cargo fmt`, `cargo test -p kithara-decode`, `cargo clippy -p kithara-decode`.

---