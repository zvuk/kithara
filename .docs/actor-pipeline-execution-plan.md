# Actor Pipeline: Execution Plan (TDD)

> Detailed step-by-step plan derived from `.docs/actor-pipeline-design.md`
> and validated by Discover phase results (`.docs/actor-pipeline-discover-results.md`).

## API Corrections (dev-0.6 branch)

Design document uses Symphonia 0.5.5 naming. Actual API in dev-0.6:

| Design Doc | Actual (dev-0.6) |
|-----------|-----------------|
| `Packet::new_from_boxed_slice()` | `Packet::new(track_id, pts, dur, data: impl Into<Box<[u8]>>)` |
| `CodecParameters` | `AudioCodecParameters` (builder: `for_codec`, `with_extra_data`, etc.) |
| `Decoder` trait | `AudioDecoder` trait (`reset()`, `decode()`, `finalize()`) |
| `CodecRegistry::make()` | `CodecRegistry::make_audio_decoder(params, opts)` → `Box<dyn AudioDecoder>` |
| `EsdsAtom::fill_codec_params()` | `EsdsAtom::fill_audio_sample_entry(&mut AudioSampleEntry)` |
| `StsdAtom::fill_codec_params()` | `StsdAtom::make_codec_params()` → `Option<CodecParameters>` |

## Test Impact

| Category | Count | % |
|----------|-------|---|
| WILL BREAK | ~150-180 | 30-36% |
| WILL SURVIVE | ~250-290 | 56-58% |
| NEEDS ADAPTATION | ~30-50 | 6-10% |

## Prerequisite: Symphonia Fork

Before Phase 1, fork symphonia and make atoms module public:

```diff
// symphonia-format-isomp4/src/lib.rs
-mod atoms;
+pub mod atoms;
```

Update `Cargo.toml`:
```toml
symphonia = { git = "https://github.com/<our-fork>/Symphonia.git", branch="dev-0.6-pub-atoms", features = ["all", "opt-simd"] }
```

---

## Phase 0: Preparation

### P0.1: Feature branch
```bash
git checkout -b refactor/actor-pipeline
```

### P0.2: Record test baseline
```bash
cargo test --workspace 2>&1 | tee .docs/test-baseline.txt
# Expected: ~1247 pass, ~84 fail
```

### P0.3: Fork Symphonia
- Fork `pdeljanov/Symphonia` → our GitHub
- Branch `dev-0.6-pub-atoms`
- Single commit: `mod atoms` → `pub mod atoms` in symphonia-format-isomp4
- Update root `Cargo.toml` git URL

### P0.4: Verify atoms accessible
```rust
// Quick compile test
use symphonia_format_isomp4::atoms::{MoovAtom, MoofAtom, TrunAtom, EsdsAtom};
```

**Gate:** `cargo check --workspace` passes with fork.

---

## Phase 1: Break

### Goal: Delete old pipeline implementations, create kithara-actor crate

### 1.1: Create `kithara-actor` crate

**RED tests:**
```rust
// crates/kithara-actor/src/lib.rs tests

#[test]
fn actor_transitions_between_states() {
    // Actor starts in StateA, receives Msg, transitions to StateB
}

#[test]
fn actor_stops_on_transition_stop() {
    // Actor receives Msg that causes Transition::Stop, loop exits
}

#[test]
fn actor_addr_send_delivers_message() {
    // Addr<Msg>::send() delivers message to actor mailbox
}

#[test]
fn actor_spawn_returns_addr() {
    // spawn(initial_state, context) returns Addr for sending messages
}
```

**GREEN implementation:**
```rust
// crates/kithara-actor/src/lib.rs

pub enum Transition<S> {
    Next(S),
    Stop,
}

pub trait State: Send + 'static {
    type Msg: Send + 'static;
    type Context;

    fn on_message(
        self,
        msg: Self::Msg,
        ctx: &mut Self::Context,
    ) -> Transition<Box<dyn State<Msg = Self::Msg, Context = Self::Context>>>;
}

pub struct Addr<M> {
    tx: kithara_platform::sync::mpsc::Sender<M>,
}

impl<M: Send + 'static> Addr<M> {
    pub fn send(&self, msg: M) { ... }
}

pub fn spawn<S: State>(
    initial: S,
    ctx: S::Context,
) -> Addr<S::Msg>
where
    S::Context: Send + 'static,
{
    // kithara_platform::thread::spawn actor loop
}
```

**Dependencies:** `kithara-platform` only.

### 1.2: Delete old pipeline (implementations only, keep tests)

