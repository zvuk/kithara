# Kanban — `kithara-decode`

> Доп. артефакты для портирования decal-style многослойности (агенты не имеют доступа к другим репо):
> - `docs/porting/decode-reference.md` (слои Source/Engine/Decoder/Pipeline, инварианты EOF/backpressure, тест-план)

## Важное правило (нормативно): публичный контракт `kithara-decode` не меняем
- Нельзя менять/переименовывать/удалять публично экспортируемые типы/функции/трейты.
- Нельзя добавлять новые публичные API элементы, если это расширяет контракт.
- `docs/porting/decode-reference.md` — это **внутреннее** руководство по архитектуре/границам и тестам. Оно не является требованием менять публичные экспорты.

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-decode`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

Reference spec (portable): `docs/porting/decode-reference.md`

---

## Цель

`kithara-decode` — слой декодирования аудио (Symphonia) с “player-grade” семантикой:

- sync декодирование в отдельном decode worker (thread / `spawn_blocking`),
- управление через команды (как минимум `Seek(Duration)`),
- выдача PCM чанков через bounded очередь (backpressure),
- корректная семантика EOS и fatal ошибок,
- generic дизайн (по мотивам `decal`): `Decoder<T>` и контракт `Source`.

---

## Инварианты (обязательные)

- Декодирование **не** выполняет сеть/оркестрацию. Источники байт (file/hls/net) работают вне decode thread.
- Нельзя допускать раннего EOF в sync чтении:
  - `Read::read() -> Ok(0)` означает EOS, поэтому bridge/источник должен возвращать `Ok(0)` только при истинном завершении (см. `docs/constraints.md`).
- PCM контракт:
  - `PcmChunk<T>` интерлеaved,
  - frame-aligned: `len % channels == 0`,
  - `channels > 0`, `sample_rate > 0`.
- Backpressure обязательна:
  - producer ждёт, когда очередь заполнена,
  - consumer не должен получать бесконечный рост памяти.
- Ошибки:
  - fatal ошибка должна приводить к детерминированному завершению стрима (одна ошибка + завершение, либо иной явно зафиксированный контракт),
  - никаких `unwrap()`/`expect()` в прод-коде без веской причины.
- Тесты:
  - детерминированные,
  - без внешней сети,
  - без флейковых таймингов.

---

## Компоненты (контракт и ответственность) — напоминание

Ожидаемое разбиение (названия могут отличаться, но ответственность должна быть ясной):

- `Source`: синхронный источник байт для Symphonia (MediaSource + `file_ext()` hint).
- `DecoderSettings`: настройки поведения декодера (минимальный baseline, например gapless toggle).
- `Decoder<T>`: низкоуровневая state machine:
  - probe с hint/extension,
  - выбор трека,
  - декод пакетов → PCM,
  - `seek(Duration)` best-effort + reset.
- `PcmSpec`, `PcmChunk<T>`: публичные PCM типы и инварианты.
- `DecodeCommand`: команды управления (минимум `Seek(Duration)`; опционально Stop/Pause если закреплено контрактом).
- `AudioStream<T>`: high-level async consumer API (bounded queue semantics) + управление командами.
- Worker loop: отдельный поток/задача, который читает из `AudioSource<T>`/`Source`, декодирует и пушит чанки в очередь.

---

## `kithara-decode` — Port legacy audio pipeline scenarios (tests)

- [ ] PCM invariants:
  - emitted `PcmChunk<T>` is interleaved and frame-aligned (`len % channels == 0`)
  - `channels > 0`, `sample_rate > 0` + tests
- [ ] Full drain closes stream:
  - decode a finite HTTP MP3-like asset and ensure PCM stream terminates (EOS) + tests
- [ ] HLS VOD decode drains sequentially without repeats (variant-independent):
  - run with manual variant selection (or AUTO) and ensure PCM drain ends + tests
- [ ] Codec switch reinitializes decoder and PCM continues:
  - fixture switches codec between segments/variants
  - decoder resets/reopens and continues producing PCM + tests
- [ ] Seek scrubbing:
  - multiple forward/backward `Seek(Duration)` commands do not deadlock and result in continued PCM production + tests
- [ ] Ordered boundaries (best-effort):
  - optionally expose best-effort “init boundary” / “segment boundary” events to validate ordering + tests

---

## Sanity check (после ужесточения fmt+clippy)

- [ ] Remove new clippy warnings/errors introduced by stricter workspace lints
- [ ] Run `cargo fmt` and ensure no formatting diffs remain
- [ ] Run `cargo test -p kithara-decode`
- [ ] Run `cargo clippy -p kithara-decode` and make it clean (no warnings/errors) under workspace lints
