# FSM Approach Comparison — Detailed Plans

## Текущая карта переходов (32 перехода)

```
Decoding ──→ SeekRequested        (seek preemption)
         ──→ WaitingForSource     (source not ready)
         ──→ RecreatingDecoder    (format boundary)
         ──→ AtEof               (decode returned None)
         ──→ Failed              (decode error, source cancelled/stopped)

SeekRequested ──→ ApplyingSeek    (anchor resolved)
              ──→ WaitingForSource (source not ready)
              ──→ Decoding        (stale/cleared seek)

ApplyingSeek ──→ AwaitingResume   (seek applied to decoder)
             ──→ WaitingForSource (source not ready)
             ──→ Failed           (source cancelled/stopped)

WaitingForSource ──→ Decoding          (Playback context, source ready)
                 ──→ SeekRequested     (Seek context, source ready)
                 ──→ ApplyingSeek      (ApplySeek context, source ready)
                 ──→ RecreatingDecoder (Recreation context, source ready)
                 ──→ Failed            (source cancelled/stopped)
                 ──→ AtEof             (source eof)

RecreatingDecoder ──→ Decoding          (success, next=Decode)
                  ──→ SeekRequested     (success, next=Seek)
                  ──→ WaitingForSource  (source not ready)
                  ──→ Failed            (recreation failed, source cancelled/stopped)

AwaitingResume ──→ Decoding  (first valid chunk)
               ──→ AtEof     (eof during resume)
               ──→ Failed    (decode error)

AtEof ──→ SeekRequested  (seek-after-EOF)

Failed ──→ (terminal)
```

---

## Вариант A: statig

### Концепция

Каждое состояние = отдельный handler method. Переходы возможны ТОЛЬКО из handler'а
текущего состояния. Hierarchy (superstates) для общего поведения.

### Иерархия superstates

```
┌─ root ─────────────────────────────────────┐
│  [seek preemption: Any → SeekRequested]    │
│                                            │
│  ┌─ active ──────────────────────────────┐ │
│  │  [source_cancelled → Failed]          │ │
│  │  [source_stopped → Failed]            │ │
│  │                                       │ │
│  │  ┌─ seeking ───────────────────────┐  │ │
│  │  │  SeekRequested                  │  │ │
│  │  │  ApplyingSeek                   │  │ │
│  │  │  AwaitingResume                 │  │ │
│  │  └─────────────────────────────────┘  │ │
│  │                                       │ │
│  │  Decoding                             │ │
│  │  WaitingForSource                     │ │
│  │  RecreatingDecoder                    │ │
│  │  AtEof                               │ │
│  └───────────────────────────────────────┘ │
│                                            │
│  Failed  (terminal, no superstate)         │
└────────────────────────────────────────────┘
```

### Код (полная реализация)

