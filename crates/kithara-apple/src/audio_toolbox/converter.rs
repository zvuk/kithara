use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
};

use super::{
    buffer::{AudioBufferListTarget, AudioBufferListWriter},
    error::AudioToolboxError,
    pod::ApplePod,
    sys::{
        AUDIO_CONVERTER_ERR_NO_DATA_NOW, AudioBufferList, AudioConverterRef,
        AudioStreamBasicDescription, AudioStreamPacketDescription, NO_ERR, OSStatus, PARAM_ERR,
        UInt32, audio_converter_dispose, audio_converter_fill_complex_buffer,
        audio_converter_get_property, audio_converter_new, audio_converter_reset,
        audio_converter_set_property,
    },
};

pub trait AudioConverterInput {
    fn fill(&mut self, request: AudioConverterInputRequest<'_>) -> OSStatus;
}

pub struct AudioConverterInputRequest<'a> {
    pub packets: &'a mut UInt32,
    pub buffers: AudioBufferListWriter<'a>,
    pub packet_descriptions: PacketDescriptionsOut<'a>,
}

pub struct PacketDescriptionsOut<'a> {
    ptr: Option<&'a mut *mut AudioStreamPacketDescription>,
}

impl PacketDescriptionsOut<'_> {
    pub fn clear(&mut self) {
        if let Some(ptr) = self.ptr.as_deref_mut() {
            *ptr = ptr::null_mut();
        }
    }

    pub fn set_one(&mut self, description: &mut AudioStreamPacketDescription) {
        if let Some(ptr) = self.ptr.as_deref_mut() {
            *ptr = ptr::from_mut(description);
        }
    }
}

#[derive(Default)]
pub struct AudioConverterPacketInput {
    packet_ptr: *const u8,
    packet_desc: AudioStreamPacketDescription,
    packet_len: UInt32,
    has_packet: bool,
    reached_eof: bool,
}

// SAFETY: the raw pointer is a borrowed packet slice consumed synchronously.
// SAFETY: the wrapper is not Sync, and callers clear it before moving work.
unsafe impl Send for AudioConverterPacketInput {}

impl AudioConverterPacketInput {
    pub fn clear(&mut self) {
        self.packet_ptr = ptr::null();
        self.packet_len = 0;
        self.has_packet = false;
        self.reached_eof = false;
    }

    pub fn finish(&mut self) {
        self.packet_ptr = ptr::null();
        self.packet_len = 0;
        self.has_packet = false;
        self.reached_eof = true;
    }

    pub fn set(&mut self, data: &[u8], description: AudioStreamPacketDescription) -> OSStatus {
        let Ok(len) = UInt32::try_from(data.len()) else {
            return PARAM_ERR;
        };
        self.packet_ptr = data.as_ptr();
        self.packet_len = len;
        self.packet_desc = AudioStreamPacketDescription {
            start_offset: 0,
            variable_frames_in_packet: description.variable_frames_in_packet,
            data_byte_size: len,
        };
        self.has_packet = true;
        self.reached_eof = false;
        NO_ERR
    }
}

impl AudioConverterInput for AudioConverterPacketInput {
    fn fill(&mut self, mut request: AudioConverterInputRequest<'_>) -> OSStatus {
        if !self.has_packet {
            *request.packets = 0;
            return if self.reached_eof {
                NO_ERR
            } else {
                AUDIO_CONVERTER_ERR_NO_DATA_NOW
            };
        }

        let status =
            set_compressed_packet_buffer(&mut request.buffers, self.packet_len, self.packet_ptr);
        if status != NO_ERR {
            return status;
        }
        *request.packets = 1;
        request.packet_descriptions.set_one(&mut self.packet_desc);
        self.has_packet = false;
        NO_ERR
    }
}

pub struct AudioConverter {
    raw: NonNull<c_void>,
}

// SAFETY: AudioConverterRef is an opaque Core Audio handle.
// SAFETY: the safe wrapper exposes mutable methods only, and it is not Sync.
unsafe impl Send for AudioConverter {}

impl AudioConverter {
    /// Creates an `AudioToolbox` converter for the two stream formats.
    ///
    /// # Errors
    ///
    /// Returns an `AudioToolbox` error when the converter cannot be created.
    pub fn new(
        source_format: &AudioStreamBasicDescription,
        destination_format: &AudioStreamBasicDescription,
    ) -> Result<Self, AudioToolboxError> {
        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: source/destination point at valid ASBD values.
        // SAFETY: converter is a writable out-param.
        let status =
            unsafe { audio_converter_new(source_format, destination_format, &mut converter) };
        if status != NO_ERR {
            return Err(AudioToolboxError::status("AudioConverterNew", status));
        }
        let raw = NonNull::new(converter)
            .ok_or_else(|| AudioToolboxError::config("AudioConverterNew returned null"))?;
        Ok(Self { raw })
    }

