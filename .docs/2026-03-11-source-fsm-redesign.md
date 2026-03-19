# Shared SourcePhase FSM Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Вынести общий wait/read FSM core в `kithara-stream`, заставить и `FileSource`, и `HlsSource` использовать один `SourcePhase`, затем удалить HLS-specific helper stack вокруг `wait_range` так, чтобы итоговый refactor был net-negative по строкам.

**Architecture:** Предыдущая более общая версия была ближе к правильной архитектуре, чем узкий HLS-only cleanup. Этот план возвращает shared core, но ограничивает его wait/read orchestration уровнем: `kithara-stream` владеет общим `SourcePhase` и чистым classifier, а `kithara-file` и `kithara-hls` только адаптируют локальные наблюдения к этому core. HLS-specific вещи, которые уже работают (`variant_fence`, `SeekLayout`, `classify_seek`), сознательно не трогаются.

**Tech Stack:** Rust workspace, `kithara-stream`, `kithara-file`, `kithara-hls`, `kithara-storage`, `Timeline`, existing hang detector macros, existing source unit tests.

---

## Hard Constraints

Эти ограничения обязательны. Если какой-то шаг нарушает их, шаг надо пересобрать, а не "договариваться с кодом".

0. Работа ведется в отдельном worktree внутри `.worktrees`, через `wt`.
1. `SourcePhase` общий для `HlsSource` и `FileSource`; определяется в `kithara-stream`.
2. Критерий чистоты один: итоговый refactor должен удалить больше строк, чем добавить.
3. Сигнатуры `Source` trait не меняются:
   - `wait_range`
   - `read_at`
   - `seek_time_anchor`
4. `kithara-audio`, decoder path, `StreamAudioSource` не трогаются.
5. Все существующие тесты для `Stream<Hls>` и `Stream<File>` должны остаться зелёными.
6. `variant_fence: Option<usize>` остается как есть.
7. `SeekLayout` и `classify_seek()` не трогаются.
8. Каждый шаг начинается с RED-теста.
9. После каждого шага обязателен review gate через Codex.
10. Перед каждым шагом обязателен explore gate через Gemini.
11. Каждый шаг завершается отдельным commit.
12. Любой цикл, который потенциально может зависнуть, обязан быть под `kithara_platform::hang_watchdog!`; нельзя добавлять новый открытый `loop {}` без hang detector.

---

## Architecture Decision

### Why the previous broader version was directionally right

Упрек справедливый: предыдущая узкая версия слишком сильно сжала архитектуру ради локального выигрыша по строкам и тем самым снова сделала `HlsSource` центром модели. Это плохой сигнал, потому что тогда `file` опять становится "особым случаем без общих правил", а FSM снова превращается в локальную HLS-технику.

### What this version keeps from that broader direction

Этот план возвращает именно shared core:

- общий `SourcePhase` живет в `kithara-stream`
- общий classifier тоже живет в `kithara-stream`
- `FileSource` и `HlsSource` оба строят один и тот же `SourcePhaseView`
- HLS-specific поведение выражается не новым ad-hoc кодом, а дополнительными состояниями того же FSM

### What this version still intentionally does not do

Этот план все еще не переносит весь blocking wait loop в `kithara-stream`. Это было бы еще "чище" архитектурно, но:

- это сильнее лезет в существующий `Source` contract
- резко увеличивает риск затронуть `kithara-audio` и pipeline path
- почти наверняка съедает требование "удалить больше строк, чем добавить"

То есть итоговая форма такая:

- shared FSM core в `kithara-stream`
- thin adapters в `kithara-file` и `kithara-hls`
- главный line-count win все еще берется из удаления HLS helper stack

---

## Shared FSM Core

### Shared types

