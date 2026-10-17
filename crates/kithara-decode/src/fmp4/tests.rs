#[cfg(feature = "resample-rubato")]
use std::num::NonZeroU32;
use std::{
    io::{self, Cursor, ErrorKind, Seek, SeekFrom},
    ops::Range,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use kithara_bufpool::PoolRegion;
use kithara_platform::{sync::Arc, time::Duration};
#[cfg(feature = "resample-rubato")]
use kithara_resampler::{ResamplerOptions, rubato::RubatoBackend};
use kithara_signal::AudioChunk;
#[cfg(feature = "symphonia")]
use kithara_stream::AudioCodec;
use kithara_stream::{ByteMap, ConstructionGate, NotReadyCause, PendingReason, SegmentDescriptor};
#[cfg(feature = "symphonia")]
use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment};
use kithara_test_utils::kithara;

use super::test_layout::{FakeSegmented, aac_five, aac_one, aac_three, flac_three};
#[cfg(feature = "resample-rubato")]
use crate::DecoderResamplerConfig;
#[cfg(all(android_backend, not(feature = "symphonia")))]
use crate::android::AndroidCodec as TestCodec;
#[cfg(all(apple_backend, not(feature = "symphonia")))]
use crate::apple::AppleCodec as TestCodec;
#[cfg(feature = "symphonia")]
use crate::symphonia::SymphoniaCodec as TestCodec;
use crate::{
    PrepareWait,
    codec::CodecPriming,
    composed::{ComposedDecoder, DecoderRuntime},
    demuxer::{DemuxOutcome, Demuxer},
    fmp4::{
        Fmp4SegmentDemuxer,
        parsing::{parse_init, parse_segment_frames},
    },
    test_pools::{TestPools, pools},
    traits::{BoxedSource, Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
};
#[cfg(feature = "symphonia")]
use crate::{
    codec::{FrameCodec, access_unit_frames},
    demuxer::TrackInfo,
    symphonia::{SymphoniaCodec, SymphoniaConfig},
};

/// Fixed-layout in-memory test source built from init+segment fixtures.
/// Records every absolute byte offset hit by `Read::read` so tests can
/// assert no-prefix-read invariants.
struct InstrumentedSource {
    reads: Arc<Mutex<Vec<Range<u64>>>>,
    record: Arc<AtomicBool>,
    inner: Cursor<Vec<u8>>,
    available_end: Option<Arc<AtomicU64>>,
    construction: Option<(ConstructionGate, Arc<AtomicBool>)>,
}

impl InstrumentedSource {
    fn check_io_mode(&self) {
        if let Some((gate, expected)) = &self.construction {
            assert_eq!(gate.is_armed(), expected.load(Ordering::Acquire));
        }
    }
}

impl io::Read for InstrumentedSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check_io_mode();
        let pos = self.inner.position();
        let available_end = self
            .available_end
            .as_ref()
            .map(|end| end.load(Ordering::Acquire));
        let result = match available_end {
            Some(end) if pos >= end && !buf.is_empty() => Err(io::Error::new(
                ErrorKind::Interrupted,
                PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
            )),
            Some(end) => {
                let available = usize::try_from(end.saturating_sub(pos))
                    .expect("fixture extent fits usize")
                    .min(buf.len());
                self.inner.read(&mut buf[..available])
            }
            None => self.inner.read(buf),
        };
        let n = result.as_ref().copied().unwrap_or(0);
        if self.record.load(Ordering::Acquire) && (n > 0 || result.is_err()) {
            self.reads
                .lock()
                .expect("BUG: reads lock")
                .push(pos..pos + n as u64);
        }
        result
    }
}

impl Seek for InstrumentedSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.check_io_mode();
        self.inner.seek(pos)
    }
}

type DecoderHarness = (
    ComposedDecoder<Fmp4SegmentDemuxer<TestPools>, TestCodec, TestPools>,
    Arc<Mutex<Vec<Range<u64>>>>,
    Arc<AtomicBool>,
);

