# Seek Refactoring Design: Timeline as Seek Coordinator

**Date**: 2026-02-21
**Status**: Approved
**Based on**: `docs/plans/2026-02-20-seek-architecture-comparison.md`
**Reference architecture**: GStreamer adaptivedemux2 (FLUSH_START/STOP pattern)

---

## Problem Statement

HLS seek has two critical bugs:

1. **Deadlock (hang)**: Worker thread stuck in `wait_range()` (condvar wait) while seek command sits unprocessed in `cmd_rx`. Downloader throttled by `should_throttle()` waiting for reader to advance. Circular dependency — nobody progresses.

2. **Deadlock (audio callback)**: `Audio::seek_with_epoch()` called from audio callback thread does blocking `cmd_tx.send()`. If cmd channel full and worker blocked in `send_with_backpressure()` (pcm channel full), both threads park permanently.

3. **Wrong audio fragment on seek**: Seek to different positions plays the same segment. Root cause: Symphonia operates on virtual byte stream where segment mapping can become stale.

## Design Principle

**Timeline is the single source of truth — no compromises.**

All seek coordination flows through Timeline atomics. No channels, no mutexes, no dual paths. Every thread reads seek state from Timeline. Every blocking point checks Timeline for interruption.

## Architecture: GStreamer-inspired Flush Protocol

Adapted from GStreamer's two-phase flush (FLUSH_START / FLUSH_STOP) to kithara's lock-free Timeline model.

### Timeline Extension

```rust
// Timeline — new seek coordinator fields
struct TimelineInner {
    // Existing fields:
    byte_position: AtomicU64,
    committed_position_ns: AtomicU64,
    download_position: AtomicU64,
    eof: AtomicBool,
    total_bytes: AtomicU64,
    total_duration_ns: AtomicU64,
    pending_seek_epoch: AtomicU64,

    // NEW seek coordinator fields:
    seek_epoch: AtomicU64,       // monotonically increasing seek counter
    flushing: AtomicBool,        // pipeline interrupted, seek pending
    seek_target_ns: AtomicU64,   // target position (SENTINEL = no seek)
}
```

### New Timeline Methods

```rust
impl Timeline {
    /// FLUSH_START: called by seek initiator (Audio::seek on any thread).
    /// Sets flushing=true, records target, increments epoch.
    /// All wait_range() and blocking reads will see this and break out.
    fn initiate_seek(&self, target: Duration) -> u64 {
        let epoch = self.inner.seek_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.seek_target_ns.store(target.as_nanos() as u64, Ordering::Release);
        self.inner.flushing.store(true, Ordering::Release);
        self.set_committed_position(target);
        epoch
    }

    /// FLUSH_STOP: called by worker after processing the seek.
    /// Clears flushing (CAS to handle superseding seeks).
    fn complete_seek(&self, epoch: u64) {
        // Only clear if this epoch is still current
        let current = self.inner.seek_epoch.load(Ordering::Acquire);
        if current == epoch {
            self.inner.seek_target_ns.store(SENTINEL, Ordering::Release);
            self.inner.flushing.store(false, Ordering::Release);
        }
    }

    /// Check if pipeline is being flushed (for wait_range, read_at, etc.)
    fn is_flushing(&self) -> bool {
        self.inner.flushing.load(Ordering::Acquire)
    }

    /// Read seek target (for worker to apply the seek)
    fn seek_target(&self) -> Option<Duration> {
        let ns = self.inner.seek_target_ns.load(Ordering::Acquire);
        if ns == SENTINEL { None } else { Some(Duration::from_nanos(ns)) }
    }

    /// Read current seek epoch
    fn seek_epoch(&self) -> u64 {
        self.inner.seek_epoch.load(Ordering::Acquire)
    }
}
```

### AudioCommand::Seek Removed

```rust
// BEFORE:
enum AudioCommand {
    Seek { position: Duration, epoch: u64 },
    // ...
}

// AFTER:
enum AudioCommand {
    // Seek is REMOVED — flows entirely through Timeline
    // ... other commands (Stop, etc.) remain
}
```

### Audio::seek() — Never Blocks

```rust
fn seek(&mut self, position: Duration) -> DecodeResult<()> {
    // 1. Atomic write to Timeline — FLUSH_START
    let epoch = self.timeline.initiate_seek(position);

    // 2. Wake wait_range() if sleeping on condvar
    self.notify_condvar();

    // 3. Update local consumer state
    self.validator.epoch = epoch;
    self.current_chunk = None;
    self.chunk_offset = 0;
    self.eof = false;

    // 4. Drain stale chunks from pcm channel
    while let Ok(Some(_)) = self.pcm_rx.try_recv() {}

    self.preloaded = false;
    Ok(())
}
// NO channel send. NO blocking. Pure atomic writes.
```

### WaitOutcome::Interrupted

```rust
enum WaitOutcome {
    Ready,
    Eof,
    Interrupted,  // NEW: seek in progress, abort read
}
```

### HlsSource::wait_range() — Interruptible

```rust
fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome> {
    loop {
        // Check flushing BEFORE any blocking
        if self.shared.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }

        let mut segments = self.shared.segments.lock();

        if segments.is_range_loaded(&range) {
            return Ok(WaitOutcome::Ready);
        }

        // ... on-demand segment request logic ...

        // Condvar wait with 50ms timeout
        self.shared.condvar.wait_for(&mut segments, Duration::from_millis(50));

        // Check flushing AFTER wakeup
        if self.shared.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }
    }
}
```

### Stream::read() — Propagates Interrupted

