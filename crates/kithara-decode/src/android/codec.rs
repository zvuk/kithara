#![allow(unsafe_code)]

use std::{ffi::CString, ptr::NonNull};

use tracing::{debug, info};

use super::{
    error::AndroidBackendError,
    extractor::{OwnedExtractor, SelectedTrack},
    ffi::{
        self, KEY_CHANNEL_COUNT, KEY_PCM_ENCODING, KEY_SAMPLE_RATE, MEDIA_STATUS_OK,
        PCM_ENCODING_16BIT, PCM_ENCODING_FLOAT,
    },
    media_format::OwnedFormat,
};
use crate::types::PcmSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AndroidPcmEncoding {
    Pcm16,
    Float,
}

pub(crate) struct OutputFormat {
    pub(crate) spec: PcmSpec,
    pub(crate) pcm_encoding: AndroidPcmEncoding,
}

pub(crate) struct InputBuffer {
    pub(crate) index: usize,
    ptr: NonNull<u8>,
    len: usize,
}

impl InputBuffer {
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

pub(crate) struct OutputBuffer {
    pub(crate) index: usize,
    pub(crate) presentation_time_us: i64,
    pub(crate) flags: u32,
    ptr: NonNull<u8>,
    len: usize,
}

impl OutputBuffer {
    pub(crate) fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

pub(crate) enum DequeueOutput {
    TryAgainLater,
    OutputFormatChanged(OutputFormat),
    Output(OutputBuffer),
}

pub(crate) struct CodecBootstrap {
    pub(crate) codec: OwnedCodec,
    pub(crate) spec: PcmSpec,
    pub(crate) pcm_encoding: AndroidPcmEncoding,
}

pub(crate) struct OwnedCodec {
    raw: NonNull<ffi::AMediaCodec>,
    started: bool,
}

unsafe impl Send for OwnedCodec {}

impl OwnedCodec {
    pub(crate) fn raw(&self) -> *mut ffi::AMediaCodec {
        self.raw.as_ptr()
    }

    pub(crate) fn flush(&mut self) -> Result<(), AndroidBackendError> {
        let status = unsafe { ffi::AMediaCodec_flush(self.raw()) };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-flush",
                format!("status={status}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn dequeue_input_buffer(
        &mut self,
        timeout_us: i64,
    ) -> Result<Option<InputBuffer>, AndroidBackendError> {
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

    pub(crate) fn queue_input_buffer(
        &mut self,
        index: usize,
        size: usize,
        presentation_time_us: i64,
        flags: u32,
    ) -> Result<(), AndroidBackendError> {
        let presentation_time_us = u64::try_from(presentation_time_us).map_err(|_| {
            AndroidBackendError::operation(
                "codec-queue-input-buffer",
                format!("negative timestamp {presentation_time_us}"),
            )
        })?;
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

    pub(crate) fn queue_end_of_stream(&mut self, index: usize) -> Result<(), AndroidBackendError> {
        self.queue_input_buffer(index, 0, 0, ffi::MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM)
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
        let index =
            unsafe { ffi::AMediaCodec_dequeueOutputBuffer(self.raw(), &mut info, timeout_us) };
        match index as i32 {
            ffi::MEDIA_CODEC_INFO_TRY_AGAIN_LATER => Ok(DequeueOutput::TryAgainLater),
            ffi::MEDIA_CODEC_INFO_OUTPUT_FORMAT_CHANGED => {
                let format = load_output_format(self)?;
                Ok(DequeueOutput::OutputFormatChanged(format))
            }
            ffi::MEDIA_CODEC_INFO_OUTPUT_BUFFERS_CHANGED => Ok(DequeueOutput::TryAgainLater),
            negative if negative < 0 => Err(AndroidBackendError::operation(
                "codec-dequeue-output-buffer",
                format!("status={negative}"),
            )),
            _ => {
                let index = usize::try_from(index).map_err(|_| {
                    AndroidBackendError::operation(
                        "codec-dequeue-output-buffer",
                        format!("buffer index out of range: {index}"),
                    )
                })?;
                let mut size = 0usize;
                let data = NonNull::new(unsafe {
                    ffi::AMediaCodec_getOutputBuffer(self.raw(), index, &mut size)
                })
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
                    flags: info.flags,
                    ptr: NonNull::new(unsafe { data.as_ptr().add(offset) }).ok_or_else(|| {
                        AndroidBackendError::operation(
                            "codec-get-output-buffer",
                            format!("buffer={index} offset {offset} produced null pointer"),
                        )
                    })?,
                    len: payload_size,
                }))
            }
        }
    }

