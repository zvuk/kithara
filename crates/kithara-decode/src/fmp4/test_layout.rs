use std::ops::Range;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{ByteMap, SegmentDescriptor};

#[derive(Clone)]
pub(crate) struct FakeSegmented {
    pub(crate) segments: Arc<Vec<SegmentDescriptor>>,
    init_range: Range<u64>,
}

impl ByteMap for FakeSegmented {
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
        let index = usize::try_from(segment_index).ok()?;
        self.segments.get(index).cloned()
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

pub(crate) fn read_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/hls")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

#[derive(Clone, Copy)]
pub(crate) enum TestLayoutCodec {
    Aac,
    Flac,
}

impl TestLayoutCodec {
    fn init_fixture(self) -> &'static str {
        match self {
            Self::Aac => "init-slq-a1.mp4",
            Self::Flac => "init-slossless-a1.mp4",
        }
    }

    fn segment_fixture(self, index: usize) -> String {
        match self {
            Self::Aac => format!("segment-{index}-slq-a1.m4s"),
            Self::Flac => format!("segment-{index}-slossless-a1.m4s"),
        }
    }
}

/// Stitch a codec-specific init segment + media segments into one byte
/// buffer and build a `FakeSegmented` over the resulting layout.
pub(crate) fn build_test_layout(
    codec: TestLayoutCodec,
    num_segments: usize,
) -> (Vec<u8>, FakeSegmented) {
    let init = read_fixture(codec.init_fixture());
    let init_len = u64::try_from(init.len()).expect("BUG: init length fits u64");

    let segment_duration_secs = 6u64;

    let mut blob = init;
    let mut descs = Vec::new();
    let mut byte_cursor = init_len;
    for i in 1..=num_segments {
        let seg_bytes = read_fixture(&codec.segment_fixture(i));
        let len = u64::try_from(seg_bytes.len()).expect("BUG: segment length fits u64");
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
