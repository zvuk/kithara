use std::{
    io::{Cursor, Seek, SeekFrom},
    ops::Range,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::PoolRegion;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioChunk;
use kithara_stream::{AudioCodec, ByteMap, PendingReason, SegmentDescriptor};
use kithara_test_utils::kithara;

use super::test_layout::{FakeSegmented, TestLayoutCodec, build_test_layout, read_fixture};
use crate::{
    codec::{CodecPriming, FrameCodec, access_unit_frames},
    composed::{ComposedDecoder, DecoderRuntime},
    demuxer::{DemuxOutcome, Demuxer, TrackInfo},
    fmp4::{
        Fmp4SegmentDemuxer,
        parsing::{parse_init, parse_segment_frames},
    },
    symphonia::{SymphoniaCodec, SymphoniaConfig},
    test_pools::{TestPools, pools},
    traits::{BoxedSource, Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
};

/// Fixed-layout in-memory test source built from init+segment fixtures.
/// Records every absolute byte offset hit by `Read::read` so tests can
/// assert no-prefix-read invariants.
struct InstrumentedSource {
    reads: Arc<Mutex<Vec<Range<u64>>>>,
    record: Arc<AtomicBool>,
    inner: Cursor<Vec<u8>>,
}

impl std::io::Read for InstrumentedSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.position();
        let n = self.inner.read(buf)?;
        if self.record.load(Ordering::Acquire) && n > 0 {
            self.reads
                .lock()
                .expect("BUG: reads lock")
                .push(pos..pos + n as u64);
        }
        Ok(n)
    }
}

impl Seek for InstrumentedSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

type DecoderHarness = (
    ComposedDecoder<Fmp4SegmentDemuxer<TestPools>, SymphoniaCodec, TestPools>,
    Arc<Mutex<Vec<Range<u64>>>>,
    Arc<AtomicBool>,
);

fn make_decoder(blob: Vec<u8>, segmented: FakeSegmented) -> DecoderHarness {
    let reads: Arc<Mutex<Vec<Range<u64>>>> = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::new(AtomicBool::new(false));
    let source: BoxedSource = Box::new(InstrumentedSource {
        inner: Cursor::new(blob),
        reads: Arc::clone(&reads),
        record: Arc::clone(&record),
    });
    let layout: Arc<dyn ByteMap> = Arc::new(segmented);
    let pools = pools();
    let demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools.clone()).expect("BUG: build demuxer");
    let codec = SymphoniaCodec::open_with_config(demuxer.track_info(), &SymphoniaConfig::default())
        .expect("BUG: open codec");
    let decoder = ComposedDecoder::new(
        demuxer,
        codec,
        DecoderRuntime {
            pools,
            ..DecoderRuntime::for_test()
        },
    );
    (decoder, reads, record)
}

#[kithara::test]
fn next_chunk_yields_pcm_from_init_plus_segment_zero() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 1);
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
    decoder: &mut ComposedDecoder<Fmp4SegmentDemuxer<TestPools>, SymphoniaCodec, TestPools>,
) -> Option<AudioChunk> {
    for _ in 0..16 {
        match decoder.next_chunk().ok()? {
            DecoderChunkOutcome::Chunk(chunk) => return Some(chunk),
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
fn an_undescribed_segment_is_not_the_end_of_the_stream() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 3);
    let init_range = 0..segmented.segments[0].byte_range.start;
    let total = segmented.segments[2].byte_range.end;
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn ByteMap> = Arc::new(PublishedButUndescribed {
        init_range,
        count: 3,
        total,
    });
    let mut demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools()).expect("BUG: build demuxer");

    let outcome = demuxer.next_frame().expect("BUG: demux a described layout");

    assert!(
        matches!(outcome, DemuxOutcome::Pending(PendingReason::Retry)),
        "an undescribed segment must park the decode, not end it: {outcome:?}"
    );
}

/// RED scaffold: a freshly opened `Fmp4SegmentDemuxer` always restarts at
/// the layout's seg-0 (`open` hardcodes `next_segment_index = 0`), so ABR
/// variant-switch `recreate_decoder` restarts playback at the new variant's
/// seg-0 instead of resuming. Pins the broken observable; invert to
/// `timestamp >= resume_point` when a non-zero resume API lands. Timestamp
/// is bounded (not 0) because the fdk-aac adapter strips ~1024 frames of
/// algo delay, landing the first chunk at packet 1 (≈46 ms @ 44.1 kHz).
#[kithara::test]
fn red_open_always_starts_at_layout_seg_0() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 3);
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
/// preceding segment (`SymphoniaCodec::priming` requests AAC pre-roll, so
/// `Fmp4SegmentDemuxer::seek` lands at `target − warmup`) and decodes that
/// segment as decode-and-discard warm-up before reaching `target`. Reads
/// must stay confined to the pre-roll segment plus the target segment —
/// never a prefix walk from seg-0.
#[kithara::test]
fn seek_backs_up_one_segment_for_aac_preroll() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 5);
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

#[kithara::test]
fn seek_emits_notneeded_for_symphonia_aac_segment_boundary() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 5);
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