```rust
// crates/kithara-audio/src/pipeline/track_fsm.rs

use statig::prelude::*;

// === Events ===

pub(crate) enum TrackEvent {
    /// Source phase changed.
    SourcePhase(SourcePhase),
    /// Decode step result.
    DecodeResult(DecodeStepResult),
    /// Seek preemption from timeline.
    SeekPreempt(SeekContext),
    /// Seek anchor resolved.
    AnchorResolved(SeekMode),
    /// Seek target cleared/stale.
    SeekStale,
    /// Decoder recreation needed.
    NeedRecreation { cause: RecreateCause, media_info: MediaInfo, offset: u64 },
    /// Decoder recreation completed.
    RecreationDone(RecreateOutcome),
    /// Seek applied to decoder.
    SeekApplied { skip: Option<Duration> },
    /// First valid chunk after resume.
    ResumeChunkReceived,
}

pub(crate) enum DecodeStepResult {
    Produced,       // chunk ready
    Eof,            // decode returned None
    Error(DecodeError),
}

pub(crate) enum RecreateOutcome {
    Success,
    Failed { offset: u64 },
}

// === State Machine ===

// Shared context (lives in StreamAudioSource<T>)
pub(crate) struct TrackFsm {
    pub timeline: Arc<Timeline>,
    pub session: Option<DecoderSession>,
    // ... other shared fields
}

#[state_machine(
    initial = "State::decoding()",
    state(derive(Debug, PartialEq)),
    on_transition = "Self::on_transition",
)]
impl TrackFsm {
    // ── Leaf states ──

    #[state(superstate = "active")]
    fn decoding(&mut self, event: &TrackEvent) -> Response<State> {
        match event {
            TrackEvent::DecodeResult(DecodeStepResult::Eof) => {
                Transition(State::at_eof())
            }
            TrackEvent::DecodeResult(DecodeStepResult::Error(e)) => {
                Transition(State::failed(TrackFailure::Decode(e.clone())))
            }
            TrackEvent::NeedRecreation { cause, media_info, offset } => {
                Transition(State::recreating_decoder(RecreateState {
                    attempt: 0,
                    cause: *cause,
                    media_info: media_info.clone(),
                    next: RecreateNext::Decode,
                    offset: *offset,
                }))
            }
            TrackEvent::SourcePhase(phase) => {
                match map_source_phase(*phase) {
                    Some(reason) => Transition(State::waiting_for_source(
                        WaitContext::Playback, reason
                    )),
                    None => Super, // delegate Cancelled/Stopped to superstate
                }
            }
            _ => Super,
        }
    }

    #[state(superstate = "seeking")]
    fn seek_requested(
        request: &mut SeekRequest,
        event: &TrackEvent,
    ) -> Response<State> {
        match event {
            TrackEvent::AnchorResolved(mode) => {
                Transition(State::applying_seek(ApplySeekState {
                    mode: *mode,
                    request: *request,
                }))
            }
            TrackEvent::SeekStale => Transition(State::decoding()),
            TrackEvent::SourcePhase(phase) => {
                match map_source_phase(*phase) {
                    Some(reason) => Transition(State::waiting_for_source(
                        WaitContext::Seek(*request), reason
                    )),
                    None => Super,
                }
            }
            _ => Super,
        }
    }

    #[state(superstate = "seeking")]
    fn applying_seek(
        state: &mut ApplySeekState,
        event: &TrackEvent,
    ) -> Response<State> {
        match event {
            TrackEvent::SeekApplied { skip } => {
                Transition(State::awaiting_resume(ResumeState {
                    recover_attempts: 0,
                    seek: state.request.seek,
                    skip: *skip,
                }))
            }
            TrackEvent::SourcePhase(phase) => {
                match map_source_phase(*phase) {
                    Some(reason) => Transition(State::waiting_for_source(
                        WaitContext::ApplySeek(*state), reason
                    )),
                    None => Super,
                }
            }
            _ => Super,
        }
    }

    #[state(superstate = "seeking")]
    fn awaiting_resume(
        state: &mut ResumeState,
        event: &TrackEvent,
    ) -> Response<State> {
        match event {
            TrackEvent::ResumeChunkReceived => Transition(State::decoding()),
            TrackEvent::DecodeResult(DecodeStepResult::Eof) => {
                Transition(State::at_eof())
            }
            TrackEvent::DecodeResult(DecodeStepResult::Error(e)) => {
                Transition(State::failed(TrackFailure::Decode(e.clone())))
            }
            _ => Super,
        }
    }

    #[state(superstate = "active")]
    fn waiting_for_source(
        context: &mut WaitContext,
        reason: &mut WaitingReason,
        event: &TrackEvent,
    ) -> Response<State> {
        match event {
            TrackEvent::SourcePhase(SourcePhase::Ready) => {
                // Resume based on stored context
                match context {
                    WaitContext::Playback => Transition(State::decoding()),
                    WaitContext::Seek(req) => {
                        Transition(State::seek_requested(*req))
                    }
                    WaitContext::ApplySeek(app) => {
                        Transition(State::applying_seek(*app))
                    }
                    WaitContext::Recreation(rec) => {
                        Transition(State::recreating_decoder(rec.clone()))
                    }
                }
            }
            TrackEvent::SourcePhase(SourcePhase::Eof) => {
                Transition(State::at_eof())
            }
            _ => Super,
        }
    }

    #[state(superstate = "active")]
    fn recreating_decoder(
        state: &mut RecreateState,
        event: &TrackEvent,
    ) -> Response<State> {
        match event {
            TrackEvent::RecreationDone(RecreateOutcome::Success) => {
                match &state.next {
                    RecreateNext::Decode => Transition(State::decoding()),
                    RecreateNext::Seek(req) => {
                        Transition(State::seek_requested(*req))
                    }
                    RecreateNext::ApplySeek(req) => {
                        Transition(State::applying_seek(ApplySeekState {
                            mode: SeekMode::Direct,
                            request: *req,
                        }))
                    }
                }
            }
            TrackEvent::RecreationDone(RecreateOutcome::Failed { offset }) => {
                Transition(State::failed(TrackFailure::RecreateFailed {
                    offset: *offset,
                }))
            }
            TrackEvent::SourcePhase(phase) => {
                match map_source_phase(*phase) {
                    Some(reason) => Transition(State::waiting_for_source(
                        WaitContext::Recreation(state.clone()), reason
                    )),
                    None => Super,
                }
            }
            _ => Super,
        }
    }

    #[state(superstate = "active")]
    fn at_eof(event: &TrackEvent) -> Response<State> {
        // Seek-after-EOF handled by root superstate (seek preemption)
        Super
    }

    #[state]
    fn failed(failure: &mut TrackFailure, _event: &TrackEvent) -> Response<State> {
        // Terminal — never transitions
        Handled
    }

    // ── Superstates ──

    #[superstate]
    fn active(&mut self, event: &TrackEvent) -> Response<State> {
        match event {
            TrackEvent::SourcePhase(SourcePhase::Cancelled) => {
                Transition(State::failed(TrackFailure::SourceCancelled))
            }
            TrackEvent::SourcePhase(SourcePhase::Stopped) => {
                Transition(State::failed(TrackFailure::SourceStopped))
            }
            _ => Super, // delegate to root
        }
    }

    #[superstate]
    fn seeking(&mut self, event: &TrackEvent) -> Response<State> {
        // Shared seeking behavior (if any)
        Super // delegate to active
    }

    // Root handles seek preemption
    #[superstate]
    fn root(&mut self, event: &TrackEvent) -> Response<State> {
        match event {
            TrackEvent::SeekPreempt(seek) if !self.is_terminal() => {
                Transition(State::seek_requested(SeekRequest {
                    attempt: 0,
                    seek: *seek,
                }))
            }
            _ => Handled,
        }
    }

    // ── Transition observer ──

    fn on_transition(&mut self, source: &State, target: &State) {
        tracing::debug!(from = ?source, to = ?target, "track state transition");
    }
}
```

