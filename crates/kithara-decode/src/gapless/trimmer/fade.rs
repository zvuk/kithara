use kithara_signal::AudioChunk;

use super::buffer::chunk_frames;
use crate::consts;

/// Raised-cosine fade-in tracker.
///
/// The state is just a counter of how many frames have already been
/// shaped; the curve is generated on demand in [`FadeInState::apply`].
/// The total fade length is in *frames*, not samples — channels are
/// handled by the apply step itself.
#[derive(Debug, Clone, Copy)]
pub(super) struct FadeInState {
    applied_frames: u16,
    total_frames: u16,
}

impl FadeInState {
    /// Apply the next slice of the fade to `chunk`, modifying samples
    /// in place. The chunk may be shorter or longer than the remaining
    /// fade window; we only touch the prefix that still needs shaping.
    fn apply(&mut self, chunk: &mut AudioChunk) {
        if self.is_done() {
            return;
        }
        let frames = chunk_frames(chunk);
        if frames == 0 {
            return;
        }
        let channels = usize::from(chunk.spec().channels.max(1));
        let remaining = self.total_frames.saturating_sub(self.applied_frames);
        let to_shape = remaining.min(u16::try_from(frames).unwrap_or(u16::MAX));
        let total = f32::from(self.total_frames.max(1));
        let start_frame = self.applied_frames;
        let shape_samples = usize::from(to_shape) * channels;
        let prefix_len = shape_samples.min(chunk.samples.len());
        let prefix = &mut chunk.samples[..prefix_len];
        for (frame_offset, frame_samples) in prefix.chunks_exact_mut(channels).enumerate() {
            let frame = start_frame.saturating_add(u16::try_from(frame_offset).unwrap_or(u16::MAX));
            let position = f32::from(frame) / total;
            let gain = 0.5f32.mul_add(-(std::f32::consts::PI * position).cos(), 0.5);
            for sample in frame_samples {
                *sample *= gain;
            }
        }
        self.applied_frames = self.applied_frames.saturating_add(to_shape);
    }

    pub(super) fn for_sample_rate(sample_rate: u32) -> Self {
        let total_frames =
            u64::from(sample_rate.max(1)).saturating_mul(consts::FADE_IN_DURATION_MS) / 1000;
        let total_frames = u16::try_from(total_frames.clamp(1, 65_535)).unwrap_or(u16::MAX);
        Self {
            total_frames,
            applied_frames: 0,
        }
    }

    /// Returns true once the fade has finished — caller can drop the state.
    const fn is_done(self) -> bool {
        self.applied_frames >= self.total_frames
    }
}

pub(super) fn apply_fade_in(fade: &mut Option<FadeInState>, chunk: &mut AudioChunk) {
    let Some(state) = fade.as_mut() else {
        return;
    };
    state.apply(chunk);
    if state.is_done() {
        *fade = None;
    }
}
