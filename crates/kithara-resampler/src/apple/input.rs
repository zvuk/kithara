use kithara_apple::{
    accelerate,
    audio_toolbox::{
        AUDIO_CONVERTER_ERR_NO_DATA_NOW, AudioConverterInput, AudioConverterInputRequest, NO_ERR,
        OSStatus, PARAM_ERR,
    },
};
use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use smallvec::SmallVec;

use crate::ResamplerError;

pub(super) struct AppleResamplerInputState {
    staged: SmallVec<[PcmBuf; 8]>,
    channels: usize,
    frames: usize,
    offset: usize,
    consumed: usize,
    eos: bool,
}

impl AppleResamplerInputState {
    pub(super) fn new(
        channels: usize,
        chunk_size: usize,
        pcm_pool: &PcmPool,
    ) -> Result<Self, BudgetExhausted> {
        let mut staged = SmallVec::new();
        for _ in 0..channels {
            let mut buffer = pcm_pool.get();
            buffer.ensure_len(chunk_size)?;
            buffer.clear();
            staged.push(buffer);
        }
        Ok(Self {
            staged,
            channels,
            frames: 0,
            offset: 0,
            consumed: 0,
            eos: false,
        })
    }

    pub(super) fn clear(&mut self) {
        for channel in &mut self.staged {
            channel.clear();
        }
        self.frames = 0;
        self.offset = 0;
        self.consumed = 0;
        self.eos = false;
    }

    pub(super) fn consumed(&self) -> usize {
        self.consumed
    }

    fn remaining(&self) -> usize {
        self.frames.saturating_sub(self.offset)
    }

    pub(super) fn stage(
        &mut self,
        input: &[&[f32]],
        chunk_size: usize,
        eos: bool,
    ) -> Result<(), ResamplerError> {
        let frames = validate_input(input, self.channels, chunk_size)?;
        for (staged, source) in self.staged.iter_mut().zip(input.iter()) {
            staged.clear();
            staged.ensure_len(source.len())?;
            accelerate::copy_f32(source, &mut staged[..source.len()]);
        }
        self.frames = frames;
        self.offset = 0;
        self.consumed = 0;
        self.eos = eos;
        Ok(())
    }

    pub(super) fn stage_empty_eos(&mut self) {
        for channel in &mut self.staged {
            channel.clear();
        }
        self.frames = 0;
        self.offset = 0;
        self.consumed = 0;
        self.eos = true;
    }
}

impl AudioConverterInput for AppleResamplerInputState {
    fn fill(&mut self, mut request: AudioConverterInputRequest<'_>) -> OSStatus {
        let requested = usize::try_from(*request.packets).unwrap_or(usize::MAX);
        let remaining = self.remaining();
        if remaining == 0 || requested == 0 {
            *request.packets = 0;
            request.packet_descriptions.clear();
            return if self.eos {
                NO_ERR
            } else {
                AUDIO_CONVERTER_ERR_NO_DATA_NOW
            };
        }

        let frames = remaining.min(requested);
        let Ok(frame_count) = u32::try_from(frames) else {
            return PARAM_ERR;
        };

        if request.buffers.set_number_buffers(self.channels).is_err() {
            return PARAM_ERR;
        }
        for (channel_idx, channel) in self.staged.iter().enumerate().take(self.channels) {
            let Some(samples) = channel.get(self.offset..self.offset.saturating_add(frames)) else {
                return PARAM_ERR;
            };
            if request
                .buffers
                .set_planar_f32_input(channel_idx, samples)
                .is_err()
            {
                return PARAM_ERR;
            }
        }
        *request.packets = frame_count;
        request.packet_descriptions.clear();

        self.offset = self.offset.saturating_add(frames);
        self.consumed = self.consumed.saturating_add(frames);
        NO_ERR
    }
}

fn validate_input(
    input: &[&[f32]],
    channels: usize,
    chunk_size: usize,
) -> Result<usize, ResamplerError> {
    if input.len() != channels {
        return Err(ResamplerError::InvalidBuffer {
            detail: "input channel count mismatch",
        });
    }
    let frames =
        input
            .first()
            .map(|channel| channel.len())
            .ok_or(ResamplerError::InvalidBuffer {
                detail: "missing input channel",
            })?;
    if frames > chunk_size {
        return Err(ResamplerError::InvalidBuffer {
            detail: "input frame count exceeds adapter quantum",
        });
    }
    if input.iter().any(|channel| channel.len() != frames) {
        return Err(ResamplerError::InvalidBuffer {
            detail: "input channel lengths differ",
        });
    }
    Ok(frames)
}