#[kithara::test]
#[case::ready(false, false)]
#[case::partial(true, false)]
#[case::same_target_supersedes_partial(true, true)]
fn prepared_frames_do_not_read_container_bytes(
    aac_three: (Vec<u8>, FakeSegmented),
    #[case] partial: bool,
    #[case] supersede: bool,
) {
    let (blob, segmented) = aac_three;
    let segment = segmented.segment_at_index(0).expect("first segment");
    let prefix_end = segment.byte_range.start + 32;
    let reads = Arc::new(Mutex::new(Vec::new()));
    let available = Arc::new(AtomicU64::new(u64::MAX));
    let gate = ConstructionGate::default();
    gate.arm();
    let expected_blocking = Arc::new(AtomicBool::new(true));
    let source: BoxedSource = Box::new(InstrumentedSource {
        inner: Cursor::new(blob),
        reads: Arc::clone(&reads),
        record: Arc::new(AtomicBool::new(true)),
        available_end: Some(Arc::clone(&available)),
        construction: Some((gate.clone(), Arc::clone(&expected_blocking))),
    });
    let mut decoder =
        make_decoder_from_source(source, Arc::new(segmented), pools(), Some(gate.clone()));
    assert!(gate.is_armed(), "cold construction retains its I/O mode");
    expected_blocking.store(false, Ordering::Release);
    let reason = PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted);
    for target in [Duration::ZERO, Duration::from_secs(1), Duration::ZERO] {
        for wait in [PrepareWait::Poll, PrepareWait::Wait] {
            expected_blocking.store(wait == PrepareWait::Wait, Ordering::Release);
            gate.arm();
            reads.lock().expect("read log").clear();
            if partial {
                available.store(prefix_end, Ordering::Release);
            }
            let outcome = decoder.seek_with_wait(target, wait).expect("seek");
            assert!(gate.is_armed(), "seek restores the caller's blocking mode");
            if partial {
                assert_eq!(outcome, DecoderSeekOutcome::Pending(reason));
                assert_eq!(
                    reads.lock().expect("read log").as_slice(),
                    &[segment.byte_range.start..prefix_end, prefix_end..prefix_end]
                );
                if supersede {
                    reads.lock().expect("read log").clear();
                    assert_eq!(
                        decoder
                            .seek_with_wait(target, wait)
                            .expect("replacement seek"),
                        outcome
                    );
                    assert!(
                        gate.is_armed(),
                        "replacement seek restores the caller's mode"
                    );
                    assert_eq!(
                        reads.lock().expect("read log").as_slice(),
                        &[segment.byte_range.start..prefix_end, prefix_end..prefix_end],
                        "a new seek replaces the old read prefix even at the same target"
                    );
                }
                gate.disarm();
                expected_blocking.store(false, Ordering::Release);
                reads.lock().expect("read log").clear();
                decoder.prepare_next_chunk();
                assert!(matches!(
                    decoder.next_chunk_prepared().expect("unresolved PCM"),
                    DecoderChunkOutcome::Pending(actual) if actual == reason
                ));
                assert!(reads.lock().expect("read log").is_empty());
                assert_eq!(
                    decoder.prepare_seek_with_wait(wait).expect("continue seek"),
                    Some(outcome)
                );
                assert!(!gate.is_armed(), "pending continuation retains probe mode");
                assert_eq!(
                    reads.lock().expect("read log").as_slice(),
                    std::slice::from_ref(&(prefix_end..prefix_end)),
                    "continuation must retain the accumulated prefix"
                );
                gate.arm();
                expected_blocking.store(wait == PrepareWait::Wait, Ordering::Release);
                reads.lock().expect("read log").clear();
                assert_eq!(
                    decoder
                        .prepare_seek_with_wait(wait)
                        .expect("armed continuation"),
                    Some(outcome)
                );
                assert!(gate.is_armed(), "continuation restores the caller's mode");
                assert_eq!(
                    reads.lock().expect("read log").as_slice(),
                    std::slice::from_ref(&(prefix_end..prefix_end)),
                    "armed continuation must retain the accumulated prefix"
                );
                gate.disarm();
                expected_blocking.store(false, Ordering::Release);
                reads.lock().expect("read log").clear();
                available.store(u64::MAX, Ordering::Release);
                let outcome = decoder.prepare_seek_with_wait(wait).expect("resolve seek");
                assert!(matches!(outcome, Some(DecoderSeekOutcome::Landed { .. })));
                assert!(!gate.is_armed(), "landing continuation retains probe mode");
                assert!(
                    reads
                        .lock()
                        .expect("read log")
                        .iter()
                        .all(|read| read.start >= prefix_end),
                    "resolving the same operation must not reread its prefix"
                );
            } else {
                assert!(matches!(outcome, DecoderSeekOutcome::Landed { .. }));
            }
            gate.disarm();
            expected_blocking.store(false, Ordering::Release);
            reads.lock().expect("read log").clear();
            assert_eq!(
                decoder.prepare_seek_with_wait(wait).expect("settled seek"),
                None
            );
            assert!(reads.lock().expect("read log").is_empty());
            let mut chunks = 0;
            for _ in 0..128 {
                decoder.prepare_next_chunk();
                reads.lock().expect("read log").clear();
                let outcome = decoder.next_chunk_prepared().expect("prepared decode");
                assert!(reads.lock().expect("read log").is_empty());
                match outcome {
                    DecoderChunkOutcome::Chunk(_) => chunks += 1,
                    DecoderChunkOutcome::Pending(_) => {}
                    DecoderChunkOutcome::Eof => break,
                }
            }
            assert!(chunks > 0, "prepared seek must produce PCM");
        }
    }
}

fn make_decoder(blob: Vec<u8>, segmented: FakeSegmented) -> DecoderHarness {
    let reads: Arc<Mutex<Vec<Range<u64>>>> = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::new(AtomicBool::new(false));
    let source: BoxedSource = Box::new(InstrumentedSource {
        inner: Cursor::new(blob),
        reads: Arc::clone(&reads),
        record: Arc::clone(&record),
        available_end: None,
        construction: None,
    });
    let layout: Arc<dyn ByteMap> = Arc::new(segmented);
    let decoder = make_decoder_from_source(source, layout, pools(), None);
    (decoder, reads, record)
}

fn make_decoder_from_source(
    source: BoxedSource,
    layout: Arc<dyn ByteMap>,
    pools: PoolRegion<TestPools>,
    construction_gate: Option<ConstructionGate>,
) -> ComposedDecoder<Fmp4SegmentDemuxer<TestPools>, TestCodec, TestPools> {
    let demuxer = Fmp4SegmentDemuxer::open(source, layout, pools.clone(), construction_gate)
        .expect("BUG: build demuxer");
    #[cfg(feature = "symphonia")]
    let codec = SymphoniaCodec::open_with_config(demuxer.track_info(), &SymphoniaConfig::default())
        .expect("BUG: open codec");
    #[cfg(all(android_backend, not(feature = "symphonia")))]
    let codec = TestCodec::open_with_config(demuxer.track_info()).expect("open Android codec");
    #[cfg(all(apple_backend, not(feature = "symphonia")))]
    let codec =
        TestCodec::open_with_config(demuxer.track_info(), false, None).expect("open Apple codec");
    ComposedDecoder::new(
        demuxer,
        codec,
        DecoderRuntime {
            pools,
            ..DecoderRuntime::for_test()
        },
    )
}

