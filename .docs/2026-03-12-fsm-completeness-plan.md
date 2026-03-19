# Plan: FSM Completeness — WaitContext + Downloader Phase Projection

**Date**: 2026-03-12
**Prerequisite**: branch `source-phase-fsm` (SourcePhase FSM core)
**Debate summary**: `.docs/debate-source-fsm-completeness.md`

## Problem

`SourcePhase` покрывает reader-facing lifecycle (что вернуть из `wait_range`),
но НЕ объясняет **почему** данных нет. В production логе `Waiting` бесполезен —
непонятно: downloader throttled? variant switch? metadata loading? gap filling?

Downloader phase **недоступен** из source side:
- `FileDownloader::phase` (`FilePhase`) — private field
- `HlsDownloader` — вообще нет explicit phase enum
- `Downloader::should_throttle()` — вызывается только изнутри Backend loop
- `had_midstream_switch` — retro signal (факт, не текущее состояние)

## Architecture Decision

**Unanimously rejected**: flat unified FSM (state explosion).
**Adopted**: layered model — SourcePhase (reader) + WaitContext (producer hint).

---

## Phase 1: WaitContext (immediate — ~100 LOC)

### 1.1 Что добавляем в kithara-stream

```rust
// source.rs — new types

/// Protocol-agnostic category explaining why data isn't ready.
///
/// Set by downloader/source, consumed by tracing and diagnostics.
/// Does NOT affect `SourcePhase::classify()` priority ordering.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WaitCategory {
    /// No specific wait reason (default / data actually ready).
    #[default]
    None,
    /// Normal network download in progress.
    Downloading,
    /// Downloader deliberately paused (ahead of reader by look_ahead limit).
    Throttled,
    /// Protocol reconfiguration (ABR variant switch, format change).
    Reconfiguring,
    /// Metadata/playlist/size-map not yet available.
    MetadataLoading,
    /// Error recovery: gap filling, retry after failure.
    Recovering,
}
```

`SourcePhaseView` получает новое поле:

```rust
pub struct SourcePhaseView {
    // ... existing 7 bool fields ...
    /// Why data isn't ready. Informational — does not affect classify().
    pub wait_category: WaitCategory,
}
```

`classify()` **не меняется** — `wait_category` ignored в decision logic.

Re-export из `lib.rs`: `pub use source::WaitCategory;`

### 1.2 Механизм публикации: shared AtomicU8

Downloader и Source живут в разных потоках. Нужен lock-free способ
передать текущий `WaitCategory` от downloader к source.

**File (kithara-file)**:
```rust
// session.rs — SharedFileState расширяется
pub struct SharedFileState {
    // ... existing fields ...
    /// Current downloader wait category (AtomicU8, maps to WaitCategory).
    pub wait_category: AtomicU8,
}
```

FileDownloader обновляет атомик при каждой смене фазы:
- `Sequential` start → `Downloading`
- `should_throttle()` true → `Throttled`
- `GapFilling` start → `Recovering`
- `Complete` → `None`
- Error + retry → `Recovering`

FileSource читает атомик в `phase()`:
```rust
let wait_category = WaitCategory::from_u8(
    self.shared.as_ref().map_or(0, |s| s.wait_category.load(Ordering::Acquire))
);
```

**HLS (kithara-hls)**:
```rust
// source.rs — SharedSegments расширяется
pub struct SharedSegments {
    // ... existing fields ...
    /// Current downloader wait category.
    pub wait_category: AtomicU8,
}
```

HlsDownloader обновляет при:
- Normal segment fetch → `Downloading`
- `should_throttle()` true → `Throttled`
- `handle_midstream_switch()` → `Reconfiguring`
- `calculate_size_map()` start → `MetadataLoading`
- No segment found + retry → `Recovering`
- Idle / complete → `None`

HlsSource читает в `phase_view()`:
```rust
wait_category: WaitCategory::from_u8(
    self.shared.wait_category.load(Ordering::Acquire)
),
```

### 1.3 WaitCategory → u8 mapping

```rust
impl WaitCategory {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Downloading => 1,
            Self::Throttled => 2,
            Self::Reconfiguring => 3,
            Self::MetadataLoading => 4,
            Self::Recovering => 5,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Downloading,
            2 => Self::Throttled,
            3 => Self::Reconfiguring,
            4 => Self::MetadataLoading,
            5 => Self::Recovering,
            _ => Self::None,
        }
    }
}
```

### 1.4 Tracing integration

В `wait_range()` обоих sources, spinning log получает context:

```rust
// HLS wait_range
debug!(
    range_start = range.start,
    range_end = range.end,
    ?phase,
    ?view.wait_category,  // NEW: structured wait reason
    "wait_range: spinning"
);
```

### 1.5 Файлы и scope изменений

| Crate | File | Change |
|-------|------|--------|
| kithara-stream | `src/source.rs` | + `WaitCategory` enum (~25 LOC), + field in `SourcePhaseView` |
| kithara-stream | `src/lib.rs` | + re-export `WaitCategory` |
| kithara-file | `src/session.rs` | + `AtomicU8` в `SharedFileState`, read in `phase()` |
| kithara-file | `src/downloader.rs` | + store `WaitCategory` при phase transitions (~10 точек) |
| kithara-hls | `src/source.rs` | + `AtomicU8` в `SharedSegments`, read in `phase_view()` |
| kithara-hls | `src/downloader.rs` | + store `WaitCategory` при state changes (~8 точек) |

**Estimated**: ~100 LOC prod + ~40 LOC tests.

### 1.6 Тесты

