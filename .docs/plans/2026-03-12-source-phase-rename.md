# Source Phase Rename + Parameterless `phase()` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Rename `Source::phase(range)` → `phase_at(range)`, add parameterless `phase() -> SourcePhase` that uses source-internal knowledge to determine readiness, and simplify Audio worker's `is_ready()`.

**Architecture:** Two-step rename (trait + impls + call sites), then add new default method `phase()` that delegates to `phase_at(pos..pos+1)`. HLS and File override with chunk-aware logic. Audio worker's `is_ready()` simplified to use `phase()` via `SharedStream`.

**Tech Stack:** Rust, kithara-stream (trait), kithara-hls, kithara-file, kithara-audio, kithara-test-utils

---

## Affected files inventory

### Trait definition
- `crates/kithara-stream/src/source.rs:118` — `fn phase(&self, range) -> SourcePhase` (required method)
- `crates/kithara-stream/src/source.rs:83` — unimock attribute generates `SourceMock`

### Implementations (5 production + 5 test)
- `crates/kithara-hls/src/source.rs:778` — `HlsSource::phase()`
- `crates/kithara-file/src/session.rs:275` — `FileSource::phase()`
- `crates/kithara-test-utils/src/memory_source.rs:75` — `MemorySource::phase()`
- `crates/kithara-stream/src/stream.rs:336` — `ScriptSource::phase()` (test)
- `crates/kithara-stream/src/source.rs:258` — `StubSource::phase()` (test)
- `tests/tests/kithara_audio/stream_source_tests.rs:169` — `TestSource::phase()`
- `tests/tests/kithara_stream/timeline_source_of_truth.rs:55` — `TimelineSource::phase()`
- `tests/tests/kithara_stream/reader_seek_overflow.rs:103` — `OverflowSource::phase()`
- `tests/fuzz/fuzz_targets/stream_read_seek.rs:144` — `FuzzSource::phase()`

### Call sites (not in impl blocks)
- `crates/kithara-file/src/session.rs:230` — `self.phase(range.clone())` in `wait_range`
- `crates/kithara-hls/src/source.rs:1318,1326,1336,1344,1352,1359` — 6 test assertions
- `crates/kithara-file/src/session.rs:530,547,564,576` — 4 test assertions

### Proxy layer (Stream<T> и SharedStream)
- `crates/kithara-stream/src/stream.rs:108-130` — delegate block (phase NOT currently proxied)
- `crates/kithara-audio/src/pipeline/source.rs:41-94` — SharedStream methods
- `crates/kithara-audio/src/pipeline/source.rs:1152-1171` — `is_ready()` to simplify

---

## Task 1: Rename `phase(range)` → `phase_at(range)` in trait and all impls

**Files:**
- Modify: `crates/kithara-stream/src/source.rs:118` — trait definition
- Modify: `crates/kithara-hls/src/source.rs:778` — HlsSource impl
- Modify: `crates/kithara-file/src/session.rs:275` — FileSource impl
- Modify: `crates/kithara-test-utils/src/memory_source.rs:75` — MemorySource impl
- Modify: `crates/kithara-stream/src/stream.rs:336` — ScriptSource (test)
- Modify: `crates/kithara-stream/src/source.rs:258` — StubSource (test)
- Modify: `tests/tests/kithara_audio/stream_source_tests.rs:169` — TestSource
- Modify: `tests/tests/kithara_stream/timeline_source_of_truth.rs:55` — TimelineSource
- Modify: `tests/tests/kithara_stream/reader_seek_overflow.rs:103` — OverflowSource
- Modify: `tests/fuzz/fuzz_targets/stream_read_seek.rs:144` — FuzzSource

**Step 1: Rename in trait definition**

В `crates/kithara-stream/src/source.rs:118`:
```rust
// Было:
fn phase(&self, range: Range<u64>) -> SourcePhase;

// Стало:
fn phase_at(&self, range: Range<u64>) -> SourcePhase;
```

**Step 2: Rename all `fn phase(` → `fn phase_at(` in impl blocks**

