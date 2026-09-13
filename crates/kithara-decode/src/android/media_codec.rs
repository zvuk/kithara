#![allow(unsafe_code)]

use std::ptr::NonNull;

use kithara_signal::AudioSpec;

use super::{
    aformat::OwnedFormat,
    error::AndroidBackendError,
    ffi::{
        self, KEY_CHANNEL_COUNT, KEY_PCM_ENCODING, KEY_SAMPLE_RATE, MEDIA_STATUS_OK,
        PCM_ENCODING_16BIT, PCM_ENCODING_FLOAT,
    },
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AndroidPcmEncoding {
    Pcm16,
    Float,
}

pub(crate) struct OutputFormat {
    pub(crate) pcm_encoding: AndroidPcmEncoding,
    pub(crate) spec: AudioSpec,
}

pub(crate) struct InputBuffer {
    pub(crate) index: usize,
    ptr: NonNull<u8>,
    len: usize,
}

impl InputBuffer {
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` covers `len` bytes of codec-owned storage, exclusively
        // borrowed for as long as the returned slice lives.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

pub(crate) struct OutputBuffer {
    pub(crate) index: usize,
    pub(crate) presentation_time_us: i64,
    pub(crate) end_of_stream: bool,
    ptr: NonNull<u8>,
    len: usize,
}

/// Parameters for `OwnedCodec::queue_input_buffer`. Bundles the related
/// FFI primitives so callers can't accidentally swap argument positions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct QueueInput {
    pub(crate) presentation_time_us: i64,
    pub(crate) flags: u32,
    pub(crate) index: usize,
    pub(crate) size: usize,
}

impl OutputBuffer {
    pub(crate) fn data(&self) -> &[u8] {
        // SAFETY: `ptr` covers `len` bytes of codec-owned storage, borrowed for
        // as long as the returned slice lives.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl AsRef<[u8]> for OutputBuffer {
    fn as_ref(&self) -> &[u8] {
        self.data()
    }
}

pub(crate) enum DequeueOutput {
    TryAgainLater,
    OutputFormatChanged(OutputFormat),
    Output(OutputBuffer),
}

pub(crate) struct OwnedCodec {
    raw: NonNull<ffi::AMediaCodec>,
    started: bool,
}

// SAFETY: `AMediaCodec` is owned exclusively by this handle, and every call
// goes through `&mut self`, so it is only ever used from one thread at a time.
unsafe impl Send for OwnedCodec {}

impl OwnedCodec {
    /// Select and start a decoder using the input MIME and parameters in
    /// the track format, whether extracted by Android or built by the demuxer.
    pub(crate) fn create_with_format(format: &OwnedFormat) -> Result<Self, AndroidBackendError> {
        let mime = format.get_str(ffi::KEY_MIME).ok_or_else(|| {
            AndroidBackendError::operation("codec-create-decoder", "track format has no MIME")
        })?;
        // SAFETY: `mime` is a NUL-terminated C string.
        let codec_raw =
            NonNull::new(unsafe { ffi::AMediaCodec_createDecoderByType(mime.as_ptr()) })
                .ok_or_else(|| {
                    AndroidBackendError::operation(
                        "codec-create-decoder",
                        format!("mime={} returned null", mime.to_string_lossy()),
                    )
                })?;
        let mut codec = Self {
            raw: codec_raw,
            started: false,
        };
        // SAFETY: both handles are live; a null surface and crypto configure a
        // decoder that writes into codec-owned buffers.
        let status = unsafe {
            ffi::AMediaCodec_configure(
                codec.raw(),
                format.raw(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-configure",
                format!("mime={} status={status}", mime.to_string_lossy()),
            ));
        }
        // SAFETY: `codec` is live and configured.
        let status = unsafe { ffi::AMediaCodec_start(codec.raw()) };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-start",
                format!("mime={} status={status}", mime.to_string_lossy()),
            ));
        }
        codec.started = true;
        Ok(codec)
    }

    pub(crate) fn dequeue_input_buffer(
        &mut self,
        timeout_us: i64,
    ) -> Result<Option<InputBuffer>, AndroidBackendError> {
        // SAFETY: `raw` is live and exclusively borrowed.
        let index = unsafe { ffi::AMediaCodec_dequeueInputBuffer(self.raw(), timeout_us) };
        if index == ffi::MEDIA_CODEC_INFO_TRY_AGAIN_LATER as isize {
            return Ok(None);
        }
        if index < 0 {
            return Err(AndroidBackendError::operation(
                "codec-dequeue-input-buffer",
                format!("status={index}"),
            ));
        }

        let index = usize::try_from(index).map_err(|_| {
            AndroidBackendError::operation(
                "codec-dequeue-input-buffer",
                format!("buffer index out of range: {index}"),
            )
        })?;

        let mut size = 0usize;
        // SAFETY: `raw` is live; the NDK bounds-checks `index` and writes the
        // buffer length into `size`.
        let data =
            NonNull::new(unsafe { ffi::AMediaCodec_getInputBuffer(self.raw(), index, &mut size) })
                .ok_or_else(|| {
                    AndroidBackendError::operation(
                        "codec-get-input-buffer",
                        format!("buffer={index} pointer was null"),
                    )
                })?;
        Ok(Some(InputBuffer {
            index,
            ptr: data,
            len: size,
        }))
    }

    pub(crate) fn dequeue_output_buffer(
        &mut self,
        timeout_us: i64,
    ) -> Result<DequeueOutput, AndroidBackendError> {
        let mut info = ffi::AMediaCodecBufferInfo {
            offset: 0,
            size: 0,
            presentation_time_us: 0,
            flags: 0,
        };
        // SAFETY: `raw` is live and exclusively borrowed; `info` is an out-param.
        let index =
            unsafe { ffi::AMediaCodec_dequeueOutputBuffer(self.raw(), &mut info, timeout_us) };
        if index == ffi::MEDIA_CODEC_INFO_TRY_AGAIN_LATER as isize
            || index == ffi::MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED as isize
        {
            return Ok(DequeueOutput::TryAgainLater);
        }
        if index == ffi::MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED as isize {
            let format = OutputFormat::read(&self.output_format()?)?;
            return Ok(DequeueOutput::OutputFormatChanged(format));
        }
        let index = usize::try_from(index).map_err(|_| {
            AndroidBackendError::operation("codec-dequeue-output-buffer", format!("status={index}"))
        })?;

        let mut size = 0usize;
        // SAFETY: `raw` is live; the NDK bounds-checks `index` and writes the
        // buffer length into `size`.
        let data =
            NonNull::new(unsafe { ffi::AMediaCodec_getOutputBuffer(self.raw(), index, &mut size) })
                .ok_or_else(|| {
                    AndroidBackendError::operation(
                        "codec-get-output-buffer",
                        format!("buffer={index} pointer was null"),
                    )
                })?;

        let offset = usize::try_from(info.offset).map_err(|_| {
            AndroidBackendError::operation(
                "codec-output-buffer-offset",
                format!("buffer={index} offset={} is negative", info.offset),
            )
        })?;
        let payload_size = usize::try_from(info.size).map_err(|_| {
            AndroidBackendError::operation(
                "codec-output-buffer-size",
                format!("buffer={index} size={} is negative", info.size),
            )
        })?;
        let end = offset.checked_add(payload_size).ok_or_else(|| {
            AndroidBackendError::operation(
                "codec-output-buffer-range",
                format!("buffer={index} offset={offset} size={payload_size} overflowed"),
            )
        })?;
        if end > size {
            return Err(AndroidBackendError::operation(
                "codec-output-buffer-range",
                format!("buffer={index} range {offset}..{end} exceeds capacity {size}"),
            ));
        }

        Ok(DequeueOutput::Output(OutputBuffer {
            index,
            presentation_time_us: info.presentation_time_us,
            end_of_stream: info.flags & ffi::MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM != 0,
            // SAFETY: `offset` is within the `size` bytes `data` points at.
            ptr: NonNull::new(unsafe { data.as_ptr().add(offset) }).ok_or_else(|| {
                AndroidBackendError::operation(
                    "codec-get-output-buffer",
                    format!("buffer={index} offset {offset} produced null pointer"),
                )
            })?,
            len: payload_size,
        }))
    }

    pub(crate) fn flush(&mut self) -> Result<(), AndroidBackendError> {
        // SAFETY: `raw` is live and exclusively borrowed.
        let status = unsafe { ffi::AMediaCodec_flush(self.raw()) };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-flush",
                format!("status={status}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn output_format(&self) -> Result<OwnedFormat, AndroidBackendError> {
        // SAFETY: `raw` is live; the caller owns the returned format.
        let raw = NonNull::new(unsafe { ffi::AMediaCodec_getOutputFormat(self.raw()) })
            .ok_or_else(|| {
                AndroidBackendError::operation("codec-output-format", "output format was null")
            })?;
        Ok(OwnedFormat::from(raw))
    }

    pub(crate) fn queue_input_buffer(
        &mut self,
        request: QueueInput,
    ) -> Result<(), AndroidBackendError> {
        let QueueInput {
            index,
            size,
            presentation_time_us,
            flags,
        } = request;
        let presentation_time_us = u64::try_from(presentation_time_us).map_err(|_| {
            AndroidBackendError::operation(
                "codec-queue-input-buffer",
                format!("negative timestamp {presentation_time_us}"),
            )
        })?;
        // SAFETY: `raw` is live; `index` names a buffer dequeued from it and
        // `size` is within that buffer.
        let status = unsafe {
            ffi::AMediaCodec_queueInputBuffer(
                self.raw(),
                index,
                0,
                size,
                presentation_time_us,
                flags,
            )
        };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-queue-input-buffer",
                format!("buffer={index} size={size} status={status}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn raw(&self) -> *mut ffi::AMediaCodec {
        self.raw.as_ptr()
    }

    pub(crate) fn release_output_buffer(
        &mut self,
        index: usize,
    ) -> Result<(), AndroidBackendError> {
        // SAFETY: `raw` is live; `index` names a buffer dequeued from it.
        let status = unsafe { ffi::AMediaCodec_releaseOutputBuffer(self.raw(), index, false) };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-release-output-buffer",
                format!("buffer={index} status={status}"),
            ));
        }
        Ok(())
    }
}

impl Drop for OwnedCodec {
    fn drop(&mut self) {
        if self.started {
            // SAFETY: `raw` is live and was started.
            let _ = unsafe { ffi::AMediaCodec_stop(self.raw()) };
        }
        // SAFETY: `raw` is live and freed exactly once, here.
        let _ = unsafe { ffi::AMediaCodec_delete(self.raw()) };
    }
}

impl OutputFormat {
    pub(crate) fn read(output: &OwnedFormat) -> Result<Self, AndroidBackendError> {
        let sample_rate = output.get_u32(KEY_SAMPLE_RATE)?.ok_or_else(|| {
            AndroidBackendError::operation("codec-output-format", "missing sample-rate")
        })?;
        let channels = output.get_u16(KEY_CHANNEL_COUNT)?.ok_or_else(|| {
            AndroidBackendError::operation("codec-output-format", "missing channel-count")
        })?;
        let pcm_encoding = match output.get_i32(KEY_PCM_ENCODING) {
            None | Some(PCM_ENCODING_16BIT) => AndroidPcmEncoding::Pcm16,
            Some(PCM_ENCODING_FLOAT) => AndroidPcmEncoding::Float,
            Some(other) => {
                return Err(AndroidBackendError::UnsupportedPcmEncoding { encoding: other });
            }
        };

        let spec = crate::types::checked_audio_spec(channels, sample_rate, "android.codec.output")
            .map_err(|error| {
                AndroidBackendError::operation("codec-output-format", error.to_string())
            })?;
        Ok(Self { pcm_encoding, spec })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn pcm_encoding_variants_are_distinct() {
        assert!(matches!(
            AndroidPcmEncoding::Pcm16,
            AndroidPcmEncoding::Pcm16
        ));
        assert!(matches!(
            AndroidPcmEncoding::Float,
            AndroidPcmEncoding::Float
        ));
    }
}
