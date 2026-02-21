# Seek Refactoring Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate seek deadlocks and wrong-fragment bugs by making Timeline the sole seek coordinator (GStreamer FLUSH_START/STOP pattern).

**Architecture:** Replace channel-based `AudioCommand::Seek` with atomic `Timeline.initiate_seek()` / `complete_seek()`. Add `WaitOutcome::Interrupted` so `wait_range()` breaks out instantly on seek. Worker reads seek state from Timeline, not from cmd channel.

**Tech Stack:** Rust, `std::sync::atomic`, `parking_lot` (Condvar), `kanal` channels (non-seek commands only), `tokio` (downloader).

**Design doc:** `docs/plans/2026-02-21-seek-refactoring-design.md`

---

### Task 1: Add seek coordinator fields to Timeline

**Files:**
- Modify: `crates/kithara-stream/src/timeline.rs`

**Step 1: Write tests for new Timeline seek methods**

Add to existing `mod tests` at `timeline.rs:175`:

```rust
#[test]
fn initiate_seek_sets_flushing_and_target() {
    let tl = Timeline::new();
    assert!(!tl.is_flushing());
    assert!(tl.seek_target().is_none());

    let epoch = tl.initiate_seek(Duration::from_secs(10));
    assert_eq!(epoch, 1);
    assert!(tl.is_flushing());
    assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
    assert_eq!(tl.seek_epoch(), 1);
    assert_eq!(tl.committed_position(), Duration::from_secs(10));
}

#[test]
fn complete_seek_clears_flushing() {
    let tl = Timeline::new();
    let epoch = tl.initiate_seek(Duration::from_secs(5));
    tl.complete_seek(epoch);
    assert!(!tl.is_flushing());
    assert!(tl.seek_target().is_none());
}

#[test]
fn complete_seek_ignores_stale_epoch() {
    let tl = Timeline::new();
    let epoch1 = tl.initiate_seek(Duration::from_secs(5));
    let epoch2 = tl.initiate_seek(Duration::from_secs(10));
    // complete with old epoch should NOT clear flushing
    tl.complete_seek(epoch1);
    assert!(tl.is_flushing());
    assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
    // complete with current epoch clears
    tl.complete_seek(epoch2);
    assert!(!tl.is_flushing());
}

#[test]
fn seek_epoch_monotonically_increases() {
    let tl = Timeline::new();
    let e1 = tl.initiate_seek(Duration::from_secs(1));
    let e2 = tl.initiate_seek(Duration::from_secs(2));
    let e3 = tl.initiate_seek(Duration::from_secs(3));
    assert_eq!(e1, 1);
    assert_eq!(e2, 2);
    assert_eq!(e3, 3);
    // Only last target survives
    assert_eq!(tl.seek_target(), Some(Duration::from_secs(3)));
}

#[test]
fn initiate_seek_is_visible_across_clones() {
    let tl = Timeline::new();
    let clone = tl.clone();
    tl.initiate_seek(Duration::from_secs(7));
    assert!(clone.is_flushing());
    assert_eq!(clone.seek_target(), Some(Duration::from_secs(7)));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p kithara-stream timeline`
Expected: FAIL — methods `initiate_seek`, `complete_seek`, `is_flushing`, `seek_target`, `seek_epoch` do not exist.

**Step 3: Add fields and methods to Timeline**

Add three new fields to the `Timeline` struct (after line 22):

```rust
seek_epoch: Arc<AtomicU64>,
flushing: Arc<AtomicBool>,
seek_target_ns: Arc<AtomicU64>,
```

Add sentinel constant (next to existing ones at line 26-28):

```rust
const NO_SEEK_TARGET: u64 = u64::MAX;
```

Add to `new()` (after line 39):

```rust
seek_epoch: Arc::new(AtomicU64::new(0)),
flushing: Arc::new(AtomicBool::new(false)),
seek_target_ns: Arc::new(AtomicU64::new(Self::NO_SEEK_TARGET)),
```

