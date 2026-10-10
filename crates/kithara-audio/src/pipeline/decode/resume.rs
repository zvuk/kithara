use kithara_signal::AudioChunk;

#[derive(Default)]
pub(crate) struct ResumeCursor {
    decode_head: Option<(u64, std::num::NonZeroU32)>,
    rendered_source_head: Option<crate::SourceEnd>,
}

impl ResumeCursor {
    pub(crate) fn clear(&mut self) {
        self.decode_head = None;
        self.rendered_source_head = None;
    }
    pub(crate) fn commit_source_end(&mut self, end: crate::SourceEnd) {
        self.rendered_source_head = Some(end);
    }
    pub(crate) fn decode_head(&self) -> Option<(u64, u32)> {
        self.decode_head.map(|(frame, rate)| (frame, rate.get()))
    }
    pub(crate) fn record(&mut self, chunk: &AudioChunk) {
        self.decode_head = Some((
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            chunk.spec().sample_rate,
        ));
    }
    pub(crate) fn source_end(&self) -> Option<crate::SourceEnd> {
        self.rendered_source_head.or_else(|| {
            self.decode_head
                .map(|(frame, rate)| crate::SourceEnd::new(frame, rate))
        })
    }
    pub(crate) fn rebase(&mut self, end: crate::SourceEnd) {
        self.decode_head = Some((end.frame(), end.sample_rate()));
        self.rendered_source_head = Some(end);
    }
}
