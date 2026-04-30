//! End-to-end test of the segment-aware decoder pipeline.

use std::{
    io::{Cursor, Seek, SeekFrom},
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use kithara_stream::{SegmentDescriptor, SegmentedSource};
use kithara_test_utils::kithara;

use crate::{
    DecoderConfig,
    backend::BoxedSource,
    fmp4_segment::{codec_symphonia::SymphoniaAacCodec, decoder::Fmp4SegmentDecoder},
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
};

/// Fixed-layout in-memory test source built from init+segment fixtures.
/// Records every absolute byte offset hit by `Read::read` so tests can
/// assert no-prefix-read invariants.
struct InstrumentedSource {
    inner: Cursor<Vec<u8>>,
    reads: Arc<Mutex<Vec<Range<u64>>>>,
    record: Arc<AtomicBool>,
}

impl std::io::Read for InstrumentedSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.position();
        let n = self.inner.read(buf)?;
        if self.record.load(Ordering::Acquire) && n > 0 {
            self.reads
                .lock()
                .expect("reads lock")
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
    init_range: Range<u64>,
    segments: Arc<Vec<SegmentDescriptor>>,
}

impl SegmentedSource for FakeSegmented {
    fn init_segment_range(&self) -> Option<Range<u64>> {
        Some(self.init_range.clone())
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

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        for desc in self.segments.iter() {
            if desc.byte_range.start >= byte_offset {
                return Some(desc.clone());
            }
        }
        None
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

    // EXTINF for the test fixture is ~6s per segment.
    let segment_duration_secs = 6u64;

    let mut blob = init.clone();
    let mut descs = Vec::new();
    let mut byte_cursor = init_len;
    for i in 1..=num_segments {
        let seg_bytes = read_fixture(&format!("segment-{i}-slq-a1.m4s"));
        let len = seg_bytes.len() as u64;
        let start = byte_cursor;
        let end = start + len;
        let seg_index = u32::try_from(i - 1).expect("segment index fits u32");
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
    Fmp4SegmentDecoder<SymphoniaAacCodec>,
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
    let segmented_arc: Arc<dyn SegmentedSource> = Arc::new(segmented);
    let config = DecoderConfig::default();
    let decoder = Fmp4SegmentDecoder::<SymphoniaAacCodec>::new(source, segmented_arc, &config)
        .expect("build decoder");
    (decoder, reads, record)
}

#[kithara::test]
fn next_chunk_yields_pcm_from_init_plus_segment_zero() {
    let (blob, segmented) = build_test_layout(1);
    let (mut decoder, _, _) = make_decoder(blob, segmented);

    // First non-empty chunk should appear within a few iterations
    // (Symphonia AAC may emit a zero-frame priming chunk).
    let mut got_chunk = None;
    for _ in 0..16 {
        match decoder.next_chunk().expect("decode") {
            DecoderChunkOutcome::Chunk(chunk) => {
                got_chunk = Some(chunk);
                break;
            }
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => break,
        }
    }
    let chunk = got_chunk.expect("at least one PCM chunk from segment 0");
    assert!(chunk.frames() > 0);
    assert!(chunk.spec().sample_rate >= 8_000);
    assert!(chunk.spec().channels >= 1);
}

#[kithara::test]
fn seek_reads_only_init_and_target_segment() {
    let (blob, segmented) = build_test_layout(5);
    let (mut decoder, reads, record) = make_decoder(blob, segmented.clone());

    // Init was already loaded during decoder construction; clear the
    // history and start recording from this point on so the asserts
    // measure only post-seek reads.
    reads.lock().expect("clear").clear();
    record.store(true, Ordering::Release);

    // Seek into segment 3 (decode_time = 18s).
    let target = Duration::from_secs(18);
    let outcome = decoder.seek(target).expect("seek");
    let DecoderSeekOutcome::Landed {
        landed_at,
        landed_byte,
        ..
    } = outcome
    else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert!(landed_at >= Duration::from_secs(18) && landed_at < Duration::from_secs(24));
    let landed_byte = landed_byte.expect("landed_byte should be set");
    let segment_3 = &segmented.segments[3];
    assert_eq!(landed_byte, segment_3.byte_range.start);

    // Pull one chunk to force the segment to actually be loaded.
    for _ in 0..16 {
        match decoder.next_chunk().expect("decode after seek") {
            DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Eof => break,
            DecoderChunkOutcome::Pending(_) => continue,
        }
    }
    record.store(false, Ordering::Release);

    // All recorded reads must be inside segment 3's byte range —
    // never inside segment 0/1/2's ranges.
    let reads_snapshot = reads.lock().expect("reads lock").clone();
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
