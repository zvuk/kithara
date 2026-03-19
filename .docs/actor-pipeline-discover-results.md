# Actor Pipeline: Discover Results

**Date:** 2026-03-11

## Symphonia API Audit (dev-0.6 branch)

### Public APIs — confirmed available:
- `Packet::new_from_boxed_slice(track_id, ts, dur, data)` — PUBLIC
- `CodecParameters::new()` + builder (.for_codec, .with_sample_rate, .with_channels, .with_extra_data) — PUBLIC
- `CodecRegistry::make(&self, params, options)` → `Box<dyn Decoder>` — PUBLIC
- `Decoder` trait with `reset(&mut self)` — PUBLIC
- `StreamInfo::read()` from symphonia-utils-xiph — PUBLIC

### Atom parsers — available via one-line patch:
- All atom types (MoovAtom, MoofAtom, TrunAtom, EsdsAtom, StsdAtom, etc.) are `pub use` in `atoms/mod.rs`
- Parent module `atoms` is `mod atoms` (not pub) in `lib.rs`
- **Fix:** Change `mod atoms` → `pub mod atoms` in fork of symphonia-format-isomp4
- Project already uses git dependency: `symphonia = { git = "...", branch="dev-0.6" }`

### Key types from atoms/mod.rs:
- `AtomHeader` — box header parsing (4cc + size)
- `MoovAtom` — movie header (init segment)
- `MoofAtom` — movie fragment (media segment)
- `TrunAtom` — track run (sample table with offsets/sizes/durations)
- `TrafAtom` — track fragment
- `TfhdAtom` — track fragment header
- `EsdsAtom` — ES descriptor (AAC codec config)
- `StsdAtom` — sample description (codec config)
- `FlacAtom` — FLAC-in-fMP4 config
- `ElstAtom` — edit list (encoder delay)

## Code Inventory

### To DELETE (~6370 lines):
| File | Lines | What |
|------|-------|------|
| kithara-stream/src/source.rs | 230 | Source trait + ReadOutcome |
| kithara-stream/src/stream.rs | 443 | Stream\<T\>, StreamType, Read+Seek impl |
| kithara-stream/src/backend.rs | 601 | Backend, downloader lifecycle |
| kithara-hls/src/source.rs | 1009 | HlsSource, SharedSegments, variant_fence |
| kithara-hls/src/downloader.rs | 2532 | HlsDownloader, seek_epoch coordination |
| kithara-hls/src/source_wait_range.rs | 294 | wait_range orchestration |
| kithara-audio/src/pipeline/source.rs | 1262 | StreamAudioSource, SharedStream, format change detection |

### To KEEP:
- FetchManager (kithara-hls/src/fetch.rs, 1063 lines)
- AssetStore chain (kithara-assets)
- kithara-net (Net trait, HttpClient, RetryNet)
- kithara-events (EventBus, Event enum)
- kithara-bufpool (Pool, BytePool, PcmPool)
- kithara-platform (Mutex, Condvar, mpsc, thread::spawn)

### To MODIFY:
- kithara-stream/src/timeline.rs — remove seek_epoch, flushing, download_position atomics; keep byte_position, eof, total_bytes

## Platform Primitives

| Primitive | Status | Notes |
|-----------|--------|-------|
| `sync::mpsc::channel()` | READY | Unbounded, `recv_sync()`, `try_recv()` |
| `Mutex<T>` | READY | parking_lot / wasm_safe_thread |
| `Condvar` | READY | `wait_sync()`, `notify_one()`, `notify_all()` |
| `thread::spawn` | READY | OS thread / Web Worker |

**Note:** mpsc is unbounded only. Backpressure via ringbuf (already used).

## Risks Assessment

| Risk | Severity | Status |
|------|----------|--------|
| Symphonia atoms private | LOW | One-line fix in fork |
| WASM actor mailbox | LOW | `recv_sync()` available on WASM |
| "Break first" = 0 tests | MEDIUM | Fast transition to Phase 2 |
| File ≠ fMP4 formats | MEDIUM | Phase 6, Symphonia FormatReader for non-fMP4 |
| Audio priming/padding | MEDIUM | ElstAtom available for edit list parsing |
| Unbounded mailbox | LOW | Backpressure via ringbuf, not mailbox |
