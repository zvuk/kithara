use kithara_decode::PcmChunk;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceEnd {
    pub(crate) frame: u64,
    pub(crate) rate: u32,
}

#[derive(Default)]
pub(crate) struct SourceWindow {
    admitted: Option<SourceEnd>,
}

impl SourceWindow {
    pub(crate) fn admit(&mut self, chunk: PcmChunk) -> PcmChunk {
        self.admitted = Some(SourceEnd {
            frame: chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            rate: chunk.meta.spec.sample_rate.get(),
        });
        chunk
    }

    pub(crate) fn emitted(&self, held_source_frames: u64) -> Option<SourceEnd> {
        self.admitted.map(|admitted| SourceEnd {
            frame: admitted.frame.saturating_sub(held_source_frames),
            ..admitted
        })
    }

    pub(crate) fn clear(&mut self) {
        self.admitted = None;
    }
}
