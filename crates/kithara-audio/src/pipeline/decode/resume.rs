use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo};

#[derive(Clone, Copy)]
struct DecodedBoundary {
    source: AudioChunkInfo,
    native_end: Duration,
}

#[derive(Default)]
pub(crate) struct ResumeCursor {
    decoded: Option<DecodedBoundary>,
    rendered_source_head: Option<crate::SourceEnd>,
}

impl ResumeCursor {
    pub(crate) fn clear(&mut self) {
        self.decoded = None;
        self.rendered_source_head = None;
    }
    pub(crate) fn commit_source_end(&mut self, end: crate::SourceEnd) {
        self.rendered_source_head = Some(end);
    }
    pub(crate) fn decode_head(&self) -> Option<(u64, u32)> {
        self.decoded.map_or_else(
            || {
                self.rendered_source_head
                    .map(|end| (end.frame(), end.sample_rate().get()))
            },
            |boundary| {
                let meta = boundary.source;
                Some((
                    meta.frame_offset.saturating_add(u64::from(meta.frames)),
                    meta.spec.sample_rate.get(),
                ))
            },
        )
    }
    pub(crate) fn record(&mut self, chunk: &AudioChunk, native_end: Duration) {
        self.decoded = Some(DecodedBoundary {
            source: chunk.meta,
            native_end,
        });
    }
    pub(crate) fn decoded_end(&self, source_revision: u64) -> Option<Duration> {
        self.decoded
            .filter(|boundary| boundary.source.source_revision == source_revision)
            .map(|boundary| boundary.native_end)
    }
    pub(crate) fn source_end(&self) -> Option<crate::SourceEnd> {
        self.rendered_source_head.or_else(|| {
            self.decoded.map(|boundary| {
                let meta = boundary.source;
                meta.source_span.map_or_else(
                    || crate::SourceEnd::new(meta.frame_range().end, meta.spec.sample_rate),
                    |span| {
                        crate::SourceEnd::new(span.end(), span.sample_rate())
                            .with_mapping_revision(span.mapping_revision())
                    },
                )
            })
        })
    }
    pub(crate) fn rebase(&mut self, end: crate::SourceEnd) {
        self.decoded = None;
        self.rendered_source_head = Some(end);
    }
}