#[kithara::test]
fn seek_emits_notneeded_for_symphonia_aac_first_segment() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 5);
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
fn seek_emits_notneeded_for_first_segment_flac() {
    let (blob, segmented) = build_test_layout(TestLayoutCodec::Flac, 3);
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn ByteMap> = Arc::new(segmented);
    let mut demuxer =
        Fmp4SegmentDemuxer::open(source, layout, pools()).expect("BUG: build FLAC demuxer");

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

type AacFrameHarness = (SymphoniaCodec, Vec<u8>, Vec<(usize, usize)>);

/// Build a `SymphoniaCodec` from the AAC init segment plus the raw AAC
/// access units in segment 0. Mirrors `Fmp4SegmentDemuxer::build_track_info`
/// so the codec is opened with the same `TrackInfo` the real demuxer would
/// produce, then returns the per-frame `(offset, size)` access-unit ranges.
fn aac_codec_and_frames() -> AacFrameHarness {
    let init_bytes = read_fixture("init-slq-a1.mp4");
    let init = parse_init(&init_bytes, &pools()).expect("BUG: parse AAC init");
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

    let seg = read_fixture("segment-1-slq-a1.m4s");
    let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
    let ranges = frames.iter().map(|f| (f.offset, f.size)).collect();
    (codec, seg, ranges)
}

fn decode_all_aac(
    codec: &mut SymphoniaCodec,
    seg: &[u8],
    ranges: &[(usize, usize)],
    pools: &PoolRegion<TestPools>,
) -> Vec<f32> {
    let mut out_pcm = Vec::new();
    for &(offset, size) in ranges {
        let frame_data = &seg[offset..offset + size];
        let mut buf = pools.get::<f32>();
        codec
            .decode_frame(frame_data, Duration::ZERO, &[], &mut buf)
            .expect("BUG: decode AAC frame");
        out_pcm.extend_from_slice(&buf[..]);
    }
    out_pcm
}

/// R-tovec: the zero-copy `PacketRef`/`decode_ref` entry must produce
/// PCM bit-identical to the owning `Packet::new(.., to_vec())` path it
/// replaces. Two independent decoder passes over the same real AAC
/// access units must yield byte-for-byte equal interleaved f32 PCM —
/// pro-DJ zero tolerance for sample drift.
#[kithara::test]
fn symphonia_aac_decode_is_bit_identical_across_passes() {
    let pools = pools();
    let (mut codec_a, seg, ranges) = aac_codec_and_frames();
    let pcm_a = decode_all_aac(&mut codec_a, &seg, &ranges, &pools);

    let (mut codec_b, _, _) = aac_codec_and_frames();
    let pcm_b = decode_all_aac(&mut codec_b, &seg, &ranges, &pools);

    assert!(!pcm_a.is_empty(), "decode produced no PCM");
    assert_eq!(
        pcm_a, pcm_b,
        "decode_ref must be deterministic and bit-identical (no sample drift)"
    );
}

#[kithara::test]
fn symphonia_aac_warm_decode_keeps_pool_bytes_stable() {
    let pools = pools();
    let (mut codec, seg, ranges) = aac_codec_and_frames();
    assert!(!ranges.is_empty(), "segment yielded no AAC frames");

    for &(offset, size) in ranges.iter().take(8) {
        let mut buf = pools.get::<f32>();
        codec
            .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
            .expect("BUG: warm-up decode");
    }
    let warm_bytes = pools.stats().allocated_bytes;

    for _ in 0..50 {
        for &(offset, size) in &ranges {
            let mut buf = pools.get::<f32>();
            codec
                .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
                .expect("BUG: warm decode");
        }
    }

    assert_eq!(pools.stats().allocated_bytes, warm_bytes);
}

/// A decoder instance strips its own algorithmic delay from the head of the
/// PCM it emits. The observed strip is the difference between packet frames
/// supplied and PCM frames emitted, and is not any declared constant: the
/// fdk-aac adapter drops `stream_info.outputDelay`, 1685 frames on this fixture,
/// while `timestamp_bias_frames` models one access unit, 1024.
///
/// The 661-frame remainder decides where an exact variant splice cuts. A decode
/// that started at the head sees it as a container timeline gap the moment the
/// next packet's timestamp runs past what the decoder has emitted; a decode
/// that started mid-stream records no such jump, because `seek` resyncs the
/// frame offset onto the packet timestamp instead. Observing the strip in
/// `ComposedDecoder` is what lets its live timeline-gap query hand both the
/// same figure.
#[kithara::test]
fn aac_head_strip_exceeds_the_bias_the_timeline_models() {
    let pools = pools();
    let (mut codec, seg, ranges) = aac_codec_and_frames();
    let supplied = ranges.len() as u64 * u64::from(access_unit_frames(AudioCodec::AacLc));
    let pcm = decode_all_aac(&mut codec, &seg, &ranges, &pools);

    let channels = u64::from(codec.spec().channels.max(1));
    let emitted = pcm.len() as u64 / channels;
    let head_strip = supplied.saturating_sub(emitted);

    assert!(
        head_strip > codec.timestamp_bias_frames(),
        "the declared bias is expected to fall short of the real strip on this \
         fixture — that shortfall is the whole point of reporting the observed \
         one: strip={head_strip}, bias={}",
        codec.timestamp_bias_frames()
    );
}
