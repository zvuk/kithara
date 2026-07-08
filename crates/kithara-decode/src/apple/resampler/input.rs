use std::{ffi::c_void, ptr};

use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use kithara_resampler::ResamplerError;
use smallvec::SmallVec;

use super::{buffer::audio_buffer_ptr, channel_byte_len};
use crate::apple::{
    consts::Consts,
    ffi::{
        AudioBuffer, AudioBufferList, AudioConverterRef, AudioStreamPacketDescription, OSStatus,
        UInt32,
    },
};

const PARAM_ERR: OSStatus = -50;

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
            staged[..source.len()].copy_from_slice(source);
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

pub(super) extern "C" fn apple_resampler_input_callback(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    if io_num_packets.is_null() || io_data.is_null() || user_data.is_null() {
        return PARAM_ERR;
    }

    // SAFETY: CoreAudio calls this with the same pointer supplied to
    // `AudioConverterFillComplexBuffer`, which points at live boxed state.
    let state = unsafe { &mut *(user_data as *mut AppleResamplerInputState) };
    // SAFETY: checked non-null above.
    let requested = unsafe { usize::try_from(*io_num_packets).unwrap_or(usize::MAX) };
    let remaining = state.remaining();
    if remaining == 0 || requested == 0 {
        // SAFETY: checked non-null above; output packet descriptions are unused for PCM.
        unsafe {
            *io_num_packets = 0;
            if !out_packet_desc.is_null() {
                *out_packet_desc = ptr::null_mut();
            }
        }
        return if state.eos {
            Consts::noErr
        } else {
            Consts::kAudioConverterErr_NoDataNow
        };
    }

    let frames = remaining.min(requested);
    let Ok(frame_count) = UInt32::try_from(frames) else {
        return PARAM_ERR;
    };
    let Ok(byte_len) = channel_byte_len(frames) else {
        return PARAM_ERR;
    };
    let Ok(channel_count) = UInt32::try_from(state.channels) else {
        return PARAM_ERR;
    };

    // SAFETY: `io_data` is a CoreAudio-provided AudioBufferList for the
    // converter's non-interleaved input ASBD; CoreAudio sizes it for the
    // channel count declared at converter construction.
    unsafe {
        (*io_data).mNumberBuffers = channel_count;
        for (channel_idx, channel) in state.staged.iter().enumerate().take(state.channels) {
            let data = channel.as_ptr().add(state.offset).cast::<c_void>();
            audio_buffer_ptr(io_data, channel_idx).write(AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: byte_len,
                mData: data.cast_mut(),
            });
        }
        *io_num_packets = frame_count;
        if !out_packet_desc.is_null() {
            *out_packet_desc = ptr::null_mut();
        }
    }

    state.offset = state.offset.saturating_add(frames);
    state.consumed = state.consumed.saturating_add(frames);
    Consts::noErr
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
