use std::{
    io::{Cursor, Seek, SeekFrom},
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_platform::time::Duration;
use kithara_stream::{SegmentDescriptor, SegmentLayout};
use kithara_test_utils::kithara;

use crate::{
    codec::{CodecPriming, FrameCodec},
    composed::ComposedDecoder,
    demuxer::{Demuxer, TrackInfo},
    fmp4::{
        Fmp4SegmentDemuxer,
        parsing::{CodecConfig, parse_init, parse_segment_frames},
    },
    symphonia::{SymphoniaCodec, SymphoniaConfig},
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

#[derive(Clone)]
struct FakeSegmented {
    segments: Arc<Vec<SegmentDescriptor>>,
    init_range: Range<u64>,
}

impl SegmentLayout for FakeSegmented {
    fn init_segment_range(&self) -> Range<u64> {
        self.init_range.clone()
    }

    fn len(&self) -> Option<u64> {
        self.segments.last().map(|s| s.byte_range.end)
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        for desc in self.segments.iter() {
            if desc.byte_range.start >= byte_offset {
                return Some(desc.clone());
            }
        }
        None
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        self.segments.get(segment_index as usize).cloned()
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        for desc in self.segments.iter() {
            let end = desc.decode_time.saturating_add(desc.duration);
            if t < end {
                return Some(desc.clone());
            }
        }
        self.segments.last().cloned()
    }

    fn segment_count(&self) -> Option<u32> {
        u32::try_from(self.segments.len()).ok()
    }
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/hls")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// Stitch the AAC init segment + a few media segments into one byte
/// buffer and build a `FakeSegmented` that maps time/byte ranges
/// against the resulting layout.
fn build_test_layout(num_segments: usize) -> (Vec<u8>, FakeSegmented) {
    let init = read_fixture("init-slq-a1.mp4");
    let init_len = init.len() as u64;

    let segment_duration_secs = 6u64;

    let mut blob = init.clone();
    let mut descs = Vec::new();
    let mut byte_cursor = init_len;
    for i in 1..=num_segments {
        let seg_bytes = read_fixture(&format!("segment-{i}-slq-a1.m4s"));
        let len = seg_bytes.len() as u64;
        let start = byte_cursor;
        let end = start + len;
        let seg_index = u32::try_from(i - 1).expect("BUG: segment index fits u32");
        descs.push(SegmentDescriptor::new(
            start..end,
            Duration::from_secs((u64::from(seg_index)) * segment_duration_secs),
            Duration::from_secs(segment_duration_secs),
            seg_index,
            0,
        ));
        blob.extend_from_slice(&seg_bytes);
        byte_cursor = end;
    }

    (
        blob,
        FakeSegmented {
            init_range: 0..init_len,
            segments: Arc::new(descs),
        },
    )
}

type DecoderHarness = (
    ComposedDecoder<Fmp4SegmentDemuxer, SymphoniaCodec>,
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
    let layout: Arc<dyn SegmentLayout> = Arc::new(segmented);
    let demuxer =
        Fmp4SegmentDemuxer::open(source, layout, BytePool::default()).expect("BUG: build demuxer");
    let codec = SymphoniaCodec::open_with_config(demuxer.track_info(), &SymphoniaConfig::default())
        .expect("BUG: open codec");
    let decoder = ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None);
    (decoder, reads, record)
}

#[kithara::test]
fn next_chunk_yields_pcm_from_init_plus_segment_zero() {
    let (blob, segmented) = build_test_layout(1);
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
    assert!(chunk.spec().sample_rate >= 8_000);
    assert!(chunk.spec().channels >= 1);
}

/// Helper used by the RED scaffolds below: pull one PCM chunk from the
/// decoder, returning `None` on EOF or after exhausting the retry budget.
fn pull_one_chunk(
    decoder: &mut ComposedDecoder<Fmp4SegmentDemuxer, SymphoniaCodec>,
) -> Option<crate::types::PcmChunk> {
    for _ in 0..16 {
        match decoder.next_chunk().ok()? {
            DecoderChunkOutcome::Chunk(chunk) => return Some(chunk),
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => return None,
        }
    }
    None
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
    let (blob, segmented) = build_test_layout(3);
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
/// frozen at `ensure_cursor`/`seek` time. If `SegmentLayout` updates the
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
    let (blob, segmented) = build_test_layout(5);
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
    let (blob, segmented) = build_test_layout(5);
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented.clone());

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
    let (blob, segmented) = build_test_layout(5);
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented.clone());

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

fn build_test_layout_flac(num_segments: usize) -> (Vec<u8>, FakeSegmented) {
    let init = read_fixture("init-slossless-a1.mp4");
    let init_len = init.len() as u64;

    let segment_duration_secs = 6u64;

    let mut blob = init.clone();
    let mut descs = Vec::new();
    let mut byte_cursor = init_len;
    for i in 1..=num_segments {
        let seg_bytes = read_fixture(&format!("segment-{i}-slossless-a1.m4s"));
        let len = seg_bytes.len() as u64;
        let start = byte_cursor;
        let end = start + len;
        let seg_index = u32::try_from(i - 1).expect("BUG: segment index fits u32");
        descs.push(SegmentDescriptor::new(
            start..end,
            Duration::from_secs(u64::from(seg_index) * segment_duration_secs),
            Duration::from_secs(segment_duration_secs),
            seg_index,
            0,
        ));
        blob.extend_from_slice(&seg_bytes);
        byte_cursor = end;
    }

    (
        blob,
        FakeSegmented {
            init_range: 0..init_len,
            segments: Arc::new(descs),
        },
    )
}

#[kithara::test]
fn seek_emits_notneeded_for_first_segment_flac() {
    let (blob, segmented) = build_test_layout_flac(3);
    let source: BoxedSource = Box::new(Cursor::new(blob));
    let layout: Arc<dyn SegmentLayout> = Arc::new(segmented);
    let mut demuxer = Fmp4SegmentDemuxer::open(source, layout, BytePool::default())
        .expect("BUG: build FLAC demuxer");

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
    let init = parse_init(&init_bytes).expect("BUG: parse AAC init");
    let extra_data = match &init.config {
        CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
    };
    let track = TrackInfo {
        codec: init.codec,
        sample_rate: init.sample_rate,
        channels: init.channels,
        extra_data,
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
    pool: &PcmPool,
) -> Vec<f32> {
    let mut out_pcm = Vec::new();
    for &(offset, size) in ranges {
        let frame_data = &seg[offset..offset + size];
        let mut buf = pool.get();
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
    let pool = PcmPool::default();
    let (mut codec_a, seg, ranges) = aac_codec_and_frames();
    let pcm_a = decode_all_aac(&mut codec_a, &seg, &ranges, &pool);

    let (mut codec_b, _, _) = aac_codec_and_frames();
    let pcm_b = decode_all_aac(&mut codec_b, &seg, &ranges, &pool);

    assert!(!pcm_a.is_empty(), "decode produced no PCM");
    assert_eq!(
        pcm_a, pcm_b,
        "decode_ref must be deterministic and bit-identical (no sample drift)"
    );
}

/// R-tovec: a warm per-packet AAC decode loop must not grow the pool's
/// `alloc_misses` after warm-up. The zero-copy `PacketRef` entry borrows
/// the frame slice instead of cloning it into an owning `Packet`, so the
/// per-packet heap allocation is gone; the `RTSan` lane is the malloc
/// oracle, this guards against any accidental per-call pool growth.
#[kithara::test]
fn symphonia_aac_warm_decode_does_not_grow_pool_alloc_misses() {
    let pool = PcmPool::new(32, 8192);
    let (mut codec, seg, ranges) = aac_codec_and_frames();
    assert!(!ranges.is_empty(), "segment yielded no AAC frames");

    for &(offset, size) in ranges.iter().take(8) {
        let mut buf = pool.get();
        codec
            .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
            .expect("BUG: warm-up decode");
    }
    let warm_misses = pool.stats().alloc_misses;

    for _ in 0..50 {
        for &(offset, size) in &ranges {
            let mut buf = pool.get();
            codec
                .decode_frame(&seg[offset..offset + size], Duration::ZERO, &[], &mut buf)
                .expect("BUG: warm decode");
        }
    }

    assert_eq!(
        pool.stats().alloc_misses,
        warm_misses,
        "warm AAC decode must not allocate fresh pool buffers per packet"
    );
}