**Files to gut:**
1. `kithara-stream/src/source.rs` — delete `Source` trait body
2. `kithara-stream/src/stream.rs` — delete `Stream<T>`, `StreamType`
3. `kithara-stream/src/backend.rs` — delete `Backend`
4. `kithara-hls/src/source.rs` — delete `HlsSource`, `SharedSegments`
5. `kithara-hls/src/downloader.rs` — delete `HlsDownloader`
6. `kithara-hls/src/source_wait_range.rs` — delete wait_range impl
7. `kithara-audio/src/pipeline/source.rs` — delete `StreamAudioSource`, `SharedStream`

**Shared atomics to remove from Timeline:**
- `download_position`, `seek_epoch`, `flushing`, `seek_target_ns`, `pending_seek_epoch`

**Gate:** `cargo check --workspace` fails (expected — broken dependencies).
Test code preserved as comments or `#[cfg(never)]` for roadmap.

---

## Phase 2: Decoder Actor (core)

### Goal: Parse fMP4 init/media segments → codec decode → PCM

### 2.1: fMP4 Init Segment Parsing

**RED tests:**
```rust
#[test]
fn parse_init_segment_extracts_aac_codec_params() {
    let init_bytes = include_bytes!("../fixtures/aac_init.mp4");
    let params = parse_init_segment(init_bytes).unwrap();
    assert_eq!(params.codec, AudioCodecId::AAC);
    assert!(params.extra_data.is_some()); // ASC bytes
    assert_eq!(params.sample_rate, Some(44100));
}

#[test]
fn parse_init_segment_extracts_flac_codec_params() {
    let init_bytes = include_bytes!("../fixtures/flac_init.mp4");
    let params = parse_init_segment(init_bytes).unwrap();
    assert_eq!(params.codec, AudioCodecId::FLAC);
    assert!(params.extra_data.is_some()); // STREAMINFO bytes
}
```

**GREEN:** `parse_init_segment(bytes) → Result<AudioCodecParameters>`
- Navigation: bytes → `AtomIterator` → `MoovAtom` → `traks[0]` → `mdia` → `minf` → `stbl` → `stsd` → `make_codec_params()`

### 2.2: fMP4 Media Segment Parsing

**RED tests:**
```rust
#[test]
fn parse_media_segment_extracts_frames() {
    let media_bytes = include_bytes!("../fixtures/aac_segment.m4s");
    let frames = parse_media_segment(media_bytes).unwrap();
    assert!(!frames.is_empty());
    // Each frame: offset in mdat, size, duration, timestamp
}

#[test]
fn frame_offsets_cover_entire_mdat() {
    let media_bytes = include_bytes!("../fixtures/aac_segment.m4s");
    let frames = parse_media_segment(media_bytes).unwrap();
    let total: u64 = frames.iter().map(|f| f.size as u64).sum();
    // total should equal mdat payload size
}
```

**GREEN:** `parse_media_segment(bytes) → Result<Vec<FrameInfo>>`
- Navigation: bytes → `AtomIterator` → `MoofAtom` → `trafs[0]` → `truns[0]` → sample table
- Compute mdat_offset from moof_base_pos + data_offset
- For each sample: compute offset, size, duration from TrunAtom

### 2.3: Direct Codec Decoding

**RED tests:**
```rust
#[test]
fn decode_aac_frames_produces_pcm() {
    let init = include_bytes!("../fixtures/aac_init.mp4");
    let media = include_bytes!("../fixtures/aac_segment.m4s");

    let params = parse_init_segment(init).unwrap();
    let mut decoder = create_decoder(&params).unwrap();
    let frames = parse_media_segment(media).unwrap();

    for frame in &frames {
        let raw = &media[frame.offset..frame.offset + frame.size];
        let packet = Packet::new(0, frame.ts, frame.dur, raw.to_vec());
        let pcm = decoder.decode(&packet).unwrap();
        assert!(pcm.frames() > 0);
    }
}

#[test]
fn codec_reset_allows_non_contiguous_decode() {
    // Decode segment 0, reset, decode segment 5 — should not error
}
```

**GREEN:** `create_decoder(params) → Result<Box<dyn AudioDecoder>>`
- Use `symphonia::default::get_codecs().make_audio_decoder(params, &opts)`

### 2.4: Decoder Actor FSM