### Миграция

1. Добавить `statig = "0.4"` в workspace dependencies
2. Переписать `track_fsm.rs` (см. выше)
3. Рефактор `source.rs`:
   - `step_track()` → формирует `TrackEvent` из текущего контекста
   - Вызывает `self.fsm.handle(&event)` вместо `self.state = TrackState::X`
   - step_decoding/step_seeking/etc → логика генерации events
4. Обновить тесты
5. `internal.rs` — убрать прямую установку state, использовать events

**Объём**: ~500 строк нового кода, ~400 строк удалённого. 2-3 дня работы.

### Плюсы и минусы

✅ **Handler isolation** — переход из Decoding ТОЛЬКО в decoding() handler
✅ **Superstates** — `SourceCancelled/Stopped → Failed` один раз в `active()`
✅ **Transition observer** — логирование каждого перехода автоматически
✅ **State-local data** — `RecreateState` живёт только в `recreating_decoder()`
✅ **Стандартная библиотека** — поддерживается, ~1k stars

❌ **Не compile-time edge validation** — handler может вернуть любой State
❌ **Proc-macro непрозрачность** — сложнее отлаживать generated код
❌ **Внешняя зависимость** — ещё один крейт в workspace
❌ **Event modeling** — нужно проектировать TrackEvent enum (сейчас events неявные)
❌ **Boilerplate** — ~200 строк handler signatures

---

## Вариант B: Transition Table + Runtime Validation

### Концепция

Оставить hand-written enums. Добавить декларативную таблицу разрешённых переходов
и метод `transition()` с `debug_assert!` (dev) / `tracing::error!` (prod).

### Код (полная реализация)

