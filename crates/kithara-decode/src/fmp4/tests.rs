use std::{
    io::{Cursor, Seek, SeekFrom},
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::PcmPool;
use kithara_platform::time::Duration;
use kithara_stream::{SegmentDescriptor, SegmentLayout};
use kithara_test_utils::kithara;

use crate::{
    composed::ComposedDecoder,
    demuxer::Demuxer,
    fmp4::Fmp4SegmentDemuxer,
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
    let demuxer = Fmp4SegmentDemuxer::open(source, layout).expect("BUG: build demuxer");
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

/// RED scaffold for ABR variant-switch sub-problem #3
/// (see project_hls_abr_variant_switch_init_range_bug).
///
/// `Fmp4SegmentDemuxer::open` hardcodes `next_byte = 0`. Production
/// calls `apply_format_change` with `target_offset` equal to the
/// init-range start in the NEW variant's byte map (which is 0 because
/// `set_layout_variant` already swapped the active byte map). The
/// factory then builds the demuxer with `OffsetReader::base_offset = 0`,
/// and the demuxer's first `next_frame` queries
/// `segment_after_byte(0)` which always returns the layout's seg-0.
///
/// Result: regardless of where the previous decoder left off, the new
/// decoder restarts at the new variant's seg-0 (T1's "drift = full
/// pre-switch frame count" symptom).
///
/// This test pins the CURRENT (broken) observable: a freshly opened
/// `Fmp4SegmentDemuxer` always emits its first chunk at
/// `decode_time = 0`. There is no API surface to resume at a non-zero
/// time / segment / byte. When such an API is added (e.g.
/// `Fmp4SegmentDemuxer::open_with_start(source, layout, ResumePoint)`),
/// this assertion must be inverted to `chunk.meta.timestamp >= resume_point`.
#[kithara::test]
fn red_open_always_starts_at_layout_seg_0() {
    let (blob, segmented) = build_test_layout(3);
    let (mut decoder, _reads, _record) = make_decoder(blob, segmented);

    let chunk = pull_one_chunk(&mut decoder).expect("BUG: at least one PCM chunk from seg-0");
    assert_eq!(
        chunk.meta.timestamp,
        Duration::ZERO,
        "RED — Fmp4SegmentDemuxer::open hardcodes next_byte=0, so the \
         first chunk always lands at decode_time=0. There is no API to \
         resume at a non-zero decode_time. ABR variant-switch \
         recreate_decoder relies on this and consequently restarts \
         playback at seg-0 of the new variant. \
         When the resume API lands, INVERT this assertion."
    );
}

/// RED scaffold for ABR variant-switch sub-problem #4
/// (cursor freshness): `SegmentCursor::read.byte_range` is frozen at
/// `ensure_cursor` / `seek` time from `descriptor.byte_range`. If the
/// underlying `SegmentLayout` updates the descriptor between
/// `ensure_cursor` and `fill_segment_buffer` (HEAD-estimated size →
/// actual committed size, or pre-DRM → post-DRM), the cursor still
/// allocates and seeks against the OLD range. `parse_segment_frames`
/// then panics with "sample byte range past segment end" because the
/// fragment's moof-described samples don't fit the smaller buffer.
///
/// Contract under test:
///   - Either `fill_segment_buffer` must re-query the layout each
///     iteration and grow the buffer if the descriptor's range
///     extended.
///   - Or `ensure_cursor` must capture a layout-version cookie and
///     refuse to fill against a stale descriptor.
///
/// Today neither holds: the cursor commits to whatever
/// `segment_after_byte` returned at first call. This scaffold is
/// `#[ignore]`-marked because reproducing the parse-side panic
/// requires bespoke moof bytes — left as a TODO for the fix author.
/// The point of the scaffold is to make the missing contract
/// explicit so the fix has somewhere to land its regression test.
#[kithara::test]
#[ignore = "RED scaffold — needs crafted moof fixture to demonstrate \
            the parse_segment_frames panic. Add when implementing \
            the cursor-freshness contract."]
fn red_cursor_byte_range_freezes_when_layout_size_grows() {
    // Intentional placeholder. See the doc comment above for the
    // contract and the TODO list for the fix author.
    panic!("RED scaffold — see doc comment");
}

#[kithara::test]
fn seek_reads_only_init_and_target_segment() {
    let (blob, segmented) = build_test_layout(5);
    let (mut decoder, reads, record) = make_decoder(blob, segmented.clone());

    reads.lock().expect("BUG: clear").clear();
    record.store(true, Ordering::Release);

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
    assert!(landed_at >= Duration::from_secs(18) && landed_at < Duration::from_secs(24));
    let landed_byte = landed_byte.expect("BUG: landed_byte should be set");
    let segment_3 = &segmented.segments[3];
    assert_eq!(landed_byte, segment_3.byte_range.start);

    for _ in 0..16 {
        match decoder.next_chunk().expect("BUG: decode after seek") {
            DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Eof => break,
            DecoderChunkOutcome::Pending(_) => continue,
        }
    }
    record.store(false, Ordering::Release);

    let reads_snapshot = reads.lock().expect("BUG: reads lock").clone();
    let target_range = segment_3.byte_range.clone();
    for r in &reads_snapshot {
        assert!(
            r.start >= target_range.start && r.end <= target_range.end,
            "read {:?} fell outside target segment {:?} (prefix-walk regression)",
            r,
            target_range,
        );
    }
}