**RED tests:**
```rust
#[test]
fn decoder_starts_in_waiting_init() {
    // New decoder actor is in WaitingInit state
}

#[test]
fn decoder_transitions_waiting_init_to_ready_on_init_msg() {
    // Send Init(data) → transitions to Probing → Ready
}

#[test]
fn decoder_produces_pcm_on_segment_msg() {
    // In Ready state, send Segment(data) → OutputMsg::Pcm emitted
}

#[test]
fn decoder_resets_codec_on_seek_msg() {
    // In Decoding state, send Seek → reset() + transition to WaitingInit/Ready
}

#[test]
fn decoder_handles_variant_switch_same_params() {
    // New Init with same params → codec.reset(), stay Ready
}

#[test]
fn decoder_recreates_codec_on_param_change() {
    // New Init with different params → create new decoder
}
```

**GREEN:** Decoder actor FSM with states:
- `WaitingInit` → `Probing` → `Ready` → `Decoding` → `Ready` (loop)
- Seek → `WaitingInit` or `Ready`

**Gate:** All Phase 2 tests green. Decoder can parse init + media segments and produce PCM.

---

## Phase 3: ContentProvider + Downloader Actor

### Goal: Protocol-agnostic content delivery via ContentProvider trait + actor FSM

### 3.1: ContentProvider Trait

**RED tests:**
```rust
#[test]
fn content_provider_trait_compiles() {
    // ContentProvider trait with fetch_init, fetch_segment, resolve_seek,
    // num_segments, num_variants, container
}

#[test]
fn mock_content_provider_returns_fixture_data() {
    // MockContentProvider returns HLS fixture bytes
    // Verifies: fetch_init returns Some(init_bytes),
    //           fetch_segment returns segment_bytes
}
```

**GREEN:** `ContentProvider` trait in `kithara-stream/src/content.rs`.
- `fetch_init(variant) → Result<Option<Vec<u8>>, ContentError>`
- `fetch_segment(variant, index) → Result<Vec<u8>, ContentError>`
- `resolve_seek(time) → Result<SeekTarget, ContentError>`
- `num_segments(variant)`, `num_variants()`, `container()`
- `MockContentProvider` for tests (returns fixture data)

### 3.2: HlsContentProvider

**RED tests:**
```rust
#[test]
fn hls_provider_fetches_init_from_cache() {
    // Init segment already in AssetStore → returns bytes without network
}

#[test]
fn hls_provider_fetches_segment_via_fetch_manager() {
    // Uses existing FetchManager (Loader trait) for network + cache
}

#[test]
fn hls_provider_resolves_seek_to_segment_index() {
    // 90s → segment #14 (from PlaylistState)
}

#[test]
fn hls_provider_reports_multiple_variants() {
    // num_variants() returns actual variant count from playlist
}
```

**GREEN:** `HlsContentProvider` in `kithara-hls/src/content_provider.rs`.
- Wraps `FetchManager<N>` + `AssetStore` + `PlaylistState`
- Internal `tokio::runtime::Runtime` for async→sync bridge
- Reuses ALL existing FetchManager/Loader/AssetStore infrastructure

### 3.3: FileContentProvider

**RED tests:**
```rust
#[test]
fn file_provider_reads_local_file() {
    // Local file → AssetStore → fetch_segment returns bytes
}

#[test]
fn file_provider_returns_no_init_for_mp3() {
    // MP3 file → fetch_init returns None
}

#[test]
fn file_provider_single_segment() {
    // num_segments(0) == 1, num_variants() == 1
}
```

**GREEN:** `FileContentProvider` in `kithara-file/src/content_provider.rs`.
- Wraps `AssetStore` + `Net` (optional, for remote files)
- `fetch_init(0)` → None for MP3/FLAC/WAV, Some(moov) for fMP4
- `fetch_segment(0, 0)` → whole file bytes from AssetStore
- `num_segments(0) = 1`, `num_variants() = 1`

### 3.4: Downloader Actor FSM

**RED tests:**
```rust
#[test]
fn downloader_starts_idle() {
    // New downloader actor with MockContentProvider is in Idle state
}

#[test]
fn downloader_fetches_init_and_segment() {
    // Send Fetch { variant, index } → delivers Init + Segment to decoder_addr
}

#[test]
fn downloader_cancel_returns_to_idle() {
    // In Fetching state, send Cancel → back to Idle
}

#[test]
fn downloader_uses_cache_transparently() {
    // ContentProvider handles caching internally — actor doesn't know
}

#[test]
fn downloader_delivers_error_on_fetch_failure() {
    // ContentProvider returns error → actor delivers error, stays Idle
}

#[test]
fn downloader_stop_terminates_actor() {
    // Stop → actor thread exits
}
```

**GREEN:** Downloader actor in `kithara-stream/src/downloader_actor.rs`.
- States: `Idle`, `Fetching { cancel }`, `Stopped`
- Context: `provider: Box<dyn ContentProvider>`, `decoder_addr: Addr<DecoderMsg>`
- Protocol-agnostic: works with HLS, File, or any ContentProvider impl