```rust
// crates/kithara-audio/src/pipeline/track_fsm.rs — additions

/// Declarative transition table.
///
/// Each row: (FROM, TO) — allowed transition.
/// Validated at dev time via debug_assert, logged in prod.
const ALLOWED_TRANSITIONS: &[(TrackPhaseTag, TrackPhaseTag)] = &[
    // Decoding →
    (TrackPhaseTag::Decoding, TrackPhaseTag::SeekRequested),
    (TrackPhaseTag::Decoding, TrackPhaseTag::WaitingForSource),
    (TrackPhaseTag::Decoding, TrackPhaseTag::RecreatingDecoder),
    (TrackPhaseTag::Decoding, TrackPhaseTag::AtEof),
    (TrackPhaseTag::Decoding, TrackPhaseTag::Failed),

    // SeekRequested →
    (TrackPhaseTag::SeekRequested, TrackPhaseTag::ApplyingSeek),
    (TrackPhaseTag::SeekRequested, TrackPhaseTag::WaitingForSource),
    (TrackPhaseTag::SeekRequested, TrackPhaseTag::Decoding),        // stale
    (TrackPhaseTag::SeekRequested, TrackPhaseTag::Failed),

    // ApplyingSeek →
    (TrackPhaseTag::ApplyingSeek, TrackPhaseTag::AwaitingResume),
    (TrackPhaseTag::ApplyingSeek, TrackPhaseTag::WaitingForSource),
    (TrackPhaseTag::ApplyingSeek, TrackPhaseTag::Failed),

    // WaitingForSource →
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::Decoding),
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::SeekRequested),
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::ApplyingSeek),
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::RecreatingDecoder),
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::Failed),
    (TrackPhaseTag::WaitingForSource, TrackPhaseTag::AtEof),

    // RecreatingDecoder →
    (TrackPhaseTag::RecreatingDecoder, TrackPhaseTag::Decoding),
    (TrackPhaseTag::RecreatingDecoder, TrackPhaseTag::SeekRequested),
    (TrackPhaseTag::RecreatingDecoder, TrackPhaseTag::WaitingForSource),
    (TrackPhaseTag::RecreatingDecoder, TrackPhaseTag::Failed),

    // AwaitingResume →
    (TrackPhaseTag::AwaitingResume, TrackPhaseTag::Decoding),
    (TrackPhaseTag::AwaitingResume, TrackPhaseTag::AtEof),
    (TrackPhaseTag::AwaitingResume, TrackPhaseTag::Failed),

    // AtEof →
    (TrackPhaseTag::AtEof, TrackPhaseTag::SeekRequested),  // seek-after-EOF

    // Failed → (terminal, no outgoing transitions)
];

impl TrackState {
    /// Validates that the transition from `self` to `new` is allowed.
    ///
    /// In debug builds: panics on invalid transition.
    /// In release builds: logs error and allows (defensive).
    fn validate_transition(&self, new: &TrackState) {
        let from = self.phase_tag();
        let to = new.phase_tag();

        let allowed = ALLOWED_TRANSITIONS
            .iter()
            .any(|(f, t)| *f == from && *t == to);

        if !allowed {
            let msg = format!(
                "Invalid FSM transition: {:?} → {:?}",
                from, to
            );
            debug_assert!(false, "{}", msg);
            tracing::error!("{}", msg);
        }
    }
}

/// Wrapper that enforces transition validation.
///
/// Replaces direct `self.state = TrackState::X(...)` assignments.
pub(crate) struct TrackMachine {
    state: TrackState,
}

impl TrackMachine {
    pub fn new() -> Self {
        Self { state: TrackState::Decoding }
    }

    /// Transition to a new state. Validates the transition.
    pub fn transition(&mut self, new: TrackState) {
        self.state.validate_transition(&new);
        tracing::debug!(
            from = ?self.state.phase_tag(),
            to = ?new.phase_tag(),
            "track state transition"
        );
        self.state = new;
    }

    /// Read current state.
    pub fn state(&self) -> &TrackState {
        &self.state
    }

    /// Take ownership of state (for destructuring in match).
    pub fn take(&mut self) -> TrackState {
        std::mem::replace(&mut self.state, TrackState::Decoding)
    }

    pub fn phase_tag(&self) -> TrackPhaseTag {
        self.state.phase_tag()
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

// === Compile-time test: verify table covers all FROM states ===

#[cfg(test)]
mod transition_table_tests {
    use super::*;

    #[test]
    fn every_non_terminal_state_has_outgoing_transitions() {
        let all_tags = [
            TrackPhaseTag::Decoding,
            TrackPhaseTag::SeekRequested,
            TrackPhaseTag::WaitingForSource,
            TrackPhaseTag::ApplyingSeek,
            TrackPhaseTag::RecreatingDecoder,
            TrackPhaseTag::AwaitingResume,
            TrackPhaseTag::AtEof,
        ];

        for tag in &all_tags {
            let has_outgoing = ALLOWED_TRANSITIONS
                .iter()
                .any(|(from, _)| from == tag);
            assert!(
                has_outgoing,
                "State {:?} has no outgoing transitions in table",
                tag
            );
        }
    }

    #[test]
    fn failed_has_no_outgoing_transitions() {
        let has_outgoing = ALLOWED_TRANSITIONS
            .iter()
            .any(|(from, _)| *from == TrackPhaseTag::Failed);
        assert!(
            !has_outgoing,
            "Failed state should have no outgoing transitions"
        );
    }

    #[test]
    fn no_self_transitions() {
        for (from, to) in ALLOWED_TRANSITIONS {
            assert_ne!(from, to, "Self-transition {:?} → {:?} not allowed", from, to);
        }
    }

    #[test]
    fn no_transitions_to_decoding_from_applying_seek() {
        // ApplyingSeek must go through AwaitingResume, not directly to Decoding
        let has_direct = ALLOWED_TRANSITIONS
            .iter()
            .any(|(f, t)| *f == TrackPhaseTag::ApplyingSeek && *t == TrackPhaseTag::Decoding);
        assert!(
            !has_direct,
            "ApplyingSeek → Decoding should not be direct"
        );
    }
}
```

