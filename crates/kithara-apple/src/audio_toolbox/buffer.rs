use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ffi::c_void,
    marker::PhantomData,
    mem::{align_of, size_of},
    ptr::{self, NonNull},
};

use super::{
    error::AudioToolboxError,
    sys::{AudioBuffer, AudioBufferList, BYTES_PER_F32_SAMPLE, UInt32},
};

const AUDIO_BUFFER_LIST_HEADER_BYTES: usize =
    size_of::<AudioBufferList>() - size_of::<AudioBuffer>();

pub trait AudioBufferListTarget {
    fn as_audio_buffer_list_mut_ptr(&mut self) -> *mut AudioBufferList;
}

pub struct OwnedAudioBufferList {
    layout: Layout,
    ptr: NonNull<AudioBufferList>,
    capacity: usize,
}

// SAFETY: OwnedAudioBufferList owns its allocation and only exposes &mut mutation.
// SAFETY: It carries no thread-affine platform handle.
unsafe impl Send for OwnedAudioBufferList {}

impl OwnedAudioBufferList {
    /// Allocates an `AudioBufferList` with `capacity` channel entries.
    ///
    /// # Errors
    ///
    /// Returns a config error when the layout overflows or allocation fails.
    pub fn new(capacity: usize) -> Result<Self, AudioToolboxError> {
        let layout = buffer_list_layout(capacity)?;
        // SAFETY: layout was computed for the header plus `capacity` buffers.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw.cast::<AudioBufferList>())
            .ok_or_else(|| AudioToolboxError::config("audio buffer list allocation failed"))?;
        let mut list = Self {
            layout,
            ptr,
            capacity,
        };
        list.writer().set_number_buffers(capacity)?;
        list.clear_buffers();
        Ok(list)
    }

    fn clear_buffers(&mut self) {
        let capacity = self.capacity;
        let mut writer = self.writer();
        for channel in 0..capacity {
            writer.clear_buffer(channel);
        }
    }

    /// Points every channel entry at a planar f32 output slice.
    ///
    /// # Errors
    ///
    /// Returns a config error when frame counts or byte sizes exceed Core Audio limits.
    pub fn set_planar_f32_output(
        &mut self,
        output: &mut [&mut [f32]],
        frames: usize,
    ) -> Result<(), AudioToolboxError> {
        let capacity = self.capacity;
        let mut writer = self.writer();
        writer.set_number_buffers(capacity)?;
        for (channel_idx, channel) in output.iter_mut().enumerate().take(capacity) {
            writer.set_planar_f32_output(channel_idx, &mut channel[..frames])?;
        }
        Ok(())
    }

    pub fn writer(&mut self) -> AudioBufferListWriter<'_> {
        AudioBufferListWriter {
            ptr: self.ptr,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }
}

impl AudioBufferListTarget for OwnedAudioBufferList {
    fn as_audio_buffer_list_mut_ptr(&mut self) -> *mut AudioBufferList {
        self.ptr.as_ptr()
    }
}

impl Drop for OwnedAudioBufferList {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with this exact layout in new.
        unsafe {
            dealloc(self.ptr.as_ptr().cast::<u8>(), self.layout);
        }
    }
}

pub struct SingleAudioBufferList<'a> {
    raw: AudioBufferList,
    _marker: PhantomData<&'a mut [f32]>,
}

impl<'a> SingleAudioBufferList<'a> {
    /// Builds a one-buffer interleaved f32 `AudioBufferList`.
    ///
    /// # Errors
    ///
    /// Returns a config error when the sample byte length exceeds Core Audio limits.
    pub fn interleaved_f32(
        channels: UInt32,
        samples: &'a mut [f32],
    ) -> Result<Self, AudioToolboxError> {
        Ok(Self {
            raw: AudioBufferList {
                number_buffers: 1,
                buffers: [AudioBuffer {
                    number_channels: channels,
                    data_byte_size: f32_byte_len(samples.len())?,
                    data: samples.as_mut_ptr().cast::<c_void>(),
                }],
            },
            _marker: PhantomData,
        })
    }
}

impl AudioBufferListTarget for SingleAudioBufferList<'_> {
    fn as_audio_buffer_list_mut_ptr(&mut self) -> *mut AudioBufferList {
        ptr::from_mut(&mut self.raw)
    }
}

pub struct AudioBufferListWriter<'a> {
    pub(super) ptr: NonNull<AudioBufferList>,
    pub(super) capacity: usize,
    pub(super) _marker: PhantomData<&'a mut AudioBufferList>,
}