    #[must_use]
    pub fn as_raw(&self) -> AudioConverterRef {
        self.raw.as_ptr()
    }

    pub fn fill_complex_buffer<I, O>(
        &mut self,
        input: &mut I,
        input_buffers: usize,
        output_packets: &mut UInt32,
        output: &mut O,
    ) -> OSStatus
    where
        I: AudioConverterInput,
        O: AudioBufferListTarget,
    {
        let mut bridge = InputBridge {
            input,
            input_buffers,
        };
        // SAFETY: self.raw is live and bridge is stack-pinned for the call.
        // SAFETY: output points at a live AudioBufferList target.
        unsafe {
            audio_converter_fill_complex_buffer(
                self.as_raw(),
                input_trampoline::<I>,
                ptr::from_mut(&mut bridge).cast::<c_void>(),
                output_packets,
                output.as_audio_buffer_list_mut_ptr(),
                ptr::null_mut(),
            )
        }
    }

    #[must_use]
    pub fn get_property<T>(&self, property: UInt32) -> Option<T>
    where
        T: ApplePod,
    {
        let mut value = T::default();
        let Ok(mut size) = UInt32::try_from(size_of::<T>()) else {
            return None;
        };
        // SAFETY: value is writable storage for exactly size bytes.
        let status = unsafe {
            audio_converter_get_property(
                self.as_raw(),
                property,
                &mut size,
                ptr::from_mut(&mut value).cast::<c_void>(),
            )
        };
        (status == NO_ERR).then_some(value)
    }

    pub fn reset(&mut self) -> OSStatus {
        // SAFETY: self.raw is a live converter handle.
        unsafe { audio_converter_reset(self.as_raw()) }
    }

    pub fn set_property_bytes(&mut self, property: UInt32, data: &[u8]) -> OSStatus {
        let Ok(size) = UInt32::try_from(data.len()) else {
            return PARAM_ERR;
        };
        // SAFETY: self.raw is a live converter handle; data points at size bytes.
        unsafe { audio_converter_set_property(self.as_raw(), property, size, data.as_ptr().cast()) }
    }
}

impl Drop for AudioConverter {
    fn drop(&mut self) {
        // SAFETY: raw was returned by audio_converter_new and is disposed once.
        let _ = unsafe { audio_converter_dispose(self.as_raw()) };
    }
}

struct InputBridge<'a, I> {
    input: &'a mut I,
    input_buffers: usize,
}

extern "C" fn input_trampoline<I>(
    _converter: AudioConverterRef,
    io_num_packets: *mut UInt32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus
where
    I: AudioConverterInput,
{
    if io_num_packets.is_null() || io_data.is_null() || user_data.is_null() {
        return PARAM_ERR;
    }

    // SAFETY: user_data is the InputBridge pointer supplied by fill_complex_buffer.
    let bridge = unsafe { &mut *(user_data as *mut InputBridge<'_, I>) };
    // SAFETY: pointer was checked non-null and remains valid for this callback.
    let packets = unsafe { &mut *io_num_packets };
    // SAFETY: io_data was checked non-null above.
    let ptr = unsafe { NonNull::new_unchecked(io_data) };
    let buffers = AudioBufferListWriter {
        ptr,
        capacity: bridge.input_buffers,
        _marker: std::marker::PhantomData,
    };
    let packet_descriptions = if out_packet_desc.is_null() {
        PacketDescriptionsOut { ptr: None }
    } else {
        // SAFETY: non-null out-param from AudioConverter.
        let out_packet_desc = unsafe { &mut *out_packet_desc };
        PacketDescriptionsOut {
            ptr: Some(out_packet_desc),
        }
    };

    bridge.input.fill(AudioConverterInputRequest {
        packets,
        buffers,
        packet_descriptions,
    })
}

fn set_compressed_packet_buffer(
    buffers: &mut AudioBufferListWriter<'_>,
    packet_len: UInt32,
    packet_ptr: *const u8,
) -> OSStatus {
    if buffers.set_number_buffers(1).is_err() {
        return PARAM_ERR;
    }
    buffers.set_raw_buffer(0, 1, packet_len, packet_ptr.cast_mut().cast());
    NO_ERR
}