### Миграция

1. Добавить `TrackMachine`, `ALLOWED_TRANSITIONS` в `track_fsm.rs`
2. В `source.rs` заменить `self.state` на `self.machine: TrackMachine`
3. Заменить все `self.state = TrackState::X(...)` на `self.machine.transition(TrackState::X(...))`
4. Прогнать тесты — `debug_assert!` поймает любые ошибки в таблице

**Объём**: ~100 строк нового кода, ~50 строк изменений в source.rs. 0.5-1 день работы.

### Плюсы и минусы

✅ **Декларативная таблица** — все разрешённые переходы в одном месте
✅ **Runtime validation** — `debug_assert!` + `tracing::error!`
✅ **Минимальная миграция** — заменить `self.state =` на `self.machine.transition()`
✅ **Нулевая зависимость** — чистый Rust
✅ **Полная прозрачность** — нет proc-macro, всё видно в debugger'е
✅ **Zero overhead в release** — `debug_assert!` компилируется в nothing
✅ **Таблица как спецификация** — тесты верифицируют полноту

❌ **Нет handler isolation** — переход из любого места в коде (через `transition()`)
❌ **Runtime-only validation** — невалидный переход = panic в dev, log в prod
❌ **Нет hierarchy** — `SourceCancelled → Failed` дублируется в каждом step_*
❌ **Таблица может устареть** — при добавлении нового state нужно обновлять вручную
❌ **Не предотвращает** — только обнаруживает невалидные переходы

---

## Сравнение

| Критерий | statig | Transition Table |
|----------|:------:|:----------------:|
| Регламентация переходов | **Структурная** (handler isolation) | **Runtime** (debug_assert) |
| Предотвращение vs обнаружение | Предотвращает (нет кода вне handler) | Обнаруживает (panic/log) |
| Hierarchy/superstates | ✅ | ❌ |
| Дублирование shared logic | Минимальное (superstates) | Значительное |
| Transition logging | Автоматическое (on_transition) | Через wrapper |
| Debuggability | ⚠️ proc-macro | ✅ plain Rust |
| Зависимость | statig 0.4 (~40KB) | нет |
| Объём миграции | 2-3 дня | 0.5-1 день |
| Масштабируемость | Отличная | Средняя |
| Подходит для простых FSM | Overkill | ✅ |
| Таблица переходов как код | В handler'ах (distributed) | В const массиве (centralized) |
