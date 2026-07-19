/// Algorithmic latency in the source and output coordinate spaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElasticLatency {
    output_frames: usize,
    source_frames: usize,
}

impl ElasticLatency {
    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) const fn new(source_frames: usize, output_frames: usize) -> Self {
        Self {
            output_frames,
            source_frames,
        }
    }

    /// Delayed output in frames.
    #[must_use]
    pub const fn output_frames(self) -> usize {
        self.output_frames
    }

    /// Required source history in frames.
    #[must_use]
    pub const fn source_frames(self) -> usize {
        self.source_frames
    }
}