/// Segment zero is still reported at its padded size until its plaintext commits.
struct EstimatedSegmentLayout {
    inner: FakeSegmented,
    committed: AtomicBool,
}

impl ByteMap for EstimatedSegmentLayout {
    fn segment_at_index(&self, index: u32) -> Option<SegmentDescriptor> {
        let mut segment = self.inner.segment_at_index(index)?;
        if index == 0 && !self.committed.load(Ordering::Acquire) {
            segment.byte_range.end += 16;
        }
        Some(segment)
    }

    delegate::delegate! {
        to self.inner {
            fn init_segment_range(&self) -> Range<u64>;
            fn len(&self) -> Option<u64>;
            fn segment_after_byte(&self, offset: u64) -> Option<SegmentDescriptor>;
            fn segment_at_time(&self, time: Duration) -> Option<SegmentDescriptor>;
            fn segment_count(&self) -> Option<u32>;
        }
    }
}

fn assert_pending_preparation_recovers_after_shrink(
    (blob, segmented): (Vec<u8>, FakeSegmented),
    consume_pending: bool,
    wrap: impl FnOnce(Box<dyn Decoder>, &PoolRegion<TestPools>) -> Box<dyn Decoder>,
) {
    let current_end = segmented.segments[0].byte_range.end;
    let reads = Arc::new(Mutex::new(Vec::new()));
    let source: BoxedSource = Box::new(InstrumentedSource {
        inner: Cursor::new(blob),
        reads: Arc::clone(&reads),
        record: Arc::new(AtomicBool::new(true)),
        available_end: Some(Arc::new(AtomicU64::new(current_end))),
        construction: None,
    });
    let layout = Arc::new(EstimatedSegmentLayout {
        inner: segmented,
        committed: AtomicBool::new(false),
    });
    let byte_map: Arc<dyn ByteMap> = layout.clone();
    let pools = pools();
    let decoder = make_decoder_from_source(source, byte_map, pools.clone(), None);
    let mut decoder = wrap(Box::new(decoder), &pools);
    let reason = PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted);

    reads.lock().expect("read log").clear();
    decoder.prepare_next_chunk();
    assert_eq!(
        reads.lock().expect("read log").last(),
        Some(&(current_end..current_end))
    );
    reads.lock().expect("read log").clear();
    assert!(matches!(
        decoder.next_chunk_prepared().expect("prepared pending"),
        DecoderChunkOutcome::Pending(actual) if actual == reason
    ));
    assert!(reads.lock().expect("read log").is_empty());

    decoder.prepare_next_chunk();
    assert_eq!(
        reads.lock().expect("read log").as_slice(),
        std::slice::from_ref(&(current_end..current_end))
    );
    reads.lock().expect("read log").clear();
    if consume_pending {
        assert!(matches!(
            decoder.next_chunk_prepared().expect("consume pending"),
            DecoderChunkOutcome::Pending(actual) if actual == reason
        ));
        assert!(reads.lock().expect("read log").is_empty());
    }
    layout.committed.store(true, Ordering::Release);

    for _ in 0..16 {
        decoder.prepare_next_chunk();
        let outcome = decoder
            .next_chunk_prepared()
            .expect("decode committed segment");
        assert!(
            reads.lock().expect("read log").is_empty(),
            "the current segment is buffered and the next remains gated"
        );
        match outcome {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(chunk.frames() > 0);
                assert_eq!(chunk.meta.segment_index, Some(0));
                assert_eq!(chunk.spec(), decoder.spec());
                return;
            }
            DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
            other => panic!("buffered current segment must yield PCM: {other:?}"),
        }
    }
    panic!("committed segment must produce PCM within the preparation budget");
}

#[kithara::test]
#[case::unconsumed(false)]
#[case::consumed(true)]
fn composed_pending_preparation_refreshes_after_live_segment_shrink(
    aac_three: (Vec<u8>, FakeSegmented),
    #[case] consume_pending: bool,
) {
    assert_pending_preparation_recovers_after_shrink(aac_three, consume_pending, |decoder, _| {
        decoder
    });
}

#[cfg(feature = "resample-rubato")]
fn wrap_with_mismatched_resampler(
    decoder: Box<dyn Decoder>,
    pools: &PoolRegion<TestPools>,
) -> Box<dyn Decoder> {
    let target = NonZeroU32::new(48_000).expect("non-zero target rate");
    assert_ne!(
        decoder.spec().sample_rate,
        target,
        "fixture must exercise the wrapper"
    );
    let config = DecoderResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .target_sample_rate(target)
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .build();
    let decoder =
        crate::resampled::wrap(decoder, Some(config), pools).expect("real resampled wrapper");
    assert_eq!(decoder.spec().sample_rate, target);
    decoder
}

#[cfg(feature = "resample-rubato")]
#[kithara::test]
#[case::unconsumed(false)]
#[case::consumed(true)]
fn resampled_pending_preparation_refreshes_after_live_segment_shrink(
    aac_three: (Vec<u8>, FakeSegmented),
    #[case] consume_pending: bool,
) {
    assert_pending_preparation_recovers_after_shrink(
        aac_three,
        consume_pending,
        wrap_with_mismatched_resampler,
    );
}

