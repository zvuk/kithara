# Synthesis: Enriching SourcePhase vs is_fetch_ready()

## Участники
- **Codex** — Approach B: новый метод `is_fetch_ready() -> bool`
- **Gemini** — Approach A: новые варианты `ReadySegment`, `ReadyFull` в SourcePhase
- **Claude** — Hybrid: новый метод `source_phase() -> SourcePhase` (без аргументов)

## Ключевая проблема (все согласны)

Audio worker `is_ready()` reimплементирует source-internal логику:
- `current_segment_range()` — HLS-specific knowledge
- `pos + 32KB` — decoder-window heuristic
- `len().min()` — EOF clamping

Эта логика принадлежит source, а не audio worker.

---

## Comparison Matrix

| Критерий | A: ReadySegment/ReadyFull | B: is_fetch_ready() | C: source_phase() |
|---|---|---|---|
| Reuses SourcePhase | Да, новые варианты | Нет, отдельный bool | Да, без изменений enum |
| Split-brain risk | Нет | Да (phase ≠ ready) | Нет |
| Breaking changes | Audit всех match | Additive | Additive |
| Segment leak в trait | Да (`ReadySegment`) | Нет | Нет |
| Empty-range hack | Да (`pos..pos`) | Нет | Нет |
| Worker simplicity | Высокая | Высокая | Высокая |
| Default correctness | Нужна логика | false (fail-safe) | Делегирует phase(pos..pos+1) |
| Новые методы | 0 | +1 (bool) | +1 (SourcePhase) |
| Discoverability | Высокая (enum) | Низкая (ещё один метод) | Средняя |

---

## Точки согласия (3/3)

1. **Audio worker не должен вычислять source-specific ranges** — логика chunk/segment boundaries принадлежит source
2. **Default должен быть fail-safe** — ложный `false`/`Waiting` лучше ложного `true`/`Ready`
3. **Enum `#[non_exhaustive]`** — добавление вариантов не ломает downstream `match` с wildcard

## Точки расхождения

### Codex vs Gemini/Claude: bool vs enum

**Codex**: `is_fetch_ready() -> bool` — минимальный, чистый, additive. Не смешивает lifecycle и readiness.

**Контраргумент (Gemini)**: split-brain — `phase()` может вернуть `Waiting`, а `is_fetch_ready()` — `true`. Два канала для одной информации.

**Контраргумент (Codex)**: SourcePhase УЖЕ смешивает lifecycle и readiness (`Ready`, `Waiting`, `WaitingDemand`). Добавлять ещё readiness-варианты — consistent с текущим дизайном.

### Gemini vs Claude: новые варианты vs новый метод

**Gemini**: `ReadySegment`/`ReadyFull` — rich semantic signal. Worker может оптимизировать (skip readiness checks для `ReadyFull`).

**Claude**: `ReadySegment` leaks HLS semantics. Что значит "segment" для File? Все три Ready-варианта для worker'а эквивалентны → не нужны отдельные варианты.

---

## Рекомендация

**Подход C (Claude) с элементами B (Codex)** — наиболее прагматичный:

```rust
/// Overall source readiness at the current timeline position.
///
/// Unlike `phase(range)` which checks a specific byte range,
/// this method uses the source's internal knowledge of chunk/segment
/// boundaries to determine if the next read can proceed without blocking.
///
/// Returns `SourcePhase` using the existing enum variants.
fn source_phase(&self) -> SourcePhase {
    let pos = self.timeline().byte_position();
    self.phase(pos..pos.saturating_add(1))
}
```

### Почему:
1. **Нет новых enum вариантов** — zero breaking change risk
2. **Нет split-brain** — единый канал (SourcePhase)
3. **Нет segment leak** — source сам решает, что значит "ready"
4. **Additive** — default делегирует к существующему `phase()`
5. **Audio worker упрощается** до `match self.shared_stream.source_phase()`
6. **HLS/File переопределяют** с chunk-aware логикой (segment / 32KB)

### Но если пользователь настаивает на обогащении enum:

Добавить **один** вариант без HLS-семантики:

```rust
pub enum SourcePhase {
    // ... existing ...
    /// Source is fully loaded — all future reads are non-blocking.
    Complete,
}
```

`Complete` полезен для: local files, MemorySource, fully-cached HLS/File. Worker может отключить readiness checks для `Complete` треков. Это единственный вариант, который даёт **новую** информацию, отсутствующую в текущем enum.

---

## Итог

| Шаг | Действие |
|---|---|
| 1 | Добавить `source_phase() -> SourcePhase` с default |
| 2 | Опционально: добавить `SourcePhase::Complete` |
| 3 | Проксировать через `Stream<T>` и `SharedStream` |
| 4 | Упростить `is_ready()` в audio worker |
| 5 | HLS/File переопределяют `source_phase()` |