### 3.5: Integration Tests

**RED tests:**
```rust
#[test]
fn hls_downloader_decoder_pipeline_produces_pcm() {
    // HlsContentProvider (fixture data) → Downloader Actor → Decoder Actor → PCM
}

#[test]
fn file_downloader_decoder_pipeline_produces_pcm() {
    // FileContentProvider (local fixture) → Downloader Actor → Decoder Actor → PCM
}
```

**Gate:** Both HLS and File paths produce PCM through the same Downloader + Decoder pipeline.

---

## Phase 4: Output Actor

### Goal: Resampler + effects + ring buffer in actor FSM

### 4.1: Output Actor FSM

**RED tests:**
```rust
#[test]
fn output_starts_buffering() {
    // New output actor starts in Buffering state
}

#[test]
fn output_transitions_to_playing_after_threshold() {
    // Send enough PcmChunks → Buffering → Playing
}

#[test]
fn output_drains_on_seek() {
    // Playing state, send Seek → drain resampler + ring buffer → Buffering
}

#[test]
fn output_pauses_and_resumes() {
    // Playing → Pause → Paused, Resume → Playing
}

#[test]
fn output_applies_resampler() {
    // PCM at 44100Hz → output at 48000Hz → resampled correctly
}

#[test]
fn output_backpressure_blocks_decoder() {
    // Ring buffer full → Output stops consuming → Decoder send() blocks
}
```

**GREEN:** Output actor with resampler + effects chain + ringbuf producer.
- States: `Buffering { threshold }`, `Playing`, `Paused { was_state }`, `Stopped`
- Context: `resampler`, `effects`, `ring: Producer`, `controller_addr`, `decoder_addr`

**Gate:** Output actor processes PCM, applies resampling, pushes to ring buffer.

---

## Phase 5: Seek + Integration

### Goal: Cascading seek, Controller wiring, full pipeline

### 5.1: Seek Cascade

**RED tests:**
```rust
#[test]
fn seek_cascades_through_all_actors() {
    // Controller → Output(Seek) → Decoder(Seek) → Downloader(Cancel+Fetch) → PCM flows
}

#[test]
fn seek_during_playback_produces_correct_pcm() {
    // Playing at 0:00, seek to 1:30 → PCM from segment #14
}

#[test]
fn rapid_seeks_do_not_deadlock() {
    // 100 rapid seeks in 1 second — no hang, no panic
}

#[test]
fn seek_to_zero_works() {
    // Edge case: seek to beginning
}

#[test]
fn seek_to_near_end_works() {
    // Edge case: seek to last segment
}
```

### 5.2: Controller Wiring

**RED tests:**
```rust
#[test]
fn controller_receives_seek_complete() {
    // After seek cascade completes, Controller gets SeekComplete
}

#[test]
fn controller_receives_playback_started() {
    // After initial buffering, Controller gets PlaybackStarted
}

#[test]
fn controller_receives_error_on_failure() {
    // Network error → Controller gets Error(PipelineError)
}
```

### 5.3: ABR Variant Switch During Seek

**RED tests:**
```rust
#[test]
fn abr_switch_during_seek_produces_clean_audio() {
    // Seek + variant switch → no click, no corruption
}

#[test]
fn abr_switch_same_codec_resets_decoder() {
    // AAC 128k → AAC 256k → codec.reset(), no recreate
}

#[test]
fn abr_switch_different_codec_recreates_decoder() {
    // AAC → FLAC → new decoder created
}
```

### 5.4: Stress Tests (previously failing 84)

**RED tests:**
```rust
#[test]
fn stress_rapid_seeks_during_abr_switch_must_not_kill_audio() {
    // 20s stress test — was in the 84 failing tests
}

#[test]
fn stress_random_seek_read_hls() {
    // Random seeks across all segments
}

#[test]
fn stress_seek_audio_hls_wav() {
    // Seek stress with WAV output
}
```

**Gate:** All 84 previously failing seek tests now pass. Total: 1331 green.

---

## Phase 6: FormatReader Mode for Non-fMP4

### Goal: Decoder Actor support for MP3/FLAC/WAV containers (via Symphonia FormatReader)

FileContentProvider already delivers file bytes (from Phase 3.3).
This phase adds the second decode path inside Decoder Actor.

### 6.1: FormatReader Decode Mode