#[kithara::test]
fn next_chunk_yields_pcm_from_init_plus_segment_zero(aac_one: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_one;
    let (mut decoder, _, _) = make_decoder(blob, segmented);

    let mut got_chunk = None;
    for _ in 0..16 {
        match decoder.next_chunk().expect("BUG: decode") {
            DecoderChunkOutcome::Chunk(chunk) => {
                got_chunk = Some(chunk);
                break;
            }
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => break,
        }
    }
    let chunk = got_chunk.expect("BUG: at least one PCM chunk from segment 0");
    assert!(chunk.frames() > 0);
    assert!(chunk.spec().sample_rate.get() >= 8_000);
    assert!(chunk.spec().channels >= 1);
}

/// Helper used by the RED scaffolds below: pull one PCM chunk from the
/// decoder, returning `None` on EOF or after exhausting the retry budget.
fn pull_one_chunk(
    decoder: &mut ComposedDecoder<Fmp4SegmentDemuxer<TestPools>, TestCodec, TestPools>,
) -> Option<AudioChunk> {
    for _ in 0..16 {
        match decoder.next_chunk().ok()? {
            DecoderChunkOutcome::Chunk(chunk) => return Some(*chunk),
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => return None,
        }
    }
    None
}

/// An HLS variant between the playlist landing and the first segment
/// settling: it counts its segments and states its extent, but has not
/// described any segment yet.
struct PublishedButUndescribed {
    init_range: Range<u64>,
    count: u32,
    total: u64,
    lookups: AtomicUsize,
}

impl ByteMap for PublishedButUndescribed {
    fn init_segment_range(&self) -> Range<u64> {
        self.init_range.clone()
    }

    fn len(&self) -> Option<u64> {
        Some(self.total)
    }

    fn segment_after_byte(&self, _byte_offset: u64) -> Option<SegmentDescriptor> {
        None
    }

    fn segment_at_index(&self, _segment_index: u32) -> Option<SegmentDescriptor> {
        assert!(
            self.lookups.fetch_add(1, Ordering::Relaxed) < 16,
            "an undescribed segment must park preparation instead of retrying forever"
        );
        None
    }

    fn segment_at_time(&self, _t: Duration) -> Option<SegmentDescriptor> {
        None
    }

    fn segment_count(&self) -> Option<u32> {
        Some(self.count)
    }
}

/// A segment the layout has not described yet is a segment still owed, not
/// the end of the stream. Run 33752112563 lost
/// `packaged_abr_switch_keeps_player_continuity` to the other reading: the
/// incoming variant of an ABR up-switch reported EOF on its first chunk,
/// the transition was discarded with `abort_intent`, and the player never
/// switched.
#[kithara::test]
fn an_undescribed_segment_is_not_the_end_of_the_stream(aac_three: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_three;
    let init_range = 0..segmented.segments[0].byte_range.start;
    let total = segmented.segments[2].byte_range.end;
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn ByteMap> = Arc::new(PublishedButUndescribed {
        init_range,
        total,
        count: 3,
        lookups: AtomicUsize::new(0),
    });
    let mut demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools(), None).expect("BUG: build demuxer");

    let outcome = demuxer.next_frame().expect("BUG: demux a described layout");

    assert!(
        matches!(outcome, DemuxOutcome::Pending(PendingReason::Retry)),
        "an undescribed segment must park the decode, not end it: {outcome:?}"
    );
}

fn assert_undescribed_segment_preparation_is_bounded(
    (blob, segmented): (Vec<u8>, FakeSegmented),
    wrap: impl FnOnce(Box<dyn Decoder>, &PoolRegion<TestPools>) -> Box<dyn Decoder>,
) {
    let layout = Arc::new(PublishedButUndescribed {
        init_range: 0..segmented.segments[0].byte_range.start,
        total: segmented.segments[2].byte_range.end,
        count: 3,
        lookups: AtomicUsize::new(0),
    });
    let byte_map: Arc<dyn ByteMap> = layout.clone();
    let pools = pools();
    let decoder =
        make_decoder_from_source(Box::new(Cursor::new(blob)), byte_map, pools.clone(), None);
    let mut decoder = wrap(Box::new(decoder), &pools);
    for _ in 0..2 {
        let before = layout.lookups.load(Ordering::Relaxed);
        decoder.prepare_next_chunk();
        let prepared = layout.lookups.load(Ordering::Relaxed);
        assert!(
            prepared > before,
            "preparation must observe the segment still owed"
        );
        assert!(matches!(
            decoder.next_chunk_prepared().expect("undescribed segment"),
            DecoderChunkOutcome::Pending(PendingReason::Retry)
        ));
        assert_eq!(
            layout.lookups.load(Ordering::Relaxed),
            prepared,
            "consuming Pending must not poll the layout"
        );
    }
}

#[kithara::test]
fn composed_preparation_of_undescribed_segment_is_bounded(aac_three: (Vec<u8>, FakeSegmented)) {
    assert_undescribed_segment_preparation_is_bounded(aac_three, |decoder, _| decoder);
}

#[cfg(feature = "resample-rubato")]
#[kithara::test]
fn resampled_preparation_of_undescribed_segment_is_bounded(aac_three: (Vec<u8>, FakeSegmented)) {
    assert_undescribed_segment_preparation_is_bounded(aac_three, wrap_with_mismatched_resampler);
}