Mechanical rename в 9 файлах (все impl Source for ... { fn phase( → fn phase_at().

**Step 3: Rename all call sites**

- `crates/kithara-file/src/session.rs:230` — `self.phase(range.clone())` → `self.phase_at(range.clone())`
- `crates/kithara-hls/src/source.rs` — 6 тестов: `source.phase(...)` → `source.phase_at(...)`
- `crates/kithara-file/src/session.rs` — 4 теста: `source.phase(...)` → `source.phase_at(...)`

**Step 4: Build and test**

```bash
cargo build --workspace
cargo test -p kithara-stream -p kithara-hls -p kithara-file -p kithara-test-utils
cargo test -p kithara-integration-tests --test suite_light -- kithara_stream
```

Expected: all pass — это mechanical rename, zero behaviour change.

**Step 5: Commit**

```
refactor: rename Source::phase(range) to phase_at(range)

Prepares for adding parameterless phase() method that uses
source-internal knowledge to determine readiness.
```

---

## Task 2: Add parameterless `phase() -> SourcePhase` default method

**Files:**
- Modify: `crates/kithara-stream/src/source.rs` — add default method after `phase_at`

**Step 1: Add method to trait**

В `crates/kithara-stream/src/source.rs`, после `phase_at`:
```rust
/// Overall source readiness at the current timeline position.
///
/// Uses the source's internal knowledge of chunk/segment boundaries
/// to determine if the next read operation can proceed without blocking.
///
/// Unlike `phase_at(range)` which checks a specific byte range,
/// this method lets the source decide the appropriate granularity.
///
/// Default checks a single byte at the current position.
/// HLS overrides with segment-aware logic, File with 32KB-window logic.
fn phase(&self) -> SourcePhase {
    let pos = self.timeline().byte_position();
    self.phase_at(pos..pos.saturating_add(1))
}
```

**Step 2: Add test for default behaviour**

В `crates/kithara-stream/src/source.rs` tests module, после `is_range_ready_default_empty_returns_true`:
```rust
#[kithara::test]
fn phase_default_delegates_to_phase_at() {
    // StubSource already implements phase_at returning Waiting.
    // Default phase() should delegate to phase_at(pos..pos+1).
    let source = StubSource;
    assert_eq!(source.phase(), SourcePhase::Waiting);
}
```

**Step 3: Build and test**

```bash
cargo test -p kithara-stream
```

Expected: pass.

**Step 4: Commit**

```
feat: add parameterless Source::phase() for position-aware readiness
```

---

## Task 3: Override `phase()` in HlsSource with segment-aware logic

**Files:**
- Modify: `crates/kithara-hls/src/source.rs` — add `fn phase()` override in `impl Source for HlsSource`

**Step 1: Implement override**

В `impl Source for HlsSource`, после `phase_at`:
```rust
fn phase(&self) -> SourcePhase {
    use kithara_stream::SourcePhase;
    let segments = self.shared.segments.lock_sync();
    if self.shared.cancel.is_cancelled() || self.shared.stopped.load(Ordering::Acquire) {
        return SourcePhase::Cancelled;
    }
    // Check if the segment at current position has data ready.
    let pos = self.shared.timeline.byte_position();
    if let Some(seg) = segments.find_at_offset(pos) {
        if self.range_ready_from_segments(&segments, &(pos..seg.end_offset())) {
            return SourcePhase::Ready;
        }
    }
    if self.shared.timeline.is_flushing() {
        return SourcePhase::Seeking;
    }
    if self.is_past_eof(&segments, &(pos..pos.saturating_add(1))) {
        return SourcePhase::Eof;
    }
    SourcePhase::Waiting
}
```

Ключевое отличие от `phase_at(range)`: проверяем готовность **всего сегмента** в котором находится позиция, а не произвольного range.

**Step 2: Add test**

```rust
#[kithara::test]
fn hls_phase_parameterless_ready_when_segment_loaded() {
    let source = build_phase_test_source(1);
    push_segment(&source.shared, 0, 0, 0, 100);

    // Write actual resource data.
    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = source.fetch.backend().open_resource(&key).unwrap();
    res.write_at(0, &[0u8; 100]).unwrap();
    res.commit(Some(100)).unwrap();

    // Position at start — segment 0 fully loaded → Ready.
    assert_eq!(source.phase(), kithara_stream::SourcePhase::Ready);
}

#[kithara::test]
fn hls_phase_parameterless_waiting_when_no_segments() {
    let source = build_phase_test_source(1);
    assert_eq!(source.phase(), kithara_stream::SourcePhase::Waiting);
}
```

**Step 3: Build and test**

```bash
cargo test -p kithara-hls -- phase
```

**Step 4: Commit**

```
feat(hls): override Source::phase() with segment-aware readiness
```

---

## Task 4: Override `phase()` in FileSource with 32KB-window logic

**Files:**
- Modify: `crates/kithara-file/src/session.rs` — add `fn phase()` override in `impl Source for FileSource`

**Step 1: Implement override**

В `impl Source for FileSource`, после `phase_at`:
```rust
fn phase(&self) -> SourcePhase {
    use kithara_stream::SourcePhase;
    let timeline = self.progress.timeline();
    let pos = timeline.byte_position();
    let total = timeline.total_bytes();

    // Data ready: check 32KB window (matches Symphonia's buffer size),
    // clamped to known length to avoid false negatives near EOF.
    let check_end = total.map_or(pos.saturating_add(32 * 1024), |len| {
        pos.saturating_add(32 * 1024).min(len)
    });
    if self.res.contains_range(pos..check_end) {
        return SourcePhase::Ready;
    }
    if timeline.is_flushing() {
        return SourcePhase::Seeking;
    }
    if total.is_some_and(|t| t > 0 && pos >= t) {
        return SourcePhase::Eof;
    }
    SourcePhase::Waiting
}
```

**Step 2: Add test**

```rust
#[kithara::test]
fn file_source_phase_parameterless_ready_when_data_available() {
    let data = vec![0u8; 64 * 1024]; // 64KB — covers 32KB window
    let res = create_committed_resource(&data);
    let progress = Arc::new(Progress::new(Timeline::new()));
    let bus = EventBus::new(16);
    progress.timeline().set_total_bytes(Some(data.len() as u64));
    let source = FileSource::new(res, progress, bus);

    assert_eq!(source.phase(), kithara_stream::SourcePhase::Ready);
}

#[kithara::test]
fn file_source_phase_parameterless_eof_at_end() {
    let data = b"abc";
    let res = create_committed_resource(data);
    let progress = Arc::new(Progress::new(Timeline::new()));
    let bus = EventBus::new(16);
    progress.timeline().set_total_bytes(Some(data.len() as u64));
    progress.set_read_pos(3); // at EOF
    let source = FileSource::new(res, progress, bus);

    assert_eq!(source.phase(), kithara_stream::SourcePhase::Eof);
}
```

**Step 3: Build and test**

```bash
cargo test -p kithara-file -- phase
```

**Step 4: Commit**

```
feat(file): override Source::phase() with 32KB-window readiness
```

---

## Task 5: Proxy `phase()` through `Stream<T>` and `SharedStream`

**Files:**
- Modify: `crates/kithara-stream/src/stream.rs:108-130` — add `phase` to delegate block
- Modify: `crates/kithara-audio/src/pipeline/source.rs:41-94` — add `phase()` to SharedStream

**Step 1: Add to Stream<T> delegate block**

В `crates/kithara-stream/src/stream.rs`, добавить в `delegate::delegate!`:
```rust
/// Overall source readiness at current position.
pub fn phase(&self) -> SourcePhase;
```

Также добавить `SourcePhase` в use-блок файла (если не импортирован через `use crate::...`).

**Step 2: Add to SharedStream**

В `crates/kithara-audio/src/pipeline/source.rs`, в `impl<T: StreamType> SharedStream<T>`:
```rust
fn phase(&self) -> kithara_stream::SourcePhase {
    self.inner.lock_sync().phase()
}
```

**Step 3: Build**

```bash
cargo check --workspace
```

**Step 4: Commit**

```
feat: proxy Source::phase() through Stream<T> and SharedStream
```

---

## Task 6: Simplify Audio worker `is_ready()` to use `phase()`

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/source.rs:1152-1171` — simplify `is_ready()`

**Step 1: Replace `is_ready()` implementation**

```rust
fn is_ready(&self) -> bool {
    use kithara_stream::SourcePhase;
    match self.shared_stream.phase() {
        SourcePhase::Ready | SourcePhase::Eof => true,
        SourcePhase::Seeking => true, // worker handles seek separately
        _ => false,
    }
}
```

Это удаляет:
- `current_segment_range()` call
- `pos + 32KB` heuristic
- `len().min()` clamping
- `is_range_ready()` call

Вся эта логика теперь живёт в `HlsSource::phase()` и `FileSource::phase()`.

**Step 2: Remove unused imports/methods from SharedStream**

Проверить, используются ли `current_segment_range()` и `is_range_ready()` ещё где-то в `SharedStream`. Если нет — удалить proxy-методы.

**Step 3: Run full test suite**

```bash
cargo test -p kithara-audio
cargo test -p kithara-integration-tests --test suite_light
```

**Step 4: Commit**

```
refactor(audio): simplify is_ready() to use Source::phase()

Removes source-specific range computation from audio worker.
Each source now determines its own readiness granularity:
- HLS: segment-aware (full segment loaded)
- File: 32KB-window (matches Symphonia buffer)
- Memory: single byte (always ready if data exists)
```

---

## Task 7: Cleanup — remove `is_range_ready` proxy from SharedStream if unused

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/source.rs` — potentially remove `is_range_ready`, `current_segment_range` from SharedStream

**Step 1: Grep for remaining usage**

```bash
# Check if is_range_ready or current_segment_range are used anywhere in kithara-audio
# outside of the deleted is_ready() code
```

Если не используются — удалить proxy-методы из SharedStream. НЕ удалять из `Stream<T>` delegate block (публичный API).

**Step 2: Build and test**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

**Step 3: Commit**

```
refactor(audio): remove unused SharedStream proxy methods
```

---

## Summary

| Task | Описание | Risk |
|------|----------|------|
| 1 | Rename `phase()` → `phase_at()` (mechanical) | Low — pure rename |
| 2 | Add default `phase()` → delegates to `phase_at(pos..pos+1)` | None — additive |
| 3 | HLS override: segment-aware `phase()` | Low — moves existing logic |
| 4 | File override: 32KB-window `phase()` | Low — moves existing logic |
| 5 | Proxy through Stream<T> / SharedStream | None — additive |
| 6 | Simplify audio `is_ready()` | Medium — behaviour change, needs test validation |
| 7 | Cleanup unused proxy methods | Low — dead code removal |
