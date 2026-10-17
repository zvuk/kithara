use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;

use super::decoder::{ResampledDecoder, round_scaled_frames};
use crate::{DecodeResult, GaplessInfo};

impl<B, S> ResampledDecoder<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
{
    pub(super) fn native_duration_frames(&self, declared: crate::DecoderDuration) -> Option<u64> {
        Some(match declared {
            crate::DecoderDuration::RawLength(length) => self.source_spec.frame_at(length).ok()?,
            crate::DecoderDuration::MediaEnd(end) => {
                let raw_end = self.decoder.media_origin()?.checked_add(end)?;
                let origin = self.decoder.initial_pcm_origin()?.position();
                self.source_spec
                    .frame_at(raw_end.checked_sub(origin)?)
                    .ok()?
            }
        })
    }

    pub(super) fn scaled_gapless(&self, info: GaplessInfo) -> DecodeResult<GaplessInfo> {
        let source_rate = self.source_spec.sample_rate.get();
        let target_rate = self.config.target_sample_rate.get();
        Ok(GaplessInfo {
            leading_frames: round_scaled_frames(info.leading_frames, source_rate, target_rate)?,
            trailing_frames: round_scaled_frames(info.trailing_frames, source_rate, target_rate)?,
        })
    }
}