/// RED scaffold: a freshly opened `Fmp4SegmentDemuxer` always restarts at
/// the layout's seg-0 (`open` hardcodes `next_segment_index = 0`), so ABR
/// variant-switch `recreate_decoder` restarts playback at the new variant's
/// seg-0 instead of resuming. Pins the broken observable; invert to
/// `timestamp >= resume_point` when a non-zero resume API lands. Timestamp
/// is bounded (not 0) because the fdk-aac adapter strips ~1024 frames of
/// algo delay, landing the first chunk at packet 1 (≈46 ms @ 44.1 kHz).
#[kithara::test]
fn red_open_always_starts_at_layout_seg_0(aac_three: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_three;
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented);

    let chunk = pull_one_chunk(&mut decoder).expect("BUG: at least one PCM chunk from seg-0");
    // WHY: 50ms cap = codec-strip frame + warm-up packet ≈ 2 × 23.22 ms ≈ 46 ms, plus margin.
    let max_strip_time = Duration::from_micros(50_000);
    assert!(
        chunk.meta.timestamp <= max_strip_time,
        "RED — Fmp4SegmentDemuxer::open hardcodes next_segment_index=0, so the \
         first chunk always lands inside seg-0 (timestamp ≤ codec strip + \
         warm-up packet, ≈46 ms @ 44.1 kHz). There is no API to resume \
         at a non-zero decode_time. ABR variant-switch recreate_decoder \
         relies on this and consequently restarts playback at seg-0 of \
         the new variant. When the resume API lands, INVERT this \
         assertion. Got: {:?}",
        chunk.meta.timestamp
    );
}

/// RED scaffold (cursor freshness): `SegmentCursor::read.byte_range` is
/// frozen at `ensure_cursor`/`seek` time. If `ByteMap` updates the
/// descriptor before `fill_segment_buffer` (HEAD estimate → committed size,
/// or pre- → post-DRM), the cursor fills against the stale range and
/// `parse_segment_frames` panics ("sample byte range past segment end").
/// Fix must re-query the layout each fill (and grow) or version-cookie the
/// cursor. `#[ignore]`d: reproducing the panic needs bespoke moof bytes.
#[kithara::test]
#[ignore = "RED scaffold — needs crafted moof fixture to demonstrate \
            the parse_segment_frames panic. Add when implementing \
            the cursor-freshness contract."]
fn red_cursor_byte_range_freezes_when_layout_size_grows() {
    panic!("RED scaffold — see doc comment");
}

/// A boundary seek for an SBR codec backs up into the immediately
/// preceding segment (the codec requests AAC pre-roll, so
/// `Fmp4SegmentDemuxer::seek` lands at `target − warmup`) and decodes that
/// segment as decode-and-discard warm-up before reaching `target`. Reads
/// must stay confined to the pre-roll segment plus the target segment —
/// never a prefix walk from seg-0.
#[kithara::test]
fn seek_backs_up_one_segment_for_aac_preroll(aac_five: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_five;
    let (mut decoder, reads, record) = make_decoder(blob, segmented.clone());

    reads.lock().expect("BUG: clear").clear();
    record.store(true, Ordering::Release);

    // WHY: 18s = seg-2/seg-3 boundary; AAC SBR warm-up backs the seek into seg-2.
    let target = Duration::from_secs(18);
    let outcome = decoder.seek(target).expect("BUG: seek");
    let DecoderSeekOutcome::Landed {
        landed_at,
        landed_byte,
        ..
    } = outcome
    else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert!(
        landed_at >= Duration::from_secs(12) && landed_at < Duration::from_secs(18),
        "AAC boundary seek must land in the preceding segment for SBR \
         pre-roll, got {landed_at:?}",
    );
    let landed_byte = landed_byte.expect("BUG: landed_byte should be set");
    let segment_2 = &segmented.segments[2];
    let segment_3 = &segmented.segments[3];
    assert_eq!(landed_byte, segment_2.byte_range.start);

    for _ in 0..16 {
        match decoder.next_chunk().expect("BUG: decode after seek") {
            DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Eof => break,
            DecoderChunkOutcome::Pending(_) => continue,
        }
    }
    record.store(false, Ordering::Release);

    let reads_snapshot = reads.lock().expect("BUG: reads lock").clone();
    let preroll_start = segment_2.byte_range.start;
    let target_end = segment_3.byte_range.end;
    for r in &reads_snapshot {
        assert!(
            r.start >= preroll_start && r.end <= target_end,
            "read {r:?} fell outside pre-roll+target window {preroll_start}..{target_end} \
             (prefix-walk regression)",
        );
    }
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn seek_emits_notneeded_for_symphonia_aac_segment_boundary(aac_five: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_five;
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented);

    let target = Duration::from_secs(18);
    let outcome = decoder.seek(target).expect("BUG: seek");
    let DecoderSeekOutcome::Landed { preroll, .. } = outcome else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert_eq!(
        preroll,
        kithara_stream::PrerollHint::NotNeeded,
        "Symphonia fdk-aac handles MDCT priming internally — fmp4 demuxer must \
         emit NotNeeded (priming.byte_margin==0); got {preroll:?}"
    );
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn seek_emits_notneeded_for_symphonia_aac_first_segment(aac_five: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_five;
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented);

    let outcome = decoder.seek(Duration::ZERO).expect("BUG: seek to start");
    let DecoderSeekOutcome::Landed { preroll, .. } = outcome else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert_eq!(
        preroll,
        kithara_stream::PrerollHint::NotNeeded,
        "Symphonia fdk-aac handles priming internally — even seg-0 seek must emit \
         NotNeeded (priming.byte_margin==0); got {preroll:?}"
    );
}

