# Отчёт: Stabilize seek (zero dead seeks)

## Worktree

- **Имя**: `seek-stabilization`
- **Путь**: `.claude/worktrees/seek-stabilization`
- **Ветка**: `worktree-seek-stabilization`
- **Базовый коммит**: `8f81652e` (Merge branch 'coverage-refactor-v2')

## Коммиты

| Hash | Описание |
|------|----------|
| `6a4886e2` | fix(seek): split flushing into flushing + seek_pending, fix epoch ordering |

## Мотивация

Seek в плеере имел два системных бага, которые приводили к «мёртвому seek» — состоянию,
когда после перемотки `read()` возвращает 0 навсегда, и плеер зависает:

1. **`epoch.store()` вызывался ДО фактического seek** (`source.rs:1148`). Чанки, декодированные
   из старой позиции, получали новый epoch и проходили через `EpochValidator` на стороне
   consumer. Результат — воспроизведение стартовало с мусорных данных или зависало.

2. **`complete_seek()` был единственным сигналом завершения seek**. Если сам seek не удавался
   (decoder.seek + recreate decoder оба failed), pipeline навсегда застревал: `flushing=false`
   (I/O разблокирован), но seek не применён, и worker не знал что нужен retry.

Стресс-тест `stress_seek_abr` допускал до 90% мёртвых seek-ов (`dead_ratio < 0.9`), что
фактически маскировало баг.

## Архитектурное решение

Разделить одноатомный флаг `flushing` на два независимых:

- **`flushing`** — гейтит I/O (`wait_range` возвращает `Interrupted`). Кратковременный:
  устанавливается в `initiate_seek()`, снимается в `complete_seek()` ДО начала seek.
  Нужен чтобы прервать блокирующие чтения в других потоках.

- **`seek_pending`** — гейтит worker retry. Долгоживущий: устанавливается в `initiate_seek()`,
  снимается в `clear_seek_pending(epoch)` ПОСЛЕ успешного seek. Если seek не удался —
  worker видит `is_seek_pending() == true` и повторяет попытку.

Это паттерн GStreamer `FLUSH_START`/`FLUSH_STOP`, но с добавлением второго бита для retry.

## Изменения по файлам

### 1. `crates/kithara-stream/src/timeline.rs` (+78 LOC)

**Что**: Добавлен `seek_pending: Arc<AtomicBool>` в `Timeline`.

**Зачем**: Отделить «seek начат» от «seek применён». `flushing` снимается рано (для I/O),
а `seek_pending` живёт до успешного seek.

**API**:
- `initiate_seek()` — теперь также ставит `seek_pending = true`
- `is_seek_pending() -> bool` — новый метод
- `clear_seek_pending(epoch: u64)` — очищает только если epoch совпадает (защита от stale)

**Тесты** (5 новых):
- `initiate_seek_sets_seek_pending`
- `clear_seek_pending_only_clears_matching_epoch`
- `new_initiate_seek_resets_seek_pending`
- `complete_seek_does_not_clear_seek_pending`
- `is_seek_pending_visible_across_clones`

### 2. `crates/kithara-audio/src/pipeline/source.rs` (+55/-3 LOC)

**Что**: Переработана `apply_pending_seek()` — epoch ordering + bounded retry.

**Зачем**: Исправить два корневых бага.

**Детали**:
- `epoch.store(epoch)` перенесён с позиции ДО seek на позицию ПОСЛЕ `applied == true`.
  Теперь чанки, декодированные до успешного seek, несут старый epoch и отфильтровываются
  `EpochValidator` на стороне consumer.
- `self.shared_stream.set_seek_epoch(epoch)` оставлен ДО seek (HLS variant fence требует).
- `complete_seek(epoch)` оставлен ДО seek (wait_range нужен flushing=false для I/O).
- Возвращаемый тип изменён с `()` на `bool`:
  - `true` — seek применён (или abandon после 3 попыток)
  - `false` — seek не применён, worker должен повторить
- Добавлен `seek_retry_count: u8` — счётчик неудачных попыток (max 3).
- При `applied == true`: `epoch.store()`, `clear_seek_pending()`, reset counter.
- При `applied == false`: increment counter, если >= 3 — abandon (accept epoch, clear pending).
- Добавлена константа `MAX_SEEK_RETRY: u8 = 3`.

**Анализ дублирования с `pending_seek_recover_*`**:
Существующий механизм `pending_seek_recover_target` / `pending_seek_recover_attempts` работает
на ДРУГОМ этапе — когда seek уже применён (`apply_seek_applied` вызван), но первый decode
после seek даёт ошибку. Новый `seek_retry_count` работает когда сам seek не удался.
Механизмы никогда не активны одновременно.

### 3. `crates/kithara-audio/src/pipeline/worker.rs` (+23/-10 LOC)

**Что**: Worker loop теперь проверяет `is_seek_pending()`.

**Зачем**: Если seek не удался и `seek_pending` остался true, worker должен повторить
`apply_pending_seek()` на следующей итерации.

**Детали**:
- `apply_pending_seek_if_flushing()` → `apply_pending_seek_if_needed()`
- Условие: `!is_flushing() && !is_seek_pending()` (вместо только `!is_flushing()`)
- Trait `AudioWorkerSource::apply_pending_seek` — возвращает `bool` вместо `()`
- `#[expect(clippy::cognitive_complexity)]` — обновлён reason

### 4. `tests/tests/kithara_hls/stress_seek_abr.rs` (-17/+5 LOC)

**Что**: Ужесточён допуск dead seeks.

**Зачем**: С retry механизмом все seek-и должны давать audio. Старый допуск 90% маскировал баг.

**Было**: `dead_ratio < 0.9` (допускалось 90% мёртвых seek-ов)
**Стало**: `dead_seeks == 0` (ноль мёртвых seek-ов)

### 5. Удаление redundant `hang_watchdog` timeout overrides (4 файла, -4 LOC)

**Файлы**: `kithara-hls/src/source.rs`, `kithara-storage/src/driver.rs`,
`kithara-stream/src/backend.rs`, `kithara-stream/src/stream.rs`

**Что**: Удалены строки `timeout: Duration::from_secs(N)` из `hang_watchdog!` макросов.

**Зачем**: Timeout override-ы были redundant (дефолт уже установлен в макросе).
Это cherry-pick изменений из коммита `b60c630c` на main, который не попал в базу worktree.

## Верификация

```
cargo test --workspace           # 381+ тестов, 0 failures
cargo clippy --workspace         # 0 warnings
cargo fmt --all --check          # clean
semgrep (ERROR + WARNING)        # 0 findings
check-arch.sh                    # no violations
```

## Для агента-мерджера

1. Коммит `6a4886e2` содержит 4 логических изменения (timeline, source, worker, stress test)
   + cleanup (hang_watchdog timeouts). Всё в одном коммите т.к. изменения взаимозависимы.

2. Изменения в `source.rs` и `worker.rs` включают удаление `timeout:` строк из
   `hang_watchdog!` — эти же изменения уже есть на main в коммите `b60c630c`.
   При мердже возможен trivial conflict в этих местах (строка уже удалена на обеих сторонах).

3. Новое поле `seek_pending` в `Timeline` — обратно совместимо. `Timeline::new()` инициализирует
   его как `false`. Существующий код, который не знает о `seek_pending`, продолжит работать.

4. Изменение возвращаемого типа `apply_pending_seek() -> bool` — breaking change для trait
   `AudioWorkerSource`. Единственная реализация — `StreamAudioSource` в `source.rs`, обновлена.