В `kithara-stream` добавляется не только общий enum, но и общий input для classifier.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SourcePhase {
    Cancelled,
    Eof,
    Ready,
    Seeking,
    Stopped,
    #[default]
    Waiting,
    WaitingDemand,
    WaitingMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SourcePhaseView {
    pub cancelled: bool,
    pub past_eof: bool,
    pub range_ready: bool,
    pub seeking: bool,
    pub stopped: bool,
    pub waiting_demand: bool,
    pub waiting_metadata: bool,
}
```

### Shared classifier

`kithara-stream` also owns the pure classifier:

```rust
impl SourcePhase {
    #[must_use]
    pub fn classify(view: SourcePhaseView) -> Self {
        if view.cancelled {
            return Self::Cancelled;
        }
        if view.stopped && !view.range_ready {
            return if view.past_eof { Self::Eof } else { Self::Stopped };
        }
        if view.range_ready {
            return Self::Ready;
        }
        if view.seeking {
            return Self::Seeking;
        }
        if view.past_eof {
            return Self::Eof;
        }
        if view.waiting_metadata {
            return Self::WaitingMetadata;
        }
        if view.waiting_demand {
            return Self::WaitingDemand;
        }
        Self::Waiting
    }
}
```

### Why these states

Это снова не "плоский giant enum на все времена", но и не слишком бедная модель. Состояния достаточны, чтобы не скатываться обратно в fallback-ветки после первой же серии edge cases:

- `Ready`
- `Seeking`
- `Eof`
- `Cancelled`
- `Stopped`
- `Waiting`
- `WaitingDemand`
- `WaitingMetadata`

`FileSource` будет использовать подмножество, `HlsSource` будет использовать все или почти все. Это нормально: один FSM core не обязан означать, что все implementors посещают все состояния.

### Output mapping

У shared FSM одна vocabulary, а mapping на текущий `Source` API остается source-local:

- `Ready` -> `WaitOutcome::Ready`
- `Seeking` -> `WaitOutcome::Interrupted`
- `Eof` -> `WaitOutcome::Eof`
- `Cancelled` -> source error
- `Stopped` -> source error
- `Waiting*` -> loop / request / wait path

---

## Hang Detector Rule

Это новое жесткое правило для rollout:

- Если шаг оставляет существующий потенциально бесконечный цикл, `hang_watchdog!` обязан остаться вокруг него.
- Если шаг переносит цикл в helper, wrapper переносится вместе с циклом.
- Нельзя добавлять новый spin / wait loop вне `hang_watchdog!`.
- Тесты, которые ждут пробуждения/сигнала, обязаны использовать `timeout(...)`, а не бесконечное ожидание.

На текущем коде это означает:

- `crates/kithara-hls/src/source.rs::wait_range()` остается под `kithara_platform::hang_watchdog!`
- `crates/kithara-stream/src/stream.rs::read()` не трогаем
- `kithara-file` не получает новый самописный loop в этом refactor

---

## Borrow Checker Rule

Исходная идея с `update_phase(&mut self)` плоха для этой реализации.

Причина:

- в HLS phase часто считается при удержанном `self.shared.segments.lock_sync()`
- `&mut self` на этом пути либо не пройдет borrow checker, либо заставит материализовать state field, который здесь только добавит stale complexity

Правильный API:

```rust
fn phase_view(
    &self,
    segments: &DownloadState,
    range: &Range<u64>,
    on_demand_epoch: Option<u64>,
    metadata_miss_count: usize,
) -> SourcePhaseView
```

И затем:

```rust
let phase = SourcePhase::classify(self.phase_view(
    &segments,
    &range,
    on_demand_epoch,
    metadata_miss_count,
));
```

То есть:

- state field `self.phase` не добавляем
- `update_phase(&mut self)` не добавляем
- фазу вычисляем как pure value внутри loop

---

## Module Rollout Order

Порядок зафиксирован и не меняется:

1. `kithara-stream`
2. `kithara-file`
3. `kithara-hls`

Это важно, потому что сначала надо доказать, что core реально общий, и только потом удалять HLS-specific scaffolding.

---

## Implementation Plan

### Task 0: Create Dedicated Worktree and Baseline

**Files:**
- No code changes

**Goal:** Работать в отдельном `wt` worktree и зафиксировать baseline SHA для LOC gate.

**Step 0.1: Gemini explore**

Проверить:

- baseline branch
- рабочее дерево чистое или сознательно переносимое
- `wt` project config действительно создает worktree внутри `.worktrees`

**Step 0.2: Create worktree**

Run:

```bash
wt switch --create source-phase-fsm
wt list
git rev-parse HEAD
```

Expected:

- worktree создан через `wt`
- path worktree находится под `.worktrees/`
- `git rev-parse HEAD` сохранить как `BASE_SHA`

Если `wt` не кладет worktree в `.worktrees`, сначала починить `wt` config, а не обходить его вручную.

**Step 0.3: Baseline gate**

Run:

```bash
cargo test -p kithara-stream -p kithara-file -p kithara-hls
```

Expected:

- baseline green before edits

No commit here.

---

### Task 1: Add Shared FSM Core to `kithara-stream`

**Files:**
- Modify: `crates/kithara-stream/src/source.rs`
- Modify: `crates/kithara-stream/src/lib.rs`
- Test: `crates/kithara-stream/src/source.rs`

**Current anchors:**
- `crates/kithara-stream/src/source.rs` near `ReadOutcome` and `Source`
- `crates/kithara-stream/src/lib.rs` `pub use source::{...}`

**Delete:**
- None

**Add:**
- `SourcePhase`
- `SourcePhaseView`
- `SourcePhase::classify(...)`
- unit tests for classifier priority

**Expected LOC effect:**
- production code `+30..45`

**Step 1.1: Gemini explore**

Проверить:

- лучший placement: same `source.rs` vs new module
- public API impact
- `#[non_exhaustive]` requirements

**Step 1.2: Write RED tests**

Добавить tests:

- `source_phase_defaults_to_waiting`
- `source_phase_classify_prefers_cancelled`
- `source_phase_classify_prefers_ready_over_seeking`
- `source_phase_classify_returns_waiting_demand`
- `source_phase_classify_returns_waiting_metadata`
- `source_phase_classify_returns_stopped_before_eof`

Run:

```bash
cargo test -p kithara-stream source_phase_classify
```

Expected:

- FAIL because shared phase types do not exist yet

**Step 1.3: Implement minimal code**

Добавить shared enum + shared input + shared classifier.

Important:

- no new trait methods
- no `Source` signature changes
- no extra helper types beyond what both `file` and `hls` will use immediately

**Step 1.4: Gate**

Run:

```bash
cargo test -p kithara-stream source_phase
```

**Step 1.5: Codex review**

Review focus:

- API surface minimal but sufficient
- no unused abstractions
- classifier ordering matches current HLS semantics

**Step 1.6: Commit**

```bash
git add crates/kithara-stream/src/source.rs crates/kithara-stream/src/lib.rs
git commit -m "refactor: add shared source phase core"
```

---

### Task 2: Make `FileSource` Use the Shared FSM Core

**Files:**
- Modify: `crates/kithara-file/src/session.rs`
- Test: `crates/kithara-file/src/session.rs`

**Current anchors:**
- `crates/kithara-file/src/session.rs::wait_range()`
- file source unit tests at bottom of same file

**Delete:**
- none required

**Add:**
- `fn phase(&self, range: &Range<u64>) -> SourcePhase`
- early-return fast path in `wait_range()`
- unit tests proving file participates in the shared FSM

**Expected LOC effect:**
- production code `+12..20`

**Step 2.1: Gemini explore**

Проверить:

- safe EOF fast path using known final length
- safe `Seeking` fast path from `Timeline::is_flushing()`
- no need for new file-owned wait loop

**Step 2.2: Write RED tests**

Добавить tests:

- `file_source_phase_ready_when_range_present`
- `file_source_phase_seeking_when_timeline_flushing`
- `file_source_phase_eof_past_known_length`
- `file_source_wait_range_returns_interrupted_while_flushing`

Run:

```bash
cargo test -p kithara-file file_source_phase
cargo test -p kithara-file file_source_wait_range_returns_interrupted_while_flushing
```

Expected:

- FAIL because file does not use shared phase yet

**Step 2.3: Implement minimal code**

В `FileSource`:

- собрать `SourcePhaseView`
- вызвать `SourcePhase::classify(...)`
- dispatch only fast paths:
  - `Ready` -> `WaitOutcome::Ready`
  - `Seeking` -> `WaitOutcome::Interrupted`
  - `Eof` -> `WaitOutcome::Eof`
  - otherwise continue with existing on-demand request + `res.wait_range(range)`

Important:

- no custom file spin loop
- no changes to `read_at`
- no changes to downloader

**Step 2.4: Gate**

Run:

```bash
cargo test -p kithara-file
```

**Step 2.5: Codex review**

Review focus:

- file now obeys same phase vocabulary as hls
- no accidental architecture drift into a second local FSM
- code addition stays small

**Step 2.6: Commit**

```bash
git add crates/kithara-file/src/session.rs
git commit -m "refactor: make file source use shared source phase"
```

---

### Task 3: Add HLS Phase Adapter Over the Shared Core

**Files:**
- Modify: `crates/kithara-hls/src/source_wait_range.rs`
- Test: `crates/kithara-hls/src/source_wait_range.rs`

**Current anchors:**
- `WaitRangeState`
- `WaitRangeContext`
- `WaitRangeDecision`
- `build_wait_range_context()`
- `decide_wait_range()`

**Delete in this task:**
- none yet

**Add:**
- `fn is_past_eof(...) -> bool`
- `fn phase_view(...) -> SourcePhaseView`
- classifier tests that exercise the richer state set

**Expected LOC effect:**
- production code `+20..30`

**Step 3.1: Gemini explore**

Проверить:

- ordering of shared classifier vs current HLS semantics
- how `on_demand_epoch` and `metadata_miss_count` map to phase view
- exact meaning of `Stopped` vs `Cancelled`

**Step 3.2: Write RED tests**

Добавить tests:

- `hls_phase_ready_when_range_ready`
- `hls_phase_seeking_when_flushing`
- `hls_phase_waiting_demand_when_pending_epoch_matches`
- `hls_phase_waiting_metadata_after_metadata_miss`
- `hls_phase_eof_when_past_effective_total`
- `hls_phase_stopped_when_stopped_before_eof`
- `hls_phase_cancelled_when_cancel_token_set`

Run:

```bash
cargo test -p kithara-hls hls_phase_
```

Expected:

- FAIL because HLS adapter over shared core does not exist yet

**Step 3.3: Implement minimal code**

`phase_view(...)` should derive:

- `cancelled` from `self.shared.cancel`
- `range_ready` from `self.range_ready_from_segments(...)`
- `seeking` from `self.shared.timeline.is_flushing()`
- `past_eof` from `is_past_eof(...)`
- `stopped` from `self.shared.stopped`
- `waiting_demand` from `on_demand_epoch == Some(seek_epoch)`
- `waiting_metadata` from `metadata_miss_count > 0 && on_demand_epoch != Some(seek_epoch)`

Important:

- pure function only
- no `self.phase` field
- no `&mut self` phase updater

**Step 3.4: Gate**

Run:

```bash
cargo test -p kithara-hls hls_phase_
```

**Step 3.5: Codex review**

Review focus:

- richer shared states are actually used
- adapter is borrow-checker-safe
- no HLS-only state machine is being reintroduced under a different name

**Step 3.6: Commit**

```bash
git add crates/kithara-hls/src/source_wait_range.rs
git commit -m "refactor: add hls adapter for shared source phase"
```

---

### Task 4: Collapse the HLS Helper Stack into the Shared FSM Flow

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`
- Modify: `crates/kithara-hls/src/source_wait_range.rs`
- Test: `crates/kithara-hls/src/source.rs`
- Test: `crates/kithara-hls/src/source_wait_range.rs`

**Current anchors:**
- `crates/kithara-hls/src/source.rs::wait_range()` around the existing `hang_watchdog!` loop
- `crates/kithara-hls/src/source_wait_range.rs:1..167` helper stack to be deleted

**Delete:**
- `WaitRangeState`
- `WaitRangeContext`
- `WaitRangeDecision`
- `build_wait_range_context()`
- `decide_wait_range()`
- `request_on_demand_if_needed()`

**Keep:**
- `request_on_demand_segment()`
- `committed_segment_for_offset()`
- `fallback_segment_index_for_offset()`
- `variant_fence`
- `SeekLayout`
- `classify_seek()`

**Add:**
- local `on_demand_epoch: Option<u64>`
- local `metadata_miss_count: usize`
- explicit `SourcePhase` dispatch in `wait_range()`

**Expected LOC effect:**
- production delete `-120..140`
- production add `+35..50`
- expected net HLS delta `-70..95`

**Step 4.1: Gemini explore**

Проверить:

- where current helper behavior must stay byte-for-byte equivalent
- which old tests must be rewritten vs kept
- net LOC still negative after this step

**Step 4.2: Write RED tests**

Добавить or rewrite tests:

- `wait_range_clears_pending_after_seek_epoch_change`
- `wait_range_clears_pending_after_midstream_switch`
- `wait_range_retries_metadata_path_without_duplicate_demand_state`
- `wait_range_returns_timeout_after_budget_exhaustion`

Существующие tests around pending on-demand should be adapted so they assert phase-driven semantics, not deleted behavior names.

Run:

```bash
cargo test -p kithara-hls wait_range_
```

Expected:

- FAIL before helper-stack removal

**Step 4.3: Implement minimal code**

Inside `HlsSource::wait_range()`:

- keep the existing `kithara_platform::hang_watchdog!`
- keep `hang_reset!()` only when the requested range is actually ready
- keep `hang_tick!()` before sleep/wait
- replace helper stack with:
  - local loop vars: `on_demand_epoch`, `metadata_miss_count`
  - `let phase = SourcePhase::classify(self.phase_view(...));`
  - explicit phase dispatch

Dispatch rules:

- `Cancelled` -> `Err(StreamError::Source(HlsError::Cancelled))`
- `Stopped` -> `Err(StreamError::Source(HlsError::Cancelled))`
- `Ready` -> `Ok(WaitOutcome::Ready)`
- `Seeking` -> `Ok(WaitOutcome::Interrupted)`
- `Eof` -> `Ok(WaitOutcome::Eof)`
- `WaitingDemand` -> do not queue a duplicate request
- `WaitingMetadata` -> retry metadata path, but do not invent a second fallback mechanism
- `Waiting` -> attempt `request_on_demand_segment(...)`

Pending-state invalidation rules stay explicit:

- clear `on_demand_epoch` when `seek_epoch` changes
- clear `on_demand_epoch` when `timeline.eof()` and range not ready
- clear `on_demand_epoch` when `had_midstream_switch.swap(false, ...)`
- reset `metadata_miss_count` on successful request or ready range

Important:

- no new helper structs replacing the deleted helper structs
- no new unguarded loops

**Step 4.4: Gate**

Run:

```bash
cargo test -p kithara-hls
```

**Step 4.5: Review LOC gate**

Run:

```bash
git diff --numstat BASE_SHA..HEAD
```

Expected:

- deleted lines already exceed added lines, or are close enough that Task 5 can only go further negative

**Step 4.6: Codex review**

Review focus:

- shared FSM core is now genuinely used by HLS
- helper stack is actually gone
- no semantic regression in seek / switch / eof handling
- hang detector still protects the only open-ended wait loop

**Step 4.7: Commit**

```bash
git add crates/kithara-hls/src/source.rs crates/kithara-hls/src/source_wait_range.rs
git commit -m "refactor: drive hls wait_range through shared source phase"
```

---

### Task 5: Final Cross-Crate Verification

**Files:**
- No required code changes
- Optional: update this plan with actual LOC delta after implementation

**Goal:** Подтвердить, что refactor одновременно и архитектурно общий, и меньше по коду.

**Step 5.1: Gemini explore**

Проверить:

- shared core правда в `kithara-stream`, а не только на бумаге
- `file` и `hls` правда используют один classifier contract
- никаких скрытых вылазок в audio/decoder path нет

**Step 5.2: Full test gate**

Run:

```bash
cargo test -p kithara-stream -p kithara-file -p kithara-hls
```

Optional lint parity:

```bash
cargo clippy -p kithara-stream -p kithara-file -p kithara-hls --all-targets -- -D warnings
```

**Step 5.3: Final LOC gate**

Run:

```bash
git diff --numstat BASE_SHA..HEAD
```

Expected:

- total deleted lines > total added lines

**Step 5.4: Codex review**

Checklist:

- `SourcePhase` is shared and lives in `kithara-stream`
- shared classifier lives in `kithara-stream`
- `FileSource` and `HlsSource` both build `SourcePhaseView`
- `Source` trait signatures unchanged
- `variant_fence` untouched
- `SeekLayout` untouched
- no changes in `kithara-audio`, decoder path, `StreamAudioSource`
- every potential spin loop is under `hang_watchdog!`
- tests green

**Step 5.5: Final commit**

If review produced fixes:

```bash
git add crates/kithara-stream crates/kithara-file crates/kithara-hls docs/plans/2026-03-11-source-fsm-redesign.md
git commit -m "refactor: share source phase fsm across file and hls"
```

---

## Dependencies Between Tasks

- Task 1 is a hard prerequisite for everything else.
- Task 2 must happen before HLS cleanup, otherwise the "shared" core is still effectively HLS-first.
- Task 3 introduces the HLS adapter and richer states before deletion, so Task 4 can be a mechanical collapse instead of a blind rewrite.
- Task 4 is where the net-negative LOC is actually earned.
- Task 5 is only validation and cleanup.

---

## Definition of Done

Работа завершена только если одновременно выполнены все пункты:

1. Есть один общий `SourcePhase` в `kithara-stream`.
2. Есть один общий classifier в `kithara-stream`.
3. `FileSource` и `HlsSource` оба используют этот core.
4. `Source` trait signatures не изменены.
5. `kithara-audio`, decoder path, `StreamAudioSource` не изменены.
6. `variant_fence` оставлен как есть.
7. `SeekLayout` и `classify_seek()` не изменены.
8. Старый HLS helper stack вокруг `wait_range` удален.
9. Все потенциально бесконечные циклы в затронутом коде защищены `hang_watchdog!`.
10. Итоговый refactor удаляет больше строк, чем добавляет.
11. После каждого task есть:
    - Gemini explore
    - RED test
    - cargo test gate
    - Codex review
    - commit
12. Финальные тесты `kithara-stream`, `kithara-file`, `kithara-hls` зелёные.