#[kithara::test]
fn seek_emits_notneeded_for_first_segment_flac(flac_three: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = flac_three;
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn ByteMap> = Arc::new(segmented);
    let mut demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools(), None).expect("BUG: build FLAC demuxer");

    let outcome = demuxer
        .seek(Duration::ZERO, CodecPriming::default())
        .expect("BUG: seek to start on FLAC");
    let crate::demuxer::DemuxSeekOutcome::Landed { preroll, .. } = outcome else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert_eq!(
        preroll,
        kithara_stream::PrerollHint::NotNeeded,
        "FLAC has warmup_frames=0, so seg-0 seek must emit NotNeeded, not FirstSegment; got {preroll:?}"
    );
}

/// A seek into the middle of a segment starts just ahead of the target, not
/// at the segment's first frame. Landing on the segment start left every
/// frame in between to be decoded and dropped — a whole segment of codec work
/// per seek, during which the pipeline publishes nothing and a blocked
/// consumer sees no progress. The run-up is bounded by
/// `seek_warmup_access_units`, so the first frame sits within a few access
/// units of the target rather than seconds before it.
#[kithara::test]
fn seek_starts_just_ahead_of_the_target(aac_five: (Vec<u8>, FakeSegmented)) {
    let (blob, segmented) = aac_five;
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn ByteMap> = Arc::new(segmented);
    let mut demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools(), None).expect("BUG: demuxer");

    // WHY: 16s sits inside seg-2 (12s..18s), four seconds past its first frame.
    let target = Duration::from_secs(16);
    demuxer
        .seek(target, CodecPriming::default())
        .expect("BUG: seek");

    let mut first = None;
    for _ in 0..16 {
        match demuxer.next_frame().expect("BUG: demux after seek") {
            DemuxOutcome::Frame(frame) => {
                first = Some(frame.pts);
                break;
            }
            DemuxOutcome::Pending(_) => continue,
            DemuxOutcome::Eof => panic!("seek into seg-2 must not report EOF"),
        }
    }
    let pts = first.expect("BUG: no frame after seek");
    let run_up = target.saturating_sub(pts);
    assert!(
        pts <= target && run_up <= Duration::from_millis(250),
        "seek must start within the codec run-up of {target:?}, but the first frame is at \
         {pts:?} ({run_up:?} early) — the segment is being replayed from its start",
    );
}

/// EXTINF is rounded, so its running sum drifts from the segments' own decode
/// times. The segment the playlist names for a target can start after it; a
/// seek that landed there lost the audio between the target and that start.
/// The first PCM after a seek is the target's, whatever the playlist rounded.
#[kithara::test]
#[case::inside_a_drifted_segment(Duration::from_millis(11_900))]
#[case::at_a_drifted_segment_start(Duration::from_millis(11_800))]
fn seek_into_a_segment_the_playlist_places_early_keeps_the_target(
    flac_three: (Vec<u8>, FakeSegmented),
    #[case] target: Duration,
) {
    let (blob, segmented) = flac_three;
    let segmented = segmented.with_extinf(Duration::from_millis(5_900));
    let selected = segmented.segment_at_time(target).expect("playlist target");
    assert_eq!(selected.segment_index, 2);
    let earlier = segmented
        .segment_at_index(1)
        .expect("predecessor descriptor");
    let init_end = usize::try_from(segmented.init_segment_range().end).expect("init end");
    let init = parse_init(&blob[..init_end], &pools()).expect("parse init");
    let selected_start = usize::try_from(selected.byte_range.start).expect("selected start");
    let selected_end = usize::try_from(selected.byte_range.end).expect("selected end");
    let frames = parse_segment_frames(&init, &blob[selected_start..selected_end])
        .expect("selected segment frames");
    let first_frame = frames.first().expect("selected segment has media");
    assert!(
        u128::from(first_frame.decode_time) * 1_000_000_000
            > target.as_nanos() * u128::from(init.timescale),
        "the playlist-selected segment must start after the requested target"
    );
    let (mut decoder, reads, record) = make_decoder(blob, segmented);
    record.store(true, Ordering::Release);

    let outcome = decoder.seek(target).expect("seek");
    let DecoderSeekOutcome::Landed {
        landed_at,
        landed_byte,
        preroll,
        ..
    } = outcome
    else {
        panic!("expected resolved landing, got {outcome:?}");
    };
    assert_eq!(
        landed_byte,
        Some(earlier.byte_range.start),
        "the returned seek destination must name the segment used after late-start descent"
    );
    assert!(
        landed_at >= earlier.decode_time && landed_at < selected.decode_time,
        "landing time must describe the earlier segment: {landed_at:?}"
    );
    assert_eq!(preroll, kithara_stream::PrerollHint::NotNeeded);
    let first = (0..64)
        .find_map(|_| match decoder.next_chunk().expect("decode after seek") {
            DecoderChunkOutcome::Chunk(chunk) => Some(chunk.meta),
            DecoderChunkOutcome::Pending(_) => None,
            DecoderChunkOutcome::Eof => panic!("seek into seg-1/seg-2 must not report EOF"),
        })
        .expect("no PCM after seek");
    assert_eq!(
        first.timestamp, target,
        "the first PCM after a seek starts at the target"
    );
    assert_eq!(first.segment_index, Some(earlier.segment_index));
    let reads = reads.lock().expect("read ledger");
    for segment in [&selected, &earlier] {
        assert!(
            reads
                .iter()
                .any(|read| read.start == segment.byte_range.start),
            "late-start descent must inspect segment {}: {reads:?}",
            segment.segment_index
        );
    }
    assert!(
        reads
            .iter()
            .all(|read| read.start >= earlier.byte_range.start)
    );
    drop(reads);
}