```rust
impl<T: StreamType> Read for Stream<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.timeline.byte_position();
        let range = pos..pos.saturating_add(buf.len() as u64);

        match self.source.wait_range(range)? {
            WaitOutcome::Ready => {}
            WaitOutcome::Eof => return Ok(0),
            WaitOutcome::Interrupted => {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "seek pending"));
            }
        }

        // Also check before actual read
        if self.source.timeline().is_flushing() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "seek pending"));
        }

        let n = self.source.read_at(pos, buf)?;
        self.timeline.set_byte_position(pos + n as u64);
        Ok(n)
    }
}
```

### Error Propagation Through Symphonia

```
HlsSource::wait_range() → WaitOutcome::Interrupted
  → Stream::read() → io::Error(Interrupted)
    → Symphonia IsoMp4Reader → symphonia::Error::IoError
      → Decoder::next_chunk() → DecodeError::Interrupted (new variant)
        → StreamAudioSource::fetch_next() → Err(Interrupted)
          → Worker main loop → continue → check is_flushing()
```

`DecodeError::Interrupted` is NOT a decode error — it does not trigger format change detection or error recovery.

### Worker Main Loop — Timeline-Driven

```rust
fn run_audio_loop(source, cmd_rx, data_tx, cancel) {
    loop {
        if cancel.is_cancelled() { break; }

        // 1. Seek check via Timeline (single source of truth)
        if source.timeline().is_flushing() {
            source.apply_pending_seek();  // reads target/epoch from Timeline
            continue;
        }

        // 2. Drain non-seek commands
        drain_commands(&mut source, &cmd_rx);

        // 3. Fetch + send
        match source.fetch_next() {
            Ok(Some(chunk)) => {
                if !send_with_backpressure(source, data_tx, chunk) {
                    continue;  // flushing detected during backpressure
                }
            }
            Ok(None) => idle_until_wakeup(cmd_rx, cancel, source.timeline()),
            Err(DecodeError::Interrupted) => continue,
            Err(e) => handle_decode_error(e),
        }
    }
}
```

### send_with_backpressure — Flushing-Aware

```rust
fn send_with_backpressure(source, data_tx, chunk) -> bool {
    loop {
        if source.timeline().is_flushing() {
            return false;  // drop chunk, return to main loop
        }
        match data_tx.try_send(chunk) {
            Ok(()) => return true,
            Err(_) => thread::sleep(BACKOFF),
        }
    }
}
```

### Downloader — Flushing-Aware

```rust
// should_throttle() respects flushing
fn should_throttle(&self) -> bool {
    if self.shared.timeline.is_flushing() {
        return false;  // don't throttle during seek
    }
    // ... existing throttle logic ...
}

// wait_ready() breaks out on flushing
async fn wait_ready_or_flushing(&self) {
    loop {
        tokio::select! {
            _ = self.shared.reader_advanced.notified() => {
                if !self.should_throttle() { return; }
            }
            _ = poll_flushing(&self.shared.timeline) => return,
        }
    }
}

// Epoch check before commit (existing, reinforced)
fn commit_segment(&self, epoch: u64) {
    if epoch != self.shared.timeline.seek_epoch() {
        return;  // stale, discard
    }
    // ... commit logic ...
}
```

### seek_epoch Migration

`seek_epoch` moves from `SharedSegments` to `Timeline`. All reads/writes go through `timeline.seek_epoch()`. This eliminates the split state where seek epoch lived in a separate atomic on `SharedSegments`.

## What Gets Removed

- `AudioCommand::Seek` variant
- `Audio::send_seek_command()` method
- `Audio::seek_with_epoch()` — merged into `Audio::seek()` (epoch comes from Timeline)
- `seek_epoch` field from `SharedSegments`
- Blocking `cmd_tx.send()` in seek path
- `PcmReader::seek_with_epoch()` trait method (seek is Timeline-driven, not method-driven)

## What Gets Added

- `Timeline`: `seek_epoch`, `flushing`, `seek_target_ns` fields
- `Timeline`: `initiate_seek()`, `complete_seek()`, `is_flushing()`, `seek_target()` methods
- `WaitOutcome::Interrupted` variant
- `DecodeError::Interrupted` variant
- `StreamAudioSource::apply_pending_seek()` method
- `is_flushing()` checks in: `wait_range()`, `Stream::read()`, `read_at()`, `send_with_backpressure()`, `should_throttle()`

## What Stays

- `cmd_tx`/`cmd_rx` channel — for non-seek commands only
- Epoch-based validation in `Audio::recv_valid_chunk()` — belt-and-suspenders
- Command coalescing in worker — for non-seek commands
- Variant fence — seek clears it in `apply_pending_seek()`
- `pending_seek_epoch` in Timeline — for tracking when post-seek output commits (SeekComplete event)

## Thread Safety Guarantees

| Thread | Reads from Timeline | Writes to Timeline |
|--------|--------------------|--------------------|
| Caller (Audio::seek) | — | `initiate_seek()`: epoch++, flushing=true, target, committed_pos |
| Worker (decoder loop) | `is_flushing()`, `seek_target()`, `seek_epoch()` | `complete_seek()`: flushing=false, target=SENTINEL. `advance_committed_samples()` |
| Downloader (tokio) | `is_flushing()`, `seek_epoch()`, `byte_position()` | `set_download_position()`, `set_eof()` |

No mutexes in seek path. All coordination via atomics. No deadlocks possible.

## Deadlock Resolution

| Deadlock | Root Cause | Fix |
|----------|-----------|-----|
| Worker stuck in wait_range | Can't process seek cmd from channel while inside Symphonia read callback | `wait_range()` checks `is_flushing()` atomically, returns `Interrupted` |
| Audio callback blocks on cmd_tx.send | Blocking send on RT thread while worker backpressured | `AudioCommand::Seek` removed. `Audio::seek()` only writes atomics. |
| Downloader throttled during seek | `should_throttle()` blocks downloader, no data for worker | `should_throttle()` returns false when `is_flushing()` |
