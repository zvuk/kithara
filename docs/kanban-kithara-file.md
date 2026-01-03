# Kanban — `kithara-file`

> Доп. артефакты для портирования legacy-идей (агенты не имеют доступа к другим репо):
> - `docs/porting/source-driver-reference.md` (portable: driver loop semantics, cancellation via drop, offline miss semantics)
> - `docs/porting/net-reference.md` (portable: timeout/retry contract, range semantics)
>
> Reference spec (portable): `docs/porting/source-driver-reference.md`

Этот документ содержит задачи, относящиеся **только** к сабкрейту `crates/kithara-file`.

Общие инструкции (обязательная преамбула к каждой задаче): см. `kithara/docs/kanban-instructions.md`.

---

## Цель

`kithara-file` — оркестратор progressive ресурсов (HTTP mp3 и т.п.) с “player-grade” семантикой.

Актуальная архитектура:

- `kithara-file` управляет **логическими ресурсами**, а не “байтовой лентой”:
  - открывает ресурсы через `kithara-assets` (persistent disk assets store),
  - наполняет большие ресурсы через `kithara-storage::StreamingResource` (HTTP Range → `write_at`) и обеспечивает возможность чтения во время наполнения (`wait_range`),
  - кэширует маленькие объекты (например, метаданные) через `kithara-storage::AtomicResource` (whole-object `read`/`write` с temp→rename).
- загрузка данных из сети — через `kithara-net` (включая Range requests),
- sync consumption для декодера — через `kithara-io`:
  - `Read+Seek` поверх `StreamingResource` (блокировка через `wait_range`, без “ложного EOF”),
- корректный `Read`/EOF (см. `docs/constraints.md`),
- `Seek` обязателен (плеер может перематывать в любой момент):
  - реализуется через range-политику и наполнение `StreamingResource` нужными диапазонами.

Важно: “stop” как отдельная команда **не является** частью контракта. Остановка = **drop session** + cancellation token.

---

## Инварианты (обязательные)

- `Read::read() -> Ok(0)` возвращается **только** при истинном EOS (см. `docs/constraints.md`).
- Seek должен быть **абсолютным**, best-effort; поведение вне доступного диапазона — **явно** определено и тестируется.
- **Cancellation via drop / token** обязательна:
  - если consumer прекратил чтение и drop’нул session, все ожидания `wait_range` и фоновые загрузки должны завершиться быстро и детерминированно (без зависаний).
- Offline режим:
  - `offline_mode=true` + cache miss => **fatal** (никаких попыток “всё равно сходить в сеть”).
- Ошибки должны быть типизированы; различать recoverable и fatal в терминах публичного API.
- Тесты: детерминированные, без внешней сети (только локальная фикстура/сервер).

---

## Публичный контракт (актуально)

- `kithara-file` предоставляет session API, который:
  - возвращает sync `Read+Seek` (через `kithara-io`) поверх ресурса,
  - поддерживает `Seek` во время проигрывания (best-effort, но обязателен),
  - использует `kithara-assets` как persistent disk assets store,
  - использует `kithara-storage` для реализации ресурсов (atomic vs streaming).
- Ошибки разделены:
  - ошибки сети (`kithara-net`),
  - ошибки assets/storage (диск/ресурс),
  - offline miss (fatal при `offline_mode=true`).
