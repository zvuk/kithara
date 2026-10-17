use std::ops::Range;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{ByteMap, SegmentDescriptor};
use kithara_test_utils::kithara;

#[derive(Clone)]
pub(crate) struct FakeSegmented {
    pub(crate) segments: Arc<Vec<SegmentDescriptor>>,
    init_range: Range<u64>,
}

impl FakeSegmented {
    /// The same segments described with a playlist duration of `extinf`
    /// each, so the running EXTINF sum drifts from the segments' own times.
    pub(crate) fn with_extinf(self, extinf: Duration) -> Self {
        let segments = self
            .segments
            .iter()
            .map(|desc| {
                let mut desc = desc.clone();
                desc.decode_time = extinf * desc.segment_index;
                desc.duration = extinf;
                desc
            })
            .collect();
        Self {
            segments: Arc::new(segments),
            ..self
        }
    }
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
    let route = format!("/hls/{name}");
    let resource = kithara_test_fixtures::hls::long_plain()
        .get(&route)
        .unwrap_or_else(|| panic!("generated HLS fixture has no `{route}`"));
    std::fs::read(resource.path())
        .unwrap_or_else(|error| panic!("read {}: {error}", resource.path().display()))
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
        descs.push(
            SegmentDescriptor::builder()
                .byte_range(start..end)
                .decode_time(Duration::from_secs(
                    u64::from(seg_index) * segment_duration_secs,
                ))
                .duration(Duration::from_secs(segment_duration_secs))
                .segment_index(seg_index)
                .variant_index(0)
                .build(),
        );
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

#[kithara::fixture]
pub(crate) fn aac_one() -> (Vec<u8>, FakeSegmented) {
    build_test_layout(TestLayoutCodec::Aac, 1)
}
#[kithara::fixture]
pub(crate) fn aac_three() -> (Vec<u8>, FakeSegmented) {
    build_test_layout(TestLayoutCodec::Aac, 3)
}
#[kithara::fixture]
pub(crate) fn aac_five() -> (Vec<u8>, FakeSegmented) {
    build_test_layout(TestLayoutCodec::Aac, 5)
}
#[kithara::fixture]
pub(crate) fn flac_three() -> (Vec<u8>, FakeSegmented) {
    build_test_layout(TestLayoutCodec::Flac, 3)
}