Add methods after `clear_pending_seek_epoch` (after line 166):

```rust
/// Initiate a seek (FLUSH_START).
///
/// Sets flushing flag, records target position, increments epoch.
/// All blocking reads (`wait_range`) will observe `is_flushing()` and abort.
///
/// Returns the new seek epoch.
pub fn initiate_seek(&self, target: Duration) -> u64 {
    let nanos = u64::try_from(target.as_nanos()).unwrap_or(u64::MAX - 1);
    let epoch = self.seek_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    self.seek_target_ns.store(nanos, Ordering::Release);
    self.set_committed_position(target);
    // flushing must be set LAST so readers see target before flushing flag
    self.flushing.store(true, Ordering::Release);
    epoch
}

/// Complete a seek (FLUSH_STOP).
///
/// Clears flushing flag only if `epoch` is still current.
/// A superseding `initiate_seek` will have incremented the epoch,
/// preventing an older completion from clearing the new seek.
pub fn complete_seek(&self, epoch: u64) {
    let current = self.seek_epoch.load(Ordering::Acquire);
    if current == epoch {
        self.seek_target_ns.store(Self::NO_SEEK_TARGET, Ordering::Release);
        self.flushing.store(false, Ordering::Release);
    }
}

/// Check if the pipeline is being flushed (seek pending).
#[must_use]
pub fn is_flushing(&self) -> bool {
    self.flushing.load(Ordering::Acquire)
}

/// Read the pending seek target position.
#[must_use]
pub fn seek_target(&self) -> Option<Duration> {
    let ns = self.seek_target_ns.load(Ordering::Acquire);
    if ns == Self::NO_SEEK_TARGET {
        None
    } else {
        Some(Duration::from_nanos(ns))
    }
}

/// Read the current seek epoch.
#[must_use]
pub fn seek_epoch(&self) -> u64 {
    self.seek_epoch.load(Ordering::Acquire)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p kithara-stream timeline`
Expected: PASS

**Step 5: Commit**

```
feat(stream): add seek coordinator to Timeline (initiate_seek/complete_seek/is_flushing)
```

---

### Task 2: Add `WaitOutcome::Interrupted` variant

**Files:**
- Modify: `crates/kithara-storage/src/resource.rs:36-41`

**Step 1: Add the variant**

Add `Interrupted` variant to `WaitOutcome` enum at line 40:

```rust
pub enum WaitOutcome {
    Ready,
    Eof,
    /// A seek or flush interrupted the wait. The caller should abort the current
    /// read and check for pending seeks.
    Interrupted,
}
```

**Step 2: Run workspace build to find all match sites**

Run: `cargo build --workspace 2>&1 | head -40`
Expected: Compiler errors at all `match` on `WaitOutcome` that don't handle `Interrupted`.

**Step 3: Add `Interrupted` arms to all match sites**

Key locations to update:
- `crates/kithara-stream/src/stream.rs:183-185` — `Stream::read()` match
- `crates/kithara-file/src/session.rs` — `FileSource::wait_range` (returns Ready/Eof only, no change needed in impl, but callers may match)
- `crates/kithara-storage/` — any internal matches
- Test utilities

For `Stream::read()` at `stream.rs:183`:
```rust
match self.source.wait_range(range)? {
    WaitOutcome::Ready => {}
    WaitOutcome::Eof => return Ok(0),
    WaitOutcome::Interrupted => {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "seek pending",
        ));
    }
}
```

**Step 4: Run workspace build to verify**

Run: `cargo build --workspace`
Expected: PASS (all match arms handled)

**Step 5: Commit**

```
feat(storage): add WaitOutcome::Interrupted for seek-driven read abort
```

---

### Task 3: Add `DecodeError::Interrupted` variant

**Files:**
- Modify: `crates/kithara-decode/src/error.rs:13-38`

**Step 1: Add the variant**

