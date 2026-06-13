//! Pre-resampler time-stretch slot. Passthrough until a DSP backend
//! lands

use kithara_decode::PcmChunk;

use crate::traits::AudioEffect;

/// Source-domain time-stretch slot. Passthrough until a backend lands.
#[derive(Default)]
pub struct TimeStretchProcessor;

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
    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;

    use super::*;

    fn chunk() -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec: PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
                frames: 2,
                frame_offset: 128,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.1, -0.2, 0.3, -0.4]),
        )
    }

    #[kithara::test]
    fn passthrough_preserves_samples_and_meta() {
        let mut fx = TimeStretchProcessor;
        let input = chunk();
        let out = fx.process(input.clone()).expect("passthrough emits");
        assert_eq!(&out.samples[..], &input.samples[..]);
        assert_eq!(out.meta, input.meta, "meta carried verbatim");
        assert!(fx.flush().is_none());
    }
}