- Unit: `WaitCategory` round-trip (as_u8 → from_u8)
- Unit: `classify()` ignores `wait_category` (same result regardless of value)
- Integration: File `phase()` returns correct `wait_category` after downloader sets it
- Integration: HLS `phase_view()` returns correct `wait_category`

### 1.7 Hard constraints

- `classify()` НЕ меняется — `WaitCategory` purely informational
- `WaitCategory` is `#[non_exhaustive]` — future variants without breaking change
- `Default` = `None` — backward compatible (existing code doesn't set it)
- No new cross-thread synchronization beyond single `AtomicU8`

---

## Phase 2: DownloaderPhase enum (medium term — ~200 LOC)

### Предпосылка

Phase 1 даёт **abstract category** (Throttled, Recovering). Phase 2 даёт
**explicit downloader lifecycle** — concrete phases, не abstract buckets.

### 2.1 Что добавляем

Каждый protocol crate получает explicit phase enum:

```rust
// kithara-file/src/downloader.rs — уже есть FilePhase, делаем pub(crate)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePhase {
    Sequential,
    GapFilling,
    Complete,
}
```

```rust
// kithara-hls/src/downloader.rs — НЕТ explicit phase, создаём
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsDownloaderPhase {
    /// Loading playlist/size map metadata.
    LoadingMetadata,
    /// Downloading segments sequentially.
    Downloading,
    /// Throttled (ahead of reader).
    Throttled,
    /// Handling ABR variant switch.
    VariantSwitching,
    /// Rewinding to fill missing segments.
    Rewinding,
    /// Waiting for on-demand segment request.
    WaitingDemand,
    /// All segments loaded.
    Complete,
}
```

### 2.2 Публикация через AtomicU8

Тот же механизм что Phase 1 — `AtomicU8` в shared state.
`HlsDownloaderPhase` маппится в `WaitCategory` для kithara-stream,
но protocol crate сохраняет полный enum для своих собственных логов.

### 2.3 Dual-level logging

```
[kithara-hls] wait_range: spinning phase=Waiting wait_category=Throttled
[kithara-hls] downloader: phase=Throttled segment=15 variant=2 ahead_bytes=524288
```

Reader thread логирует abstract `WaitCategory`.
Downloader thread логирует concrete `HlsDownloaderPhase`.
Correlation по seek_epoch / timestamp.

### 2.4 Mapping таблица

| HlsDownloaderPhase | → WaitCategory |
|---------------------|----------------|
| LoadingMetadata | MetadataLoading |
| Downloading | Downloading |
| Throttled | Throttled |
| VariantSwitching | Reconfiguring |
| Rewinding | Recovering |
| WaitingDemand | None |
| Complete | None |

| FilePhase + context | → WaitCategory |
|---------------------|----------------|
| Sequential (downloading) | Downloading |
| Sequential (throttled) | Throttled |
| GapFilling | Recovering |
| Complete | None |

---

## Phase 3: Diagnostic snapshot (long term — ~300 LOC)

### Предпосылка

Phases 1-2 дают per-field observability. Phase 3 добавляет
**atomic snapshot** всего состояния для external consumers
(UI, metrics, debug endpoints).

### 3.1 StatusSnapshot в kithara-stream

```rust
/// Immutable snapshot of playback state for external consumers.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    /// Reader-facing phase.
    pub phase: SourcePhase,
    /// Why waiting (if applicable).
    pub wait_category: WaitCategory,
    /// Current byte position.
    pub byte_position: u64,
    /// Buffered bytes ahead of read position.
    pub buffered_bytes: u64,
    /// Total expected bytes (if known).
    pub total_bytes: Option<u64>,
    /// Current seek epoch.
    pub seek_epoch: u64,
    /// Protocol-specific detail (opaque string for logs).
    pub detail: Option<String>,
}
```

### 3.2 Публикация через tokio::sync::watch

```rust
// В Stream<T> или SharedStream<T>:
pub fn status_rx(&self) -> watch::Receiver<StatusSnapshot>;
```

Source обновляет snapshot в `wait_range()` loop при каждой итерации.
External consumers (UI, TUI dashboard, metrics) подписываются.

### 3.3 TUI dashboard integration

kithara-app TUI может показывать:
```
Track: song.mp3
Phase: Waiting (Throttled)
Buffer: 2.3MB / 8.1MB [=====>      ] 28%
Seek: epoch=3
```

### 3.4 Scope

| Crate | Change |
|-------|--------|
| kithara-stream | `StatusSnapshot` struct, `watch` channel |
| kithara-file | Publish snapshot in wait_range |
| kithara-hls | Publish snapshot in wait_range |
| kithara-app | TUI widget consumes snapshot |

---

## Не делаем (explicitly out of scope)

1. **Session reducer / actor** (Codex Round 3) — требует новый crate,
   rewrite Backend loop. Отложено до появления unified playback coordinator.
2. **WaitCategory влияет на classify()** — сознательно informational-only.
   Если понадобятся разные timeout budgets per category — отдельный design.
3. **Opaque protocol_code: u32** (Gemini) — `detail: Option<String>` в Phase 3
   достаточен. Numeric codes добавляют complexity без clear benefit.

---

## Execution order

Phase 1 можно начать немедленно на текущей ветке `source-phase-fsm`.
Phases 2-3 — отдельные ветки после merge Phase 1.

Phase 1 tasks:
1. Add `WaitCategory` enum + tests в kithara-stream (~30 LOC)
2. Add `AtomicU8` в SharedFileState, publish from FileDownloader (~30 LOC)
3. Add `AtomicU8` в SharedSegments, publish from HlsDownloader (~30 LOC)
4. Wire into `phase()` / `phase_view()` + tracing (~20 LOC)
5. Full test gate + review