#[cfg(feature = "symphonia")]
type AacFrameHarness = (SymphoniaCodec, Vec<u8>, Vec<(usize, usize)>);

/// Build a `SymphoniaCodec` from the AAC init segment plus the raw AAC
/// access units in segment 0. Mirrors `Fmp4SegmentDemuxer::build_track_info`
/// so the codec is opened with the same `TrackInfo` the real demuxer would
/// produce, then returns the per-frame `(offset, size)` access-unit ranges.
#[cfg(feature = "symphonia")]
fn aac_codec_and_frames(aac_init: &[u8], aac_segment: &[u8]) -> AacFrameHarness {
    let init_bytes = aac_init;
    let init = parse_init(init_bytes, &pools()).expect("BUG: parse AAC init");
    let extra_data = init.config.as_ref().to_vec();
    let track = TrackInfo {
        extra_data,
        codec: init.codec,
        sample_rate: init.sample_rate,
        channels: init.channels,
        duration: None,
        gapless: init.gapless,
    };
    let codec = SymphoniaCodec::open_with_config(&track, &SymphoniaConfig::default())
        .expect("BUG: open AAC codec");

    let seg = aac_segment.to_vec();
    let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
    let ranges = frames.iter().map(|f| (f.offset, f.size)).collect();
    (codec, seg, ranges)
}

