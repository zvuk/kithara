//! Pre-resampler time-stretch slot. Passthrough until a DSP backend
//! lands

use kithara_decode::PcmChunk;

use crate::traits::AudioEffect;

/// Source-domain time-stretch slot. Passthrough until a backend lands.
#[derive(Default)]
pub struct TimeStretchProcessor;

impl TimeStretchProcessor {
    /// Create the (currently passthrough) slot.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl AudioEffect for TimeStretchProcessor {
    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        Some(chunk)
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;

    use super::*;

    fn chunk() -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels: 2,
                    sample_rate: 44_100,
                },
                frames: 2,
                frame_offset: 128,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.1, -0.2, 0.3, -0.4]),
        )
    }

    #[kithara::test]
    fn passthrough_preserves_samples_and_meta() {
        let mut fx = TimeStretchProcessor::new();
        let input = chunk();
        let out = fx.process(input.clone()).expect("passthrough emits");
        assert_eq!(&out.pcm[..], &input.pcm[..]);
        assert_eq!(out.meta, input.meta, "meta carried verbatim");
        assert!(fx.flush().is_none());
    }
}
