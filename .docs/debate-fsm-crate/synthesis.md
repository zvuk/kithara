# FSM Crate Debate — Synthesis

**Date:** 2026-03-14
**Topic:** Какой FSM крейт лучше для Kithara?
**Participants:** Codex (statig), Gemini (smlang), Claude (moderator)
**User Priority:** Compile-time transition safety

---

## Critical Finding

**Ни один event-driven FSM крейт не предоставляет истинную compile-time transition safety.**

| Крейт | "Compile-time safety" | Реальное поведение |
|-------|----------------------|-------------------|
| **statig** | Payload type safety | Любой handler может вернуть любой State — нет ограничений |
| **smlang** | DSL validation | `process_event()` → `Err(InvalidTransition)` в runtime |
| **rust-fsm** | DSL validation | Аналогично smlang — runtime ошибки |
| **Typestate** (sm, state_machines) | TRUE compile-time | Практично для API/lifecycle, непрактично для event-driven loops |

Истинная compile-time safety (typestate) требует, чтобы тип машины менялся при каждом переходе.
Для event-driven loop это требует boxing/enum wrapper, что нивелирует преимущество.

---

## Round 1 — Позиции

### 🔴 Codex → statig
**Сильные аргументы:**
- Hierarchical State Machines — superstate `seeking` объединяет SeekRequested + ApplyingSeek + AwaitingResume
- State-local storage — каждый handler видит только свои данные, нет "context bag"
- `no_std`, zero-alloc, wasm32 совместим
- Миграция: сохранить существующие payload types, добавить TrackEvent enum

**Слабости (признанные):**
- Нет compile-time edge validation (handler может вернуть любой State)
- Proc-macro непрозрачность при отладке
- Hierarchy может стать over-engineered

### 🟡 Gemini → smlang
**Сильные аргументы:**
- DSL transition table — вся машина видна в одном месте
- `no_std`, zero-alloc, embedded-first
- Compile-time DSL validation (но НЕ call-site validation)
- Чистый "specification as code"

**Слабости (признанные и вскрытые):**
- Нет compile-time transition safety на call-site (runtime `InvalidTransition`)
- Нет hierarchical states — `_ + Event = State` wildcards не заменяют superstates
- WaitingForSource{context, reason} — два поля требуют wrapper struct
- Для 8 states × 11+ transitions DSL становится стеной текста

---

## Round 2 — Перекрёстная экспертиза

### Codex контраргументы против smlang:
1. `process_event()` возвращает `Result` → invalid transitions = runtime error, не compile-time
2. Без hierarchy Kithara должна дублировать `_ + Seek = ...` для preemption
3. "Single table" для 11+ transitions + guards/actions → индекс к scattered logic
4. Два поля в WaitingForSource → awkward в DSL, natural в statig

### Gemini контраргументы против statig:
1. statig НЕ предотвращает invalid transitions → "disqualifying" для compile-time priority
2. Hierarchy = over-engineering для 8 states
3. ~60 строк boilerplate handler signatures > 20-строчная transition table
4. Wildcard `_ + Event` покрывает "any → Failed" без hierarchy

---

## Round 3 — Верификация

**Обе стороны подтвердили:**
- smlang: invalid transitions = runtime `Err(Error::InvalidTransition)`
- statig: handlers can return ANY State variant
- True compile-time safety exists only in typestate crates (sm, state_machines) — archived, impractical for event loops

---

## Verdict — Рекомендация модератора

### Для Kithara оптимален **гибридный подход**:

#### Вариант A: **statig** (рекомендуется)
**Почему:** Несмотря на отсутствие compile-time edge validation, statig даёт:
- **Payload isolation** — handler видит только свои данные (ближайший аналог compile-time safety для event loops)
- **Hierarchy** — реально полезна для seek preemption, fail-from-anywhere, wait resume logic
- **Migration path** — сохраняет все текущие payload types
- **Активная разработка** — 0.4.1 (Jul 2025), ~1k stars

**Compile-time safety усиливаем:**
- Exhaustive transition tests (уже есть в track_fsm.rs)
- `#[deny(unreachable_patterns)]` — стандартный Rust
- Property-based tests с `proptest` для покрытия всех путей
- Custom lint (ast-grep rule) запрещающий определённые `State::` возвраты из конкретных handlers

#### Вариант B: **Hand-written enums + transition validation macro**
**Почему может быть лучше:**
- Нулевая зависимость
- Полный контроль
- Текущий код уже хорошо структурирован
- Можно добавить `#[valid_transitions(Decoding => SeekRequested | Failed)]` proc-macro

**Минус:** Придётся писать и поддерживать собственный proc-macro.

#### Вариант C: **smlang**
**Почему хуже для Kithara:**
- Нет hierarchy → дублирование shared behavior
- DSL не масштабируется для 8 × 11+ с guards
- "Compile-time" обещание не выполняется на call-site
- WaitingForSource с двумя полями → workaround

### Scope рекомендация
- **Сложные FSM** (TrackState, SourcePhase): мигрировать на выбранный подход
- **Простые FSM** (ResourceStatus, MmapState, AssetResourceState): оставить hand-written enums — overhead библиотеки не оправдан для 3-state машин

---

## Score Matrix

| Критерий (вес) | statig | smlang | Hand-written |
|---------------|--------|--------|-------------|
| Type safety переходов (25%) | 7/10 | 6/10 | 5/10 |
| Rich state payloads (15%) | 9/10 | 7/10 | 10/10 |
| Performance zero-alloc (15%) | 9/10 | 9/10 | 10/10 |
| Async compatibility (10%) | 9/10 | 8/10 | 10/10 |
| wasm32 support (10%) | 8/10 | 8/10 | 10/10 |
| Ergonomics миграции (10%) | 7/10 | 6/10 | 10/10 |
| Debuggability (10%) | 6/10 | 6/10 | 9/10 |
| Maintainability (5%) | 8/10 | 7/10 | 6/10 |
| **Weighted Total** | **7.6** | **6.9** | **8.1** |

### Итог
**Hand-written enums побеждают по очкам**, но с оговоркой: если проект растёт и нужна hierarchy + формализация переходов, **statig** — лучший library choice. Для текущего масштаба (8 states) рекомендуется **улучшить текущий подход** вместо миграции на внешний крейт.