Add after `ProbeFailed` (line 34):

```rust
/// A seek interrupted the decode operation. Not a real error —
/// the caller should check for pending seeks and retry.
#[error("Interrupted by seek")]
Interrupted,
```

**Step 2: Add conversion from `io::Error(Interrupted)` → `DecodeError::Interrupted`**

Modify the existing `From<io::Error>` impl. Currently it's derived via `#[from]`. We need a manual impl to special-case `Interrupted`:

Remove `#[from]` from the `Io` variant and add manual impl:

```rust
#[error("IO error: {0}")]
Io(io::Error),  // remove #[from]
```

```rust
impl From<io::Error> for DecodeError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::Interrupted {
            Self::Interrupted
        } else {
            Self::Io(err)
        }
    }
}
```

**Step 3: Add test**

```rust
#[test]
fn test_io_interrupted_becomes_decode_interrupted() {
    let io_err = io::Error::new(io::ErrorKind::Interrupted, "seek pending");
    let decode_err: DecodeError = io_err.into();
    assert!(matches!(decode_err, DecodeError::Interrupted));
}
```

**Step 4: Run tests**

Run: `cargo test -p kithara-decode error`
Expected: PASS

**Step 5: Commit**

```
feat(decode): add DecodeError::Interrupted for seek-driven decode abort
```

---

### Task 4: Make `HlsSource::wait_range()` flushing-aware

**Files:**
- Modify: `crates/kithara-hls/src/source.rs:263-415`

**Step 1: Add flushing check at loop entry and after condvar wakeup**

At the top of `wait_range()` loop body (after line 265 approximately), add:

```rust
// FLUSH_START check: abort immediately if a seek is pending.
if self.shared.timeline.is_flushing() {
    return Ok(WaitOutcome::Interrupted);
}
```

After each `condvar.wait_for()` call, add the same check:

```rust
self.shared.condvar.wait_for(&mut segments, Duration::from_millis(50));

// FLUSH_START check after wakeup.
if self.shared.timeline.is_flushing() {
    return Ok(WaitOutcome::Interrupted);
}
```

**Step 2: Also check in `read_at()` before doing work**

At the top of `read_at()` (line ~417):

```rust
if self.shared.timeline.is_flushing() {
    return Ok(0);
}
```

**Step 3: Run HLS tests**