**RED tests:**
```rust
#[test]
fn decoder_handles_raw_file_segment_mp3() {
    // DecoderMsg::Segment with MP3 bytes → FormatReader → packets → PCM
}

#[test]
fn decoder_handles_raw_file_segment_flac() {
    // FLAC file bytes → FormatReader → packets → PCM
}

#[test]
fn decoder_seeks_within_format_reader_file() {
    // Seek → FormatReader.seek() works (single file, not virtual layout)
}
```

**GREEN:** Decoder Actor detects container format from ContentProvider metadata:
- fMP4 → existing direct codec path (init segment + media segments)
- MP3/FLAC/WAV → FormatReader mode (whole file as Read+Seek cursor)
- FormatReader seek works correctly for single files (no virtual layout)

### 6.2: End-to-End File Pipeline

**RED tests:**
```rust
#[test]
fn file_pipeline_plays_mp3() {
    // FileContentProvider → Downloader → Decoder (FormatReader) → Output → PCM
}

#[test]
fn file_pipeline_seeks() {
    // Seek within file → FormatReader.seek() → correct PCM position
}
```

**Gate:** File playback and seek work through the unified actor pipeline.

---

## Phase 7: kithara-play Integration

### Goal: Engine, Player, QueuePlayer through new pipeline

### 7.1: Player Integration

**RED tests:**
```rust
#[test]
fn player_plays_hls_track() {
    // Player::play(hls_url) → audio output
}

#[test]
fn player_seeks_hls_track() {
    // Player::seek(Duration) → seek cascade → audio resumes
}

#[test]
fn player_switches_tracks() {
    // Player::play(track_a), Player::play(track_b) → clean transition
}
```

### 7.2: QueuePlayer Integration

**RED tests:**
```rust
#[test]
fn queue_player_advances_to_next_track() {
    // Queue with 3 tracks → auto-advance at EOF
}

#[test]
fn queue_player_seeks_across_tracks() {
    // Seek that crosses track boundary
}
```

### 7.3: WASM Integration

**RED tests:**
```rust
#[test]
fn wasm_player_plays_hls() {
    // AudioWorklet integration with new actor pipeline
}
```

**Gate:** All integration tests green. WASM builds.

---

## Phase Completion Criteria

| Phase | Gate | Tests |
|-------|------|-------|
| 0 | Fork works, baseline recorded | cargo check passes |
| 1 | kithara-actor compiles, old code deleted | ~4-6 new actor tests green |
| 2 | Init+media parsing, codec decode | ~10-15 new tests green |
| 3 | Downloader fetches, delivers to Decoder | ~6-8 new tests + existing fetch tests |
| 4 | Output processes PCM, ring buffer | ~6-8 new tests |
| 5 | Seek cascade works, stress tests pass | ~84 previously failing + new seek tests |
| 6 | File support restored | existing file tests green |
| 7 | Full integration, WASM | all 1331 tests green |

## File Structure After Refactoring

```
crates/
├── kithara-actor/          ← NEW
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          (State, Transition, Addr, spawn)
│       └── tests.rs
├── kithara-stream/
│   └── src/
│       ├── lib.rs          (AudioCodec, ContainerFormat, MediaInfo — kept)
│       ├── timeline.rs     (simplified — no seek_epoch/flushing)
│       └── [source.rs, stream.rs, backend.rs — DELETED]
├── kithara-hls/
│   └── src/
│       ├── lib.rs
│       ├── fetch.rs        (FetchManager — kept)
│       ├── inner.rs        (Hls orchestrator — rewritten to use actors)
│       ├── decoder_actor.rs    ← NEW (fMP4 parsing + codec decode FSM)
│       ├── downloader_actor.rs ← NEW (FetchManager wrapper FSM)
│       ├── output_actor.rs     ← NEW (resampler + effects + ring buffer FSM)
│       ├── controller.rs       ← NEW (seek cascade, error handling)
│       ├── fmp4_parser.rs      ← NEW (init/media segment parsing)
│       └── [source.rs, downloader.rs, source_wait_range.rs — DELETED]
├── kithara-audio/
│   └── src/
│       ├── pipeline/
│       │   ├── mod.rs      (rewritten to use actor pipeline)
│       │   └── [source.rs — DELETED]
│       ├── resampler.rs    (kept)
│       └── effects/        (kept)
└── ... (other crates unchanged)
```

## Execution Order

```
Phase 0 → Phase 1 → Phase 2 → Phase 3 → Phase 4 → Phase 5 → Phase 6 → Phase 7
  prep     break     decoder   downloader  output    seek+ABR    file     play
  (1 day)  (1 day)  (2-3 days) (1-2 days) (1-2 days)(2-3 days)(1-2 days)(2-3 days)
```

Each phase follows strict TDD: RED test → GREEN implementation → REFACTOR.
