use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ffi::c_void,
    mem::{align_of, size_of},
    ptr::{self, NonNull},
};

use super::{channel_byte_len, constants};
use crate::{
    apple::ffi::{AudioBuffer, AudioBufferList},
    resampler::{ResamplerBuildError, ResamplerError},
};

pub(super) struct PlanarAudioBufferList {
    layout: Layout,
    ptr: NonNull<AudioBufferList>,
    channels: usize,
}

impl PlanarAudioBufferList {
    pub(super) fn new(channels: usize) -> Result<Self, ResamplerBuildError> {
        let layout = buffer_list_layout(channels)?;
        // SAFETY: `layout` is non-zero and was computed for an AudioBufferList
        // header followed by `channels` AudioBuffer entries.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw.cast::<AudioBufferList>()).ok_or_else(|| {
            ResamplerBuildError::apple_config("audio buffer list allocation failed")
        })?;
        let mut list = Self {
            layout,
            ptr,
            channels,
        };
        // SAFETY: `list.ptr` owns enough initialized storage for the header.
        unsafe {
            (*list.ptr.as_ptr()).mNumberBuffers = u32::try_from(channels).map_err(|_| {
                ResamplerBuildError::apple_config("channel count exceeds CoreAudio limit")
            })?;
        }
        list.clear_buffers();
        Ok(list)
    }

    pub(super) fn as_mut_ptr(&mut self) -> *mut AudioBufferList {
        self.ptr.as_ptr()
    }

    unsafe fn buffer_ptr(&mut self, channel: usize) -> *mut AudioBuffer {
        // SAFETY: caller guarantees `channel` is within the allocated buffer array.
        unsafe {
            self.ptr
                .as_ptr()
                .cast::<u8>()
                .add(constants::AUDIO_BUFFER_LIST_HEADER_BYTES)
                .cast::<AudioBuffer>()
                .add(channel)
        }
    }

    fn clear_buffers(&mut self) {
        for channel in 0..self.channels {
            // SAFETY: `channel < self.channels`; storage was allocated with
            // one AudioBuffer slot per channel.
            unsafe {
                self.buffer_ptr(channel).write(AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: 0,
                    mData: ptr::null_mut(),
                });
            }
        }
    }

    pub(super) fn set_output(
        &mut self,
        output: &mut [Vec<f32>],
        output_frames: usize,
    ) -> Result<(), ResamplerError> {
        for (channel_idx, channel) in output.iter_mut().enumerate().take(self.channels) {
            let bytes = channel_byte_len(output_frames)?;
            // SAFETY: `channel_idx < self.channels`; `channel` has exactly
            // `output_frames` samples by caller validation.
            unsafe {
                self.buffer_ptr(channel_idx).write(AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: channel.as_mut_ptr().cast::<c_void>(),
                });
            }
        }
        Ok(())
    }
}

impl Drop for PlanarAudioBufferList {
    fn drop(&mut self) {
        // SAFETY: `ptr` was allocated with this exact layout in `new`.
        unsafe {
            dealloc(self.ptr.as_ptr().cast::<u8>(), self.layout);
        }
    }
}

pub(super) unsafe fn audio_buffer_ptr(
    list: *mut AudioBufferList,
    channel: usize,
) -> *mut AudioBuffer {
    // SAFETY: caller guarantees `list` points to an AudioBufferList with at
    // least `channel + 1` AudioBuffer entries.
    unsafe {
        list.cast::<u8>()
            .add(constants::AUDIO_BUFFER_LIST_HEADER_BYTES)
            .cast::<AudioBuffer>()
            .add(channel)
    }
}

fn buffer_list_layout(channels: usize) -> Result<Layout, ResamplerBuildError> {
    let buffer_bytes = size_of::<AudioBuffer>()
        .checked_mul(channels)
        .ok_or_else(|| ResamplerBuildError::apple_config("audio buffer list size overflow"))?;
    let total_bytes = constants::AUDIO_BUFFER_LIST_HEADER_BYTES
        .checked_add(buffer_bytes)
        .ok_or_else(|| ResamplerBuildError::apple_config("audio buffer list size overflow"))?;
    Layout::from_size_align(total_bytes, align_of::<AudioBufferList>())
        .map_err(|_| ResamplerBuildError::apple_config("invalid audio buffer list layout"))
}