Run: `cargo test -p kithara-hls`
Expected: PASS (existing tests don't trigger flushing)

**Step 4: Commit**

```
feat(hls): make wait_range/read_at interruptible via Timeline.is_flushing
```

---

### Task 5: Migrate `seek_epoch` from `SharedSegments` to Timeline

**Files:**
- Modify: `crates/kithara-hls/src/source.rs` — remove `seek_epoch: AtomicU64` from `SharedSegments`, use `timeline.seek_epoch()`
- Modify: `crates/kithara-hls/src/downloader.rs` — update `is_stale_epoch()` and all `seek_epoch` reads

**Step 1: Remove `seek_epoch` field from `SharedSegments`**

At `source.rs:67`, remove `pub(crate) seek_epoch: AtomicU64`.

Replace all `self.shared.seek_epoch.load(...)` with `self.shared.timeline.seek_epoch()`.
Replace all `self.shared.seek_epoch.store(...)` with nothing (Timeline's `initiate_seek` handles this).

**Step 2: Update `set_seek_epoch()` in `HlsSource`**

In `set_seek_epoch()` at line 523-537: remove the `seek_epoch.store()` line (epoch is now set by `Timeline::initiate_seek`). Keep the segment clearing, EOF reset, request drain, and notifications.

**Step 3: Update downloader**

In `downloader.rs`, `is_stale_epoch()` (line ~34): change from reading `shared.seek_epoch` to `shared.timeline.seek_epoch()`.

In `SegmentRequest` struct and wherever `seek_epoch` is pushed to the request queue: read from `self.shared.timeline.seek_epoch()`.

**Step 4: Run tests**

Run: `cargo test -p kithara-hls`
Expected: PASS

**Step 5: Commit**

```
refactor(hls): migrate seek_epoch from SharedSegments to Timeline
```

---

### Task 6: Remove `AudioCommand::Seek` — seek via Timeline only

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/audio.rs:16-19,124-151,326-412`
- Modify: `crates/kithara-audio/src/pipeline/worker.rs:16-19,33-50,102-180`
- Modify: `crates/kithara-audio/src/pipeline/stream_source_core.rs:728-837`

**Step 1: Rewrite `Audio::seek()` — pure atomic, no channel**

Replace `seek()` (line 326-371) and `seek_with_epoch()` (line 380-412) with a single method:

```rust
pub fn seek(&mut self, position: Duration) -> DecodeResult<()> {
    // FLUSH_START: atomic write to Timeline
    let epoch = self.timeline.initiate_seek(position);

    // Wake wait_range() condvar if sleeping
    // (condvar is notified via SharedSegments, need to notify it)
    // The condvar.notify_all() should be triggered —
    // we can use the shared condvar reference if available,
    // or rely on the 50ms timeout + is_flushing check.

    // Update local consumer state
    self.validator.epoch = epoch;
    self.current_chunk = None;
    self.chunk_offset = 0;
    self.eof = false;

    // Drain stale chunks from pcm channel
    while let Ok(Some(_)) = self.pcm_rx.try_recv() {}

    self.preloaded = false;

    debug!(?position, epoch, "seek initiated via Timeline");
    Ok(())
}
```

Remove `seek_with_epoch()` entirely. Remove `send_seek_command()` entirely.

**Step 2: Remove `AudioCommand::Seek` from enum**

At `worker.rs:16-19` (or `audio.rs:16-19`), remove the `Seek` variant.

**Step 3: Add `apply_pending_seek()` to `StreamAudioSource`**

In `stream_source_core.rs`, add a new method that reads seek state from Timeline:

```rust
fn apply_pending_seek(&mut self) {
    let timeline = self.shared_stream.timeline();
    let Some(target) = timeline.seek_target() else {
        // Flushing flag set but no target — shouldn't happen, clear it
        timeline.complete_seek(timeline.seek_epoch());
        return;
    };
    let epoch = timeline.seek_epoch();

    // Reuse existing seek logic from handle_command's Seek branch
    // (set_seek_epoch, clear_variant_fence, seek_time_anchor, etc.)
    // but read position/epoch from Timeline instead of command.

    self.handle_seek(target, epoch);

    // FLUSH_STOP
    timeline.complete_seek(epoch);
}
```

Extract the seek logic from `handle_command`'s `AudioCommand::Seek` branch into a shared `handle_seek(position, epoch)` method.

**Step 4: Update worker main loop**

In `run_audio_loop()` at `worker.rs:102`, add flushing check:

```rust
loop {
    if cancel.is_cancelled() { break; }

    // 1. Check seek via Timeline (single source of truth)
    if source.timeline().is_flushing() {
        source.apply_pending_seek();
        continue;
    }

    // 2. Drain non-seek commands
    drain_commands(&mut source, &cmd_rx);

    // 3. Fetch + send
    match source.fetch_next() {
        Ok(Some(fetch)) => {
            match send_with_backpressure(&mut source, &cmd_rx, &data_tx, &cancel, fetch) {
                Ok(true) => {}
                Ok(false) => continue,  // flushing or command interrupted
                Err(()) => break,
            }
        }
        // ... existing EOF/error handling ...
    }
}
```

**Step 5: Update `send_with_backpressure` — check flushing**

Add at top of retry loop:

```rust
if source.timeline().is_flushing() {
    return Ok(false);
}
```

**Step 6: Remove Seek from `drain_commands` and `handle_command`**

`drain_commands` no longer needs to handle Seek commands. Remove the `AudioCommand::Seek` match arm from `handle_command`.

**Step 7: Add `timeline()` to `AudioWorkerSource` trait**

At `worker.rs:22-28`:

```rust
pub(crate) trait AudioWorkerSource {
    type Chunk: Send;
    type Command: Send;

    fn timeline(&self) -> &Timeline;  // NEW
    fn fetch_next(&mut self) -> ...;
    fn handle_command(&mut self, cmd: Self::Command);
}
```

Implement for `StreamAudioSource`.

**Step 8: Run tests**

Run: `cargo test -p kithara-audio`
Expected: PASS (update any tests that send AudioCommand::Seek)

**Step 9: Commit**

```
feat(audio): remove AudioCommand::Seek, seek flows entirely through Timeline
```

---

### Task 7: Make downloader flushing-aware

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs:1009-1022`

**Step 1: Update `should_throttle()`**

At line 1009:

```rust
fn should_throttle(&self) -> bool {
    // Never throttle during seek — downloader must be free to respond
    if self.shared.timeline.is_flushing() {
        return false;
    }
    let Some(limit) = self.look_ahead_bytes else {
        return false;
    };
    let reader_pos = self.shared.timeline.byte_position();
    let downloaded = self.shared.segments.lock().max_end_offset();
    downloaded.saturating_sub(reader_pos) > limit
}
```

**Step 2: Update `wait_ready()`**

At line 1020, replace simple `notified().await` with flushing-aware wait:

```rust
async fn wait_ready(&self) {
    loop {
        tokio::select! {
            _ = self.shared.reader_advanced.notified() => {
                return;
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // Check flushing periodically as a safety net
                if self.shared.timeline.is_flushing() || !self.should_throttle() {
                    return;
                }
            }
        }
    }
}
```

**Step 3: Add flushing check in main download loop**

In `poll_demand()` (line ~717), at the top of the method:

```rust
if self.shared.timeline.is_flushing() {
    // Wait for seek to complete before planning next download
    while self.shared.timeline.is_flushing() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if self.cancel.is_cancelled() {
            return None;
        }
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p kithara-hls`
Expected: PASS

**Step 5: Commit**

```
feat(hls): make downloader flushing-aware (unblock throttle on seek)
```

---

### Task 8: Remove `PcmReader::seek_with_epoch` and upstream seek_with_epoch chain

**Files:**
- Modify: `crates/kithara-audio/src/traits.rs:70-72` — remove `seek_with_epoch()` default method
- Modify: `crates/kithara-play/src/impls/player_resource.rs:173-184` — simplify to `seek()` only
- Modify: `crates/kithara-play/src/impls/player_track.rs:313-321` — simplify to `seek()` only
- Modify: `crates/kithara-play/src/impls/resource.rs:199-201` — remove `seek_with_epoch()`
- Modify: `crates/kithara-play/src/impls/player_processor.rs` — `apply_seek()` calls `seek()` not `seek_with_epoch()`

**Step 1: Remove `seek_with_epoch` from PcmReader trait**

At `traits.rs:70-72`, remove the method entirely. Seek epoch now lives in Timeline — callers just call `seek()` and Timeline handles the epoch.

**Step 2: Simplify PlayerTrack and PlayerResource**

`PlayerTrack::seek_with_epoch()` becomes `PlayerTrack::seek()`:
```rust
pub(crate) fn seek(&mut self, seconds: f64) {
    if let Some(mut resource) = self.resource.try_lock() {
        resource.seek(seconds);
    }
}
```

`PlayerProcessor::apply_seek()` calls `track.seek(seconds)` instead of `track.seek_with_epoch(seconds, seek_epoch)`.

Remove `seek_with_epoch` from `PlayerResource` and `Resource`.

**Step 3: Run tests**

Run: `cargo test -p kithara-play`
Expected: PASS (update tests that used seek_with_epoch)

**Step 4: Commit**

```
refactor(play): remove seek_with_epoch chain, seek epoch managed by Timeline
```

---

### Task 9: Notify condvar from Audio::seek()

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/audio.rs`
- Possibly: `crates/kithara-hls/src/source.rs` — expose condvar notify

**Step 1: Ensure condvar is woken on seek**

`Audio::seek()` sets `timeline.flushing = true`, but `wait_range()` is sleeping on a condvar with 50ms timeout. To get instant wakeup, we need to notify the condvar.

Options:
a) Add `condvar: Arc<Condvar>` reference to `Audio` struct (passed from HLS source setup)
b) Rely on 50ms timeout (simpler but slower seek response)
c) Add `notify_flush` method to `Source` trait

Option (c) is cleanest. Add to `Source` trait:

```rust
/// Wake any blocked `wait_range()` calls.
/// Called after `Timeline::initiate_seek()` to ensure immediate response.
fn notify_waiting(&self) {}
```

`HlsSource` implements it to call `self.shared.condvar.notify_all()`.

Then `Audio::seek()` stores a notify callback or the stream directly calls it.

Since `Audio` holds `SharedStream<T>` (which wraps `Stream<T>` in Mutex), we need an approach that doesn't require locking. Best approach: store `Arc<Condvar>` (or `Arc<Notify>`) separately in `Audio` struct, passed during construction.

**Step 2: Run tests**

Run: `cargo test --workspace`
Expected: PASS

**Step 3: Commit**

```
feat(audio): notify condvar on seek for instant wait_range wakeup
```

---

### Task 10: Integration test — seek during active playback

**Files:**
- Create: `tests/tests/kithara_hls/seek_flush_test.rs` or add to existing integration test

**Step 1: Write test**

Test scenario:
1. Start HLS playback
2. Read some chunks (establish normal pipeline flow)
3. Seek to a different position
4. Verify: no hang (timeout-guarded), correct position after seek, new chunks arrive

```rust
#[test]
fn seek_during_playback_does_not_hang() {
    // Setup HLS source + decoder
    // Read 5 chunks
    // Seek to position 30s
    // Read 5 more chunks within 5s timeout
    // Verify position >= 30s
}
```

**Step 2: Run test**

Run: `cargo test -p kithara seek_flush --timeout 30`
Expected: PASS (no hang, correct position)

**Step 3: Commit**

```
test: add seek-during-playback integration test (deadlock regression)
```

---

### Task 11: Cleanup and final verification

**Files:**
- All modified files

**Step 1: Run full workspace checks**

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
bash scripts/ci/lint-style.sh
```

**Step 2: Fix any issues**

**Step 3: Final commit**

```
chore: cleanup after seek refactoring
```

---

## Execution Order and Dependencies

```
Task 1: Timeline seek fields          ← foundation, no deps
Task 2: WaitOutcome::Interrupted       ← foundation, no deps
Task 3: DecodeError::Interrupted       ← foundation, no deps
  ↓ (Tasks 1-3 can run in parallel)
Task 4: wait_range flushing-aware      ← depends on 1, 2
Task 5: seek_epoch migration           ← depends on 1
  ↓
Task 6: Remove AudioCommand::Seek      ← depends on 1, 3, 4, 5 (core change)
Task 7: Downloader flushing-aware      ← depends on 1, 5
Task 8: Remove seek_with_epoch chain   ← depends on 6
Task 9: Condvar notification           ← depends on 6
  ↓
Task 10: Integration test              ← depends on all above
Task 11: Cleanup                       ← depends on all above
```

## Risk Mitigation

- **Each task is independently compilable** — workspace builds after each commit
- **Tasks 1-3 are pure additions** — no existing code breaks
- **Task 6 is the riskiest** — largest change, touches 3 files. Do it incrementally: extract `handle_seek()` first, then remove `AudioCommand::Seek`, then update worker loop.
- **WASM path**: `Audio::seek()` is also called from WASM. Since we removed channel send, WASM gets the same non-blocking behavior for free.