#[cfg(feature = "symphonia")]
fn decode_all_aac(
    codec: &mut SymphoniaCodec,
    seg: &[u8],
    ranges: &[(usize, usize)],
    pools: &PoolRegion<TestPools>,
) -> Vec<f32> {
    let mut out_pcm = Vec::new();
    for (index, &(offset, size)) in ranges.iter().enumerate() {
        let frame_data = &seg[offset..offset + size];
        let mut buf = pools.get::<f32>();
        codec
            .decode_frame(
                crate::demuxer::Frame {
                    data: frame_data,
                    pts: codec
                        .spec()
                        .duration_for(index as u64 * 1_024)
                        .expect("AAC packet position"),
                    duration: codec
                        .spec()
                        .duration_for(1_024)
                        .expect("AAC packet duration"),
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata::default(),
                &mut buf,
            )
            .expect("BUG: decode AAC frame");
        if out_pcm.is_empty() && !buf.is_empty() {
            assert_eq!(codec.decoded_pts(), Some(Duration::ZERO));
        }
        out_pcm.extend_from_slice(&buf[..]);
        while codec.has_output() {
            codec
                .take_output(&mut buf)
                .expect("remaining AAC presentation interval");
            out_pcm.extend_from_slice(&buf);
        }
    }
    out_pcm
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn symphonia_aac_decode_is_bit_identical_across_passes(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
    let pools = pools();
    let (mut codec_a, seg, ranges) = aac_codec_and_frames(&aac_init, &aac_segment);
    let pcm_a = decode_all_aac(&mut codec_a, &seg, &ranges, &pools);

    let (mut codec_b, _, _) = aac_codec_and_frames(&aac_init, &aac_segment);
    let mut pcm_b = Vec::new();
    for (index, &(offset, size)) in ranges.iter().enumerate() {
        let packet = index as u64 * 1_024;
        let position = packet + if packet >= 10_240 { 4_096 } else { 0 };
        let mut buf = pools.get::<f32>();
        let mut output = codec_b
            .decode_frame(
                crate::demuxer::Frame {
                    data: &seg[offset..offset + size],
                    pts: codec_b
                        .spec()
                        .duration_for(position)
                        .expect("gapped packet position"),
                    duration: codec_b.spec().duration_for(1_024).expect("packet duration"),
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata {
                    segment_index: Some(u32::try_from(index / 8).expect("fixture segment")),
                    variant_index: Some(7),
                    source_bytes: u64::try_from(size).expect("fixture packet bytes"),
                },
                &mut buf,
            )
            .expect("decode AAC with a presentation gap");
        loop {
            if output.frames > 0 {
                let produced = pcm_b.len() as u64 / 2;
                let packet_index = usize::try_from(produced / 1_024).expect("source packet index");
                assert_eq!(
                    output.metadata.segment_index,
                    Some(u32::try_from(packet_index / 8).expect("source segment"))
                );
                assert_eq!(output.metadata.variant_index, Some(7));
                let expected_bytes = if produced.is_multiple_of(1_024) {
                    u64::try_from(ranges[packet_index].1).expect("source packet bytes")
                } else {
                    0
                };
                assert_eq!(
                    output.metadata.source_bytes, expected_bytes,
                    "encoded packet bytes are charged once to their source interval"
                );
                let expected = produced + if produced >= 10_240 { 4_096 } else { 0 };
                assert_eq!(
                    codec_b.decoded_pts(),
                    Some(
                        codec_b
                            .spec()
                            .duration_for(expected)
                            .expect("gapped PCM position")
                    )
                );
                pcm_b.extend_from_slice(&buf);
            }
            if !codec_b.has_output() {
                break;
            }
            output = codec_b
                .take_output(&mut buf)
                .expect("drain gapped AAC buffer");
        }
    }

    assert!(!pcm_a.is_empty(), "decode produced no PCM");
    assert_eq!(
        pcm_a, pcm_b,
        "decode_ref must be deterministic and bit-identical (no sample drift)"
    );
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn symphonia_aac_warm_decode_keeps_pool_bytes_stable(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
    let pools = pools();
    let (mut codec, seg, ranges) = aac_codec_and_frames(&aac_init, &aac_segment);
    assert!(!ranges.is_empty(), "segment yielded no AAC frames");

    let mut packet_frame = 0;
    for &(offset, size) in ranges.iter().take(8) {
        let mut buf = pools.get::<f32>();
        codec
            .decode_frame(
                crate::demuxer::Frame {
                    data: &seg[offset..offset + size],
                    pts: codec
                        .spec()
                        .duration_for(packet_frame)
                        .expect("AAC packet position"),
                    duration: codec
                        .spec()
                        .duration_for(1_024)
                        .expect("AAC packet duration"),
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata::default(),
                &mut buf,
            )
            .expect("BUG: warm-up decode");
        while codec.has_output() {
            codec
                .take_output(&mut buf)
                .expect("warm-up buffered AAC output");
        }
        packet_frame += 1_024;
    }
    let warm_bytes = pools.stats().allocated_bytes;

    for _ in 0..50 {
        for &(offset, size) in &ranges {
            let mut buf = pools.get::<f32>();
            codec
                .decode_frame(
                    crate::demuxer::Frame {
                        data: &seg[offset..offset + size],
                        pts: codec
                            .spec()
                            .duration_for(packet_frame)
                            .expect("AAC packet position"),
                        duration: codec
                            .spec()
                            .duration_for(1_024)
                            .expect("AAC packet duration"),
                        packet_desc: &[],
                    },
                    crate::demuxer::PacketMetadata::default(),
                    &mut buf,
                )
                .expect("BUG: warm decode");
            while codec.has_output() {
                codec
                    .take_output(&mut buf)
                    .expect("warm buffered AAC output");
            }
            packet_frame += 1_024;
        }
    }

    assert_eq!(pools.stats().allocated_bytes, warm_bytes);
}

/// The fdk-aac adapter drops `stream_info.outputDelay`, 1685 frames on this
/// fixture, from the head of the PCM it emits. That delay is lookahead latency,
/// not stripped signal: the first emitted frame is the first packet's first
/// frame. The codec clock therefore labels each chunk with the time of the
/// first packet's frame plus the frames emitted before it, rounded once like
/// every demuxer time, and models no timestamp bias.
///
/// A clock that put the dropped frames on top of the packet time placed this
/// backend 1685 frames later than every other decoder of the same stream, and
/// an exact variant splice cut where that offset said.
#[cfg(feature = "fdk-aac")]
#[kithara::test]
fn aac_head_strip_exceeds_the_bias_the_timeline_models(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
    let pools = pools();
    let (mut codec, seg, ranges) = aac_codec_and_frames(&aac_init, &aac_segment);
    let access_unit = u64::from(access_unit_frames(AudioCodec::AacLc));
    let supplied = ranges.len() as u64 * access_unit;
    // A first packet off the nanosecond grid: its time is already rounded.
    let first_frame = u64::from(codec.spec().sample_rate.get()) / 4 + 1;
    let mut emitted = 0_u64;

    for (index, &(offset, size)) in ranges.iter().enumerate() {
        let pts = codec
            .spec()
            .duration_for(first_frame + index as u64 * access_unit)
            .expect("packet time");
        let mut buf = pools.get::<f32>();
        let mut frames = codec
            .decode_frame(
                crate::demuxer::Frame {
                    data: &seg[offset..offset + size],
                    pts,
                    duration: codec
                        .spec()
                        .duration_for(access_unit)
                        .expect("AAC packet duration"),
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata::default(),
                &mut buf,
            )
            .expect("BUG: decode AAC frame")
            .frames;
        loop {
            if frames > 0 {
                assert_eq!(
                    codec.decoded_pts(),
                    Some(
                        codec
                            .spec()
                            .duration_for(first_frame + emitted)
                            .expect("PCM time")
                    ),
                    "packet {index}: PCM is labelled by the frames emitted since the first packet"
                );
                emitted += u64::from(frames);
            }
            if !codec.has_output() {
                break;
            }
            frames = codec
                .take_output(&mut buf)
                .expect("phase-preserving buffered AAC output")
                .frames;
        }
    }
    let head_strip = supplied.saturating_sub(emitted);

    assert!(
        head_strip > codec.timestamp_bias_frames(),
        "the adapter drops its output delay while the clock models no bias: \
         strip={head_strip}, bias={}",
        codec.timestamp_bias_frames()
    );
    assert_eq!(codec.timestamp_bias_frames(), 0);
    assert!(codec.needs_eof_drain(codec.spec().sample_rate.get()));
    let channels = usize::from(codec.spec().channels.max(1));
    let mut recovered = 0;
    let mut calls = 0;
    loop {
        let mut buf = pools.get::<f32>();
        let frames = if codec.has_output() {
            codec.take_output(&mut buf)
        } else {
            codec.decode_frame(
                crate::demuxer::Frame {
                    data: &[],
                    pts: Duration::ZERO,
                    duration: Duration::ZERO,
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata::default(),
                &mut buf,
            )
        }
        .expect("drain delayed AAC PCM")
        .frames;
        if frames == 0 {
            break;
        }
        calls += 1;
        assert!(
            calls <= 8,
            "native drain must terminate from its delay budget"
        );
        assert_eq!(buf.len(), frames as usize * channels);
        assert!(buf.iter().all(|sample| sample.is_finite()));
        assert_eq!(
            codec.decoded_pts(),
            Some(
                codec
                    .spec()
                    .duration_for(first_frame + emitted + recovered)
                    .expect("delayed PCM position")
            )
        );
        recovered += u64::from(frames);
    }
    assert_eq!(
        recovered, head_strip,
        "EOF must recover all delayed real PCM"
    );
    assert_eq!(emitted + recovered, supplied);
}
