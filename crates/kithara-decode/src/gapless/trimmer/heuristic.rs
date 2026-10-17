use kithara_signal::AudioChunk;

use super::{
    buffer::{BufferedChunks, trim_leading},
    fade::{FadeInState, apply_fade_in, apply_trailing_fade_out},
    silence::{find_leading_trim_frames, trailing_silent_frames},
};
use crate::{DecodeResult, GaplessOutput, gapless::heuristic::SilenceTrimParams};

#[derive(Debug)]
pub(super) struct HeuristicState {
    /// Fade-in applied to the first frames after a successful leading
    /// trim. `None` while we're still buffering or if no trim happened.
    fade_in: Option<FadeInState>,
    params: SilenceTrimParams,
    /// Buffered chunks while we look for the first non-silent frame.
    /// Once the search ends, the buffer is drained into `tail_buffer`
    /// (with leading frames trimmed) and never refilled.
    leading_buffer: BufferedChunks,
    leading_enabled: bool,
    /// Pre-computed linear amplitude floor — recomputing on every
    /// frame would be wasteful and `params` is immutable for the
    /// lifetime of the trimmer.
    silence_threshold_amp: f32,
}

impl HeuristicState {
    pub(super) fn new(params: SilenceTrimParams) -> Self {
        let silence_threshold_amp = params.threshold_amplitude();
        Self {
            params,
            silence_threshold_amp,
            leading_buffer: BufferedChunks::default(),
            leading_enabled: true,
            fade_in: None,
        }
    }

    pub(super) fn notify_seek(&mut self) {
        self.leading_buffer.clear();
        self.leading_enabled = false;
        self.fade_in = None;
    }

    pub(super) fn push(
        &mut self,
        tail: &mut BufferedChunks,
        trailing_frames: u64,
        chunk: AudioChunk,
    ) -> DecodeResult<GaplessOutput> {
        if !self.leading_enabled {
            return Ok(self.forward_post_leading(tail, trailing_frames, chunk));
        }
        self.leading_buffer.push(chunk);
        if let Some(trim_frames) = find_leading_trim_frames(
            self.leading_buffer.chunks(),
            &self.params,
            self.silence_threshold_amp,
        ) {
            self.leading_enabled = false;
            if trim_frames > 0 {
                self.arm_fade_in();
            }
            return self.drain_leading_buffer(tail, trailing_frames, trim_frames);
        }
        if self.leading_buffer.frames() >= self.params.scan_window_frames {
            self.leading_enabled = false;
            return self.drain_leading_buffer(tail, trailing_frames, 0);
        }
        Ok(GaplessOutput::new())
    }

    fn forward_post_leading(
        &mut self,
        tail: &mut BufferedChunks,
        trailing_frames: u64,
        mut chunk: AudioChunk,
    ) -> GaplessOutput {
        apply_fade_in(&mut self.fade_in, &mut chunk);
        tail.push(chunk);
        tail.release(trailing_frames)
    }

    pub(super) fn flush(
        &mut self,
        tail: &mut BufferedChunks,
        trailing_frames: u64,
    ) -> DecodeResult<GaplessOutput> {
        let mut ready = GaplessOutput::new();
        if self.leading_enabled {
            let trim_frames = find_leading_trim_frames(
                self.leading_buffer.chunks(),
                &self.params,
                self.silence_threshold_amp,
            )
            .unwrap_or(0);
            self.leading_enabled = false;
            if trim_frames > 0 {
                self.arm_fade_in();
            }
            ready.extend(self.drain_leading_buffer(tail, trailing_frames, trim_frames)?);
        }
        if self.params.trim_trailing {
            let silent_suffix = trailing_silent_frames(tail.chunks(), self.silence_threshold_amp);
            if silent_suffix > 0
                && silent_suffix < tail.frames()
                && silent_suffix >= self.params.min_trim_frames
            {
                tail.trim(silent_suffix)?;
                let sample_rate = tail
                    .chunks()
                    .last()
                    .map_or(0, |chunk| chunk.spec().sample_rate.get());
                apply_trailing_fade_out(tail.chunks_mut(), sample_rate);
            }
        }
        ready.extend(tail.drain());
        Ok(ready)
    }

    fn arm_fade_in(&mut self) {
        let sample_rate = self
            .leading_buffer
            .chunks()
            .first()
            .map_or(0, |chunk| chunk.spec().sample_rate.get());
        self.fade_in = Some(FadeInState::for_sample_rate(sample_rate));
    }

    fn drain_leading_buffer(
        &mut self,
        tail: &mut BufferedChunks,
        trailing_frames: u64,
        mut trim_frames: u64,
    ) -> DecodeResult<GaplessOutput> {
        for chunk in self.leading_buffer.take() {
            let Some(mut chunk) = trim_leading(chunk, &mut trim_frames)? else {
                continue;
            };
            apply_fade_in(&mut self.fade_in, &mut chunk);
            tail.push(chunk);
        }
        Ok(tail.release(trailing_frames))
    }
}
