use std::ptr::NonNull;

use fdk_aac_sys as sys;

use crate::StreamInfo;

/// Failure to configure, feed, or execute the native decoder.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The native decoder could not allocate its instance.
    #[error("FDK AAC decoder allocation failed")]
    AllocationFailed,
    /// The decoder did not expose its stream information.
    #[error("FDK AAC stream information is missing")]
    MissingStreamInfo,
    /// An input length or output capacity exceeds the native API range.
    #[error("FDK AAC buffer length exceeds the native API range")]
    LengthOverflow,
    /// The decoder did not consume the complete input packet.
    #[error("FDK AAC decoder did not accept the complete packet")]
    IncompleteInput,
    /// The native decoder reported a failed operation.
    #[error("FDK AAC decoder operation failed with status {0:#x}")]
    NativeStatus(u32),
}

/// Exclusively owned FDK AAC decoder configured for raw MPEG-4 packets.
pub struct Decoder {
    handle: NonNull<sys::AAC_DECODER_INSTANCE>,
}

// SAFETY: this instance exclusively owns the handle; moving it transfers all decoder state.
unsafe impl Send for Decoder {}
// SAFETY: shared methods only read stream metadata. Every native mutation requires &mut self.
unsafe impl Sync for Decoder {}

impl Decoder {
    /// Creates a decoder from MPEG-4 `AudioSpecificConfig` bytes.
    ///
    /// # Errors
    /// Returns an error if allocation fails, the configuration is too large, or FDK rejects it.
    pub fn new(extra: &[u8]) -> Result<Self, Error> {
        // SAFETY: the raw transport has one layer and returns a fresh exclusively owned handle.
        let handle = unsafe { sys::aacDecoder_Open(sys::TRANSPORT_TYPE_TT_MP4_RAW, 1) };
        let decoder = Self {
            handle: NonNull::new(handle).ok_or(Error::AllocationFailed)?,
        };
        let mut bytes = extra.as_ptr().cast_mut();
        let length = u32::try_from(extra.len()).map_err(|_| Error::LengthOverflow)?;
        // SAFETY: the handle is live and ConfigRaw reads the supplied initialized configuration.
        check(unsafe { sys::aacDecoder_ConfigRaw(decoder.handle.as_ptr(), &mut bytes, &length) })?;
        Ok(decoder)
    }

    /// Borrows the native stream metadata until the next decoder mutation.
    ///
    /// # Errors
    /// Returns an error if the native decoder has no stream metadata.
    pub fn stream_info(&self) -> Result<&StreamInfo, Error> {
        // SAFETY: the handle is live; GetStreamInfo returns metadata owned by this decoder.
        let info = unsafe { sys::aacDecoder_GetStreamInfo(self.handle.as_ptr()).as_ref() };
        info.ok_or(Error::MissingStreamInfo)
    }

    /// Supplies one complete raw AAC packet to the native decoder.
    ///
    /// # Errors
    /// Returns an error if the packet is too large or FDK cannot consume it completely.
    pub fn fill(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut input = bytes.as_ptr().cast_mut();
        let length = u32::try_from(bytes.len()).map_err(|_| Error::LengthOverflow)?;
        let mut valid = length;
        // SAFETY: Fill reads one initialized packet, updates valid, and does not retain its pointer.
        check(unsafe {
            sys::aacDecoder_Fill(self.handle.as_ptr(), &mut input, &length, &mut valid)
        })?;
        if valid != 0 {
            return Err(Error::IncompleteInput);
        }
        Ok(())
    }

    /// Decodes one frame, or flushes delayed native PCM when `draining` is true.
    ///
    /// # Errors
    /// Returns an error if the output capacity is too large or FDK fails to decode a frame.
    pub fn decode_frame(&mut self, pcm: &mut [i16], draining: bool) -> Result<(), Error> {
        let length = i32::try_from(pcm.len()).map_err(|_| Error::LengthOverflow)?;
        let flags = if draining { sys::AACDEC_FLUSH } else { 0 };
        // SAFETY: pcm has length writable i16 elements; this handle is exclusively borrowed.
        let status = unsafe {
            sys::aacDecoder_DecodeFrame(self.handle.as_ptr(), pcm.as_mut_ptr(), length, flags)
        };
        check(status)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: this owner closes its fresh handle exactly once, after all borrows have ended.
        unsafe { sys::aacDecoder_Close(self.handle.as_ptr()) };
    }
}

fn check(status: sys::AAC_DECODER_ERROR) -> Result<(), Error> {
    if status == sys::AAC_DECODER_ERROR_AAC_DEC_OK {
        Ok(())
    } else {
        Err(Error::NativeStatus(status))
    }
}