    pub(crate) fn release_output_buffer(
        &mut self,
        index: usize,
    ) -> Result<(), AndroidBackendError> {
        let status = unsafe { ffi::AMediaCodec_releaseOutputBuffer(self.raw(), index, false) };
        if status != MEDIA_STATUS_OK {
            return Err(AndroidBackendError::operation(
                "codec-release-output-buffer",
                format!("buffer={index} status={status}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn output_format(&self) -> Result<OwnedFormat, AndroidBackendError> {
        let raw = NonNull::new(unsafe { ffi::AMediaCodec_getOutputFormat(self.raw()) })
            .ok_or_else(|| {
                AndroidBackendError::operation("codec-output-format", "output format was null")
            })?;
        Ok(OwnedFormat::from_raw(raw))
    }
}

impl Drop for OwnedCodec {
    fn drop(&mut self) {
        if self.started {
            let _ = unsafe { ffi::AMediaCodec_stop(self.raw()) };
        }
        let _ = unsafe { ffi::AMediaCodec_delete(self.raw()) };
    }
}

pub(crate) fn bootstrap_codec(
    extractor: &OwnedExtractor,
    track: &SelectedTrack,
) -> Result<CodecBootstrap, AndroidBackendError> {
    debug!(
        track_index = track.index,
        mime = %track.mime,
        "Configuring Android MediaCodec decoder"
    );
    let mime = CString::new(track.mime.as_str())
        .map_err(|error| AndroidBackendError::operation("codec-mime-cstring", error.to_string()))?;
    let codec_raw = NonNull::new(unsafe { ffi::AMediaCodec_createDecoderByType(mime.as_ptr()) })
        .ok_or_else(|| {
            AndroidBackendError::operation(
                "codec-create-decoder",
                format!("mime={} returned null", track.mime),
            )
        })?;
    let mut codec = OwnedCodec {
        raw: codec_raw,
        started: false,
    };

    let mut format = extractor.track_format(track.index)?;
    if !format.set_i32(KEY_PCM_ENCODING, PCM_ENCODING_16BIT) {
        debug!(
            mime = %track.mime,
            "Android MediaCodec ignored requested PCM 16-bit output; continuing with negotiated output format"
        );
    }

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
            format!("mime={} status={status}", track.mime),
        ));
    }

    let status = unsafe { ffi::AMediaCodec_start(codec.raw()) };
    if status != MEDIA_STATUS_OK {
        return Err(AndroidBackendError::operation(
            "codec-start",
            format!("mime={} status={status}", track.mime),
        ));
    }
    codec.started = true;

    let output = load_output_format(&codec)?;
    info!(
        mime = %track.mime,
        sample_rate = output.spec.sample_rate,
        channels = output.spec.channels,
        pcm_encoding = ?output.pcm_encoding,
        "Android MediaCodec decoder started"
    );

    Ok(CodecBootstrap {
        codec,
        spec: output.spec,
        pcm_encoding: output.pcm_encoding,
    })
}

fn load_output_format(codec: &OwnedCodec) -> Result<OutputFormat, AndroidBackendError> {
    let output = codec.output_format()?;
    let sample_rate = output.get_u32(KEY_SAMPLE_RATE)?.ok_or_else(|| {
        AndroidBackendError::operation("codec-output-format", "missing sample-rate")
    })?;
    let channels = output.get_u16(KEY_CHANNEL_COUNT)?.ok_or_else(|| {
        AndroidBackendError::operation("codec-output-format", "missing channel-count")
    })?;
    let pcm_encoding = match output.get_i32(KEY_PCM_ENCODING) {
        None | Some(PCM_ENCODING_16BIT) => AndroidPcmEncoding::Pcm16,
        Some(PCM_ENCODING_FLOAT) => AndroidPcmEncoding::Float,
        Some(other) => return Err(AndroidBackendError::UnsupportedPcmEncoding { encoding: other }),
    };

    Ok(OutputFormat {
        spec: PcmSpec {
            sample_rate,
            channels,
        },
        pcm_encoding,
    })
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