impl AudioBufferListWriter<'_> {
    pub fn clear_buffer(&mut self, channel: usize) {
        self.write_buffer(
            channel,
            AudioBuffer {
                number_channels: 1,
                data_byte_size: 0,
                data: ptr::null_mut(),
            },
        );
    }

    /// Sets the visible buffer count for the list.
    ///
    /// # Errors
    ///
    /// Returns a config error when `count` exceeds the allocated capacity.
    pub fn set_number_buffers(&mut self, count: usize) -> Result<(), AudioToolboxError> {
        if count > self.capacity {
            return Err(AudioToolboxError::config(
                "AudioBufferList buffer count exceeds allocated capacity",
            ));
        }
        let count = UInt32::try_from(count).map_err(|_| {
            AudioToolboxError::config("AudioBufferList buffer count exceeds CoreAudio limit")
        })?;
        // SAFETY: self.ptr owns or borrows a live AudioBufferList header.
        unsafe {
            self.ptr.as_mut().number_buffers = count;
        }
        Ok(())
    }

    /// Writes a planar f32 input channel entry.
    ///
    /// # Errors
    ///
    /// Returns a config error when the sample byte length exceeds Core Audio limits.
    pub fn set_planar_f32_input(
        &mut self,
        channel: usize,
        samples: &[f32],
    ) -> Result<(), AudioToolboxError> {
        self.write_buffer(
            channel,
            AudioBuffer {
                number_channels: 1,
                data_byte_size: f32_byte_len(samples.len())?,
                data: samples.as_ptr().cast::<c_void>().cast_mut(),
            },
        );
        Ok(())
    }

    /// Writes a planar f32 output channel entry.
    ///
    /// # Errors
    ///
    /// Returns a config error when the sample byte length exceeds Core Audio limits.
    pub fn set_planar_f32_output(
        &mut self,
        channel: usize,
        samples: &mut [f32],
    ) -> Result<(), AudioToolboxError> {
        self.write_buffer(
            channel,
            AudioBuffer {
                number_channels: 1,
                data_byte_size: f32_byte_len(samples.len())?,
                data: samples.as_mut_ptr().cast::<c_void>(),
            },
        );
        Ok(())
    }

    pub fn set_raw_buffer(
        &mut self,
        index: usize,
        channels: UInt32,
        data_byte_size: UInt32,
        data: *mut c_void,
    ) {
        self.write_buffer(
            index,
            AudioBuffer {
                number_channels: channels,
                data_byte_size,
                data,
            },
        );
    }

    fn write_buffer(&mut self, channel: usize, buffer: AudioBuffer) {
        debug_assert!(channel < self.capacity);
        if channel >= self.capacity {
            return;
        }
        // SAFETY: channel is bounded by capacity.
        // SAFETY: the writer is created for owned storage or the converter callback.
        unsafe {
            buffer_ptr(self.ptr.as_ptr(), channel).write(buffer);
        }
    }
}

/// Converts an f32 sample count to Core Audio byte length.
///
/// # Errors
///
/// Returns a config error when the byte length overflows or exceeds Core Audio limits.
pub fn f32_byte_len(samples: usize) -> Result<UInt32, AudioToolboxError> {
    let bytes = samples
        .checked_mul(usize::try_from(BYTES_PER_F32_SAMPLE).unwrap_or(4))
        .ok_or_else(|| AudioToolboxError::config("f32 byte length overflow"))?;
    UInt32::try_from(bytes)
        .map_err(|_| AudioToolboxError::config("f32 byte length exceeds CoreAudio limit"))
}

unsafe fn buffer_ptr(list: *mut AudioBufferList, channel: usize) -> *mut AudioBuffer {
    // SAFETY: caller guarantees enough AudioBufferList tail storage for channel.
    unsafe {
        list.cast::<u8>()
            .add(AUDIO_BUFFER_LIST_HEADER_BYTES)
            .cast::<AudioBuffer>()
            .add(channel)
    }
}

fn buffer_list_layout(capacity: usize) -> Result<Layout, AudioToolboxError> {
    let buffer_bytes = size_of::<AudioBuffer>()
        .checked_mul(capacity)
        .ok_or_else(|| AudioToolboxError::config("audio buffer list size overflow"))?;
    let total_bytes = AUDIO_BUFFER_LIST_HEADER_BYTES
        .checked_add(buffer_bytes)
        .ok_or_else(|| AudioToolboxError::config("audio buffer list size overflow"))?;
    Layout::from_size_align(total_bytes, align_of::<AudioBufferList>())
        .map_err(|_| AudioToolboxError::config("invalid audio buffer list layout"))
}
