use kithara_signal::AudioChunk;
use smallvec::SmallVec;

use crate::{DecodeResult, GaplessInfo, GaplessTailCompensation};

/// Inline batch of chunks released by one `GaplessTrimmer` operation.
pub type GaplessOutput = SmallVec<[AudioChunk; 2]>;
use super::{
    buffer::{BufferedChunks, chunk_frames, output_with, trim_leading, validate_geometry},
    fade::{FadeInState, apply_fade_in},
};

/// Stateful PCM trimmer that applies one track's gapless contract.
#[derive(Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub struct GaplessTrimmer {
    mode: GaplessMode,
    #[field(with, set)]
    tail_compensation: Option<GaplessTailCompensation>,
    tail_buffer: BufferedChunks,
    input_frames_seen: u64,
    /// Metadata-driven trailing trim held until EOF.
    trailing_frames: u64,
}

#[derive(Debug, Default)]
enum GaplessMode {
    #[default]
    Disabled,
    Fixed {
        leading_remaining: u64,
        /// Click-suppression fade applied to the first `consts::FADE_IN_DURATION_MS`
        /// of audio that survives the leading trim. `None` for
        /// metadata-driven trim — that boundary is sample-exact.
        fade_in: Option<FadeInState>,
    },
}

impl GaplessTrimmer {
    /// Build a trimmer that drops a fixed number of leading frames
    /// looked up from a codec table. The boundary is by definition
    /// approximate, so a short raised-cosine fade-in is applied to
    /// the first frames of audible output to avoid clicks.
    ///
    /// `sample_rate` is needed to size the fade-in in frames.
    #[must_use]
    pub fn codec_priming(leading_frames: u64, sample_rate: u32) -> Self {
        if leading_frames == 0 {
            return Self::disabled();
        }
        Self {
            mode: GaplessMode::Fixed {
                leading_remaining: leading_frames,
                fade_in: Some(FadeInState::for_sample_rate(sample_rate)),
            },
            trailing_frames: 0,
            tail_compensation: None,
            input_frames_seen: 0,
            tail_buffer: BufferedChunks::default(),
        }
    }

    fn compensated_trailing_frames(&self) -> u64 {
        let Some(compensation) = self.tail_compensation else {
            return self.trailing_frames;
        };
        let deficit = compensation.deficit_frames(self.input_frames_seen);
        if deficit > 1 {
            debug_assert!(
                deficit <= 1,
                "gapless tail deficit exceeded one frame: deficit={deficit}, ideal={}, actual={}",
                compensation.ideal_pre_trim_frames(),
                self.input_frames_seen
            );
            tracing::warn!(
                deficit,
                ideal = compensation.ideal_pre_trim_frames(),
                actual = self.input_frames_seen,
                "gapless tail deficit exceeded one frame; bounding trailing trim compensation"
            );
        }
        self.trailing_frames.saturating_sub(deficit.min(1))
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// # Errors
    /// Returns invalid geometry when retained PCM cannot be mapped.
    pub fn flush(&mut self) -> DecodeResult<GaplessOutput> {
        match &mut self.mode {
            GaplessMode::Disabled => Ok(GaplessOutput::new()),
            GaplessMode::Fixed { .. } => {
                let trailing_frames = self.compensated_trailing_frames();
                self.tail_buffer.trim(trailing_frames)?;
                Ok(self.tail_buffer.drain())
            }
        }
    }

    /// Drop leading trim and the codec-priming fade after a seek.
    ///
    /// Call outside the non-blocking produce core: releasing a pooled buffer can deallocate
    /// when its shard is full.
    pub fn notify_seek(&mut self) {
        match &mut self.mode {
            GaplessMode::Disabled => {}
            GaplessMode::Fixed {
                leading_remaining,
                fade_in,
            } => {
                *leading_remaining = 0;
                *fade_in = None;
            }
        }
        self.tail_compensation = None;
        self.input_frames_seen = 0;
        self.tail_buffer.clear();
    }

    /// # Errors
    /// Rejects mapped metadata whose extent differs from the supplied PCM.
    pub fn push(&mut self, chunk: AudioChunk) -> DecodeResult<GaplessOutput> {
        validate_geometry(&chunk)?;
        if matches!(self.mode, GaplessMode::Fixed { .. }) {
            self.input_frames_seen = self.input_frames_seen.saturating_add(chunk_frames(&chunk));
        }
        match &mut self.mode {
            GaplessMode::Disabled => Ok(output_with(chunk)),
            GaplessMode::Fixed {
                leading_remaining,
                fade_in,
            } => {
                let Some(mut chunk) =
                    trim_leading(chunk, leading_remaining)?.filter(|chunk| chunk_frames(chunk) > 0)
                else {
                    return Ok(GaplessOutput::new());
                };
                apply_fade_in(fade_in, &mut chunk);

                self.tail_buffer.push(chunk);
                Ok(self.tail_buffer.release(self.trailing_frames))
            }
        }
    }
}

impl From<GaplessInfo> for GaplessTrimmer {
    fn from(info: GaplessInfo) -> Self {
        let enabled = info.leading_frames > 0 || info.trailing_frames > 0;
        Self {
            mode: if enabled {
                GaplessMode::Fixed {
                    leading_remaining: info.leading_frames,
                    fade_in: None,
                }
            } else {
                GaplessMode::Disabled
            },
            trailing_frames: info.trailing_frames,
            tail_compensation: None,
            input_frames_seen: 0,
            tail_buffer: BufferedChunks::default(),
        }
    }
}
