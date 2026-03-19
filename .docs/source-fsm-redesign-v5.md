# Source FSM Redesign v5 — Flat FSM (final)

## Суть

Заменить разбросанную логику `WaitRangeState` + `WaitRangeContext` + `WaitRangeDecision` +
`build_wait_range_context()` + `decide_wait_range()` на единый `SourcePhase` enum + `update_phase()`.

**Критерий**: меньше кода. Удаляем ~130 строк, добавляем ~45. Нетто: **−85 строк**.

## SourcePhase

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SourcePhase {
    #[default]
    Waiting,
    Ready,
    Seeking,
    Eof,
    Failed,
}
```

Хранится как поле `HlsSource::phase`. Обновляется в начале каждой итерации `wait_range` через `update_phase()`.

## Что удаляется

| Что | Файл | Строки |
|-----|------|--------|
| `WaitRangeState` struct + impl | source_wait_range.rs | ~29 |
| `WaitRangeContext` struct | source_wait_range.rs | ~9 |
| `WaitRangeDecision` enum | source_wait_range.rs | ~7 |
| `build_wait_range_context()` | source_wait_range.rs | ~23 |
| `decide_wait_range()` | source_wait_range.rs | ~40 |
| `request_on_demand_if_needed()` | source_wait_range.rs | ~21 |
| **Итого** | | **~129** |

## Что добавляется

| Что | Файл | Строки |
|-----|------|--------|
| `SourcePhase` enum | source.rs | ~10 |
| `update_phase()` | source_wait_range.rs | ~25 |
| `is_past_eof()` helper | source_wait_range.rs | ~6 |
| `phase` field in HlsSource | source.rs | ~2 |
| **Итого** | | **~43** |

## Что остаётся без изменений

- `fallback_segment_index_for_offset()` — utility
- `committed_segment_for_offset()` — lookup
- `request_on_demand_segment()` — core on-demand logic
- `variant_fence: Option<usize>` — работает, замена на VariantPhase добавит код
- `SeekLayout` + `classify_seek()` — уже реализованы
- `had_midstream_switch: AtomicBool` — отдельный scope (switch_generation), не в этом PR

## Новый wait_range (псевдокод)

```rust
fn wait_range(&mut self, range: Range<u64>, timeout: Duration) -> StreamResult<WaitOutcome, HlsError> {
    let mut on_demand_epoch: Option<u64> = None;
    let mut metadata_miss_count: usize = 0;
    let started_at = Instant::now();

    kithara_platform::hang_watchdog! {
        timeout: timeout;
        thread: "hls.wait_range";
        loop {
            let segments = self.shared.segments.lock_sync();
            self.update_phase(&segments, &range);

            match self.phase {
                SourcePhase::Failed => return Err(StreamError::Source(HlsError::Cancelled)),
                SourcePhase::Ready => { hang_reset!(); return Ok(WaitOutcome::Ready); }
                SourcePhase::Eof => return Ok(WaitOutcome::Eof),
                SourcePhase::Seeking => return Ok(WaitOutcome::Interrupted),
                SourcePhase::Waiting => {}
            }

            // Invalidate stale on-demand request
            let seek_epoch = self.shared.timeline.seek_epoch();
            if on_demand_epoch.is_some_and(|e| e != seek_epoch) { on_demand_epoch = None; }
            if self.shared.timeline.eof() { on_demand_epoch = None; }
            if self.shared.had_midstream_switch.swap(false, Ordering::AcqRel) { on_demand_epoch = None; }
            drop(segments);

            // On-demand request (inlined, no WaitRangeState)
            if !on_demand_epoch.is_some_and(|e| e == seek_epoch) {
                let requested = self.request_on_demand_segment(
                    range.start, seek_epoch, &mut metadata_miss_count,
                    WAIT_RANGE_MAX_METADATA_MISS_SPINS,
                )?;
                if requested { on_demand_epoch = Some(seek_epoch); }
            }

            // Timeout
            if started_at.elapsed() > timeout {
                return Err(StreamError::Source(HlsError::Timeout(...)));
            }

            // Condvar wait
            segments = self.shared.segments.lock_sync();
            hang_tick!();
            kithara_platform::thread::yield_now();
            let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
            let (_segments, _) = self.shared.condvar.wait_sync_timeout(segments, deadline);

            if self.shared.timeline.is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
        }
    }
}
```

## update_phase

```rust
fn update_phase(&mut self, segments: &DownloadState, range: &Range<u64>) {
    if self.shared.cancel.is_cancelled() {
        self.phase = SourcePhase::Failed;
        return;
    }
    let range_ready = self.range_ready_from_segments(segments, range);
    if self.shared.stopped.load(Ordering::Acquire) && !range_ready {
        self.phase = if self.is_past_eof(segments, range) { SourcePhase::Eof } else { SourcePhase::Failed };
        return;
    }
    if range_ready {
        self.phase = SourcePhase::Ready;
        return;
    }
    if self.shared.timeline.is_flushing() {
        self.phase = SourcePhase::Seeking;
        return;
    }
    if self.is_past_eof(segments, range) {
        self.phase = SourcePhase::Eof;
        return;
    }
    self.phase = SourcePhase::Waiting;
}

fn is_past_eof(&self, segments: &DownloadState, range: &Range<u64>) -> bool {
    let eof = self.shared.timeline.eof();
    let effective = segments.max_end_offset().max(self.shared.timeline.total_bytes().unwrap_or(0));
    eof && effective > 0 && range.start >= effective
}
```

## Шаги реализации

1. Добавить `SourcePhase` enum + `phase` field в `HlsSource`
2. Написать `update_phase()` + `is_past_eof()` в source_wait_range.rs
3. Переписать `wait_range()` на dispatch по `self.phase`
4. Удалить `WaitRangeState`, `WaitRangeContext`, `WaitRangeDecision`, `build_wait_range_context()`, `decide_wait_range()`, `request_on_demand_if_needed()`
5. Адаптировать тесты
6. `cargo test -p kithara-hls` — всё зелёное

## Source trait API — без изменений

`wait_range`, `read_at`, `seek_time_anchor`, `clear_variant_fence` — сигнатуры не меняются.
`Stream<T>`, `Audio<Stream<T>>` — не затрагиваются.
