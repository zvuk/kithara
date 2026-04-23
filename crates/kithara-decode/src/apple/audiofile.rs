//! `AudioFile` wrapper: reads packets via `AudioFileOpenWithCallbacks`.
//!
//! Supports every atom-aware container that has real `stsz`/`stco`
//! tables (MP3, FLAC, ADTS, CAF, WAV, non-fragmented MP4). Fragmented
//! MP4 is handled by [`super::fmp4`] instead.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    ptr, slice,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tracing::{debug, warn};

use super::{
    consts::{Consts, os_status_to_string},
    ffi::{
        AudioFileClose, AudioFileGetProperty, AudioFileGetPropertyInfo, AudioFileID,
        AudioFileOpenWithCallbacks, AudioFilePropertyID, AudioFileReadPacketData, AudioFileTypeID,
        AudioFramePacketTranslation, AudioStreamBasicDescription, AudioStreamPacketDescription,
        Float64, OSStatus, SInt64, UInt32,
    },
    reader::{PacketReader, PacketRef},
};
use crate::{
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
};

/// Callback context: owns the `BoxedSource` and keeps it alive for the
/// lifetime of the `AudioFileID`.
struct SourceCtx {
    source: BoxedSource,
    byte_len: Arc<AtomicU64>,
}

impl SourceCtx {
    fn size(&self) -> i64 {
        let len = self.byte_len.load(Ordering::Acquire);
        if len == 0 {
            // Unknown length → sentinel so AudioFile still reads linearly
            // for byte-oriented containers.
            i64::MAX
        } else {
            i64::try_from(len).unwrap_or(i64::MAX)
        }
    }
}

extern "C" fn read_callback(
    client_data: *mut c_void,
    position: SInt64,
    request_count: UInt32,
    buffer: *mut c_void,
    actual_count: *mut UInt32,
) -> OSStatus {
    // SAFETY: `client_data` is the pointer passed at open-time;
    // the owning `Box<SourceCtx>` outlives every callback invocation.
    let ctx = unsafe { &mut *(client_data as *mut SourceCtx) };
    // SAFETY: AudioFile guarantees `buffer` has `request_count` writable bytes.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, request_count as usize) };
    if position < 0 {
        // SAFETY: out-param provided by AudioFile.
        unsafe { *actual_count = 0 };
        return Consts::kAudioFilePositionError;
    }
    let mut filled = 0usize;
    while filled < buf.len() {
        if ctx
            .source
            .seek(SeekFrom::Start(position.cast_unsigned() + filled as u64))
            .is_err()
        {
            // SAFETY: out-param.
            unsafe { *actual_count = UInt32::try_from(filled).unwrap_or(UInt32::MAX) };
            return if filled == 0 {
                Consts::kAudioFilePositionError
            } else {
                Consts::noErr
            };
        }
        match ctx.source.read(&mut buf[filled..]) {
            Ok(n) if n > 0 => filled += n,
            _ => break,
        }
    }
    // SAFETY: out-param.
    unsafe { *actual_count = UInt32::try_from(filled).unwrap_or(UInt32::MAX) };
    if filled == 0 {
        Consts::kAudioFileEndOfFileError
    } else {
        Consts::noErr
    }
}

extern "C" fn get_size_callback(client_data: *mut c_void) -> SInt64 {
    // SAFETY: same aliasing guarantee as `read_callback`.
    let ctx = unsafe { &*(client_data as *const SourceCtx) };
    ctx.size()
}

pub(super) struct AudioFileReader {
    file: AudioFileID,
    /// Boxed so the pointer fed into `AudioFileOpenWithCallbacks` stays
    /// stable for the lifetime of the `AudioFileID`.
    _ctx: Box<SourceCtx>,
    format: AudioStreamBasicDescription,
    magic_cookie: Option<Vec<u8>>,
    packet_upper_bound: u32,
    duration: Option<Duration>,
    packet_buf: Vec<u8>,
    last_num_bytes: u32,
    last_desc: AudioStreamPacketDescription,
    next_packet: u64,
}

impl AudioFileReader {
    pub(super) fn open(
        source: BoxedSource,
        byte_len: Arc<AtomicU64>,
        file_type: AudioFileTypeID,
    ) -> DecodeResult<Self> {
        let mut ctx = Box::new(SourceCtx { source, byte_len });
        let ctx_ptr = ctx.as_mut() as *mut SourceCtx as *mut c_void;

        let mut file: AudioFileID = ptr::null_mut();
        // SAFETY: `ctx_ptr` outlives the AudioFileID (owned by `ctx` kept
        // inside `Self`). Callbacks cast it back to `&mut SourceCtx`.
        let status = unsafe {
            AudioFileOpenWithCallbacks(
                ctx_ptr,
                read_callback,
                ptr::null(),
                get_size_callback,
                ptr::null(),
                file_type,
                &mut file,
            )
        };
        if status != Consts::noErr {
            let err = os_status_to_string(status);
            warn!(status, err = %err, "AudioFileOpenWithCallbacks failed");
            return Err(DecodeError::InvalidData(format!(
                "AudioFileOpenWithCallbacks failed: {err}"
            )));
        }

        let format = Self::get_format(file)?;
        let magic_cookie = Self::get_magic_cookie(file);
        let packet_upper_bound =
            Self::get_property_u32(file, Consts::kAudioFilePropertyPacketSizeUpperBound)
                .or_else(|_| {
                    Self::get_property_u32(file, Consts::kAudioFilePropertyMaximumPacketSize)
                })
                .unwrap_or(64 * 1024);
        let duration = Self::get_property_f64(file, Consts::kAudioFilePropertyEstimatedDuration)
            .ok()
            .filter(|d| *d > 0.0)
            .map(Duration::from_secs_f64);

        debug!(
            file_type = format!("{:#x}", file_type),
            sample_rate = format.mSampleRate,
            channels = format.mChannelsPerFrame,
            frames_per_packet = format.mFramesPerPacket,
            packet_upper_bound,
            ?duration,
            cookie_size = magic_cookie.as_ref().map_or(0, Vec::len),
            "AudioFileReader opened"
        );

        Ok(Self {
            file,
            _ctx: ctx,
            format,
            magic_cookie,
            packet_upper_bound,
            duration,
            packet_buf: Vec::new(),
            last_num_bytes: 0,
            last_desc: AudioStreamPacketDescription::default(),
            next_packet: 0,
        })
    }

    fn get_format(file: AudioFileID) -> DecodeResult<AudioStreamBasicDescription> {
        let mut format = AudioStreamBasicDescription::default();
        let mut size =
            UInt32::try_from(size_of::<AudioStreamBasicDescription>()).map_err(|_| {
                DecodeError::InvalidData("AudioStreamBasicDescription size exceeds UInt32".into())
            })?;
        // SAFETY: live AudioFileID + stack-allocated struct.
        let status = unsafe {
            AudioFileGetProperty(
                file,
                Consts::kAudioFilePropertyDataFormat,
                &mut size,
                &mut format as *mut _ as *mut c_void,
            )
        };
        if status != Consts::noErr {
            return Err(DecodeError::InvalidData(format!(
                "AudioFile data-format query failed: {}",
                os_status_to_string(status)
            )));
        }
        if format.mSampleRate <= 0.0 || format.mChannelsPerFrame == 0 {
            return Err(DecodeError::InvalidData(format!(
                "AudioFile reported empty format (sr={} ch={})",
                format.mSampleRate, format.mChannelsPerFrame
            )));
        }
        Ok(format)
    }

    fn get_magic_cookie(file: AudioFileID) -> Option<Vec<u8>> {
        let mut size: UInt32 = 0;
        let mut writable: UInt32 = 0;
        // SAFETY: live AudioFileID + writable u32 out-params.
        let status = unsafe {
            AudioFileGetPropertyInfo(
                file,
                Consts::kAudioFilePropertyMagicCookieData,
                &mut size,
                &mut writable,
            )
        };
        if status != Consts::noErr || size == 0 {
            return None;
        }
        let mut cookie = vec![0u8; size as usize];
        // SAFETY: `cookie` has `size` writable bytes.
        let status = unsafe {
            AudioFileGetProperty(
                file,
                Consts::kAudioFilePropertyMagicCookieData,
                &mut size,
                cookie.as_mut_ptr() as *mut c_void,
            )
        };
        if status != Consts::noErr {
            return None;
        }
        Some(cookie)
    }

    /// Fetch a scalar `AudioFile` property of type `T` into a stack-allocated slot.
    fn get_property_scalar<T: Default>(
        file: AudioFileID,
        prop: AudioFilePropertyID,
    ) -> DecodeResult<T> {
        let mut value = T::default();
        let mut size = UInt32::try_from(size_of::<T>()).map_err(|_| {
            DecodeError::InvalidData(format!("property 0x{prop:08x} value size exceeds UInt32"))
        })?;
        // SAFETY: `file` is a live AudioFileID; `value` is a writable slot
        // of exactly `size` bytes with a matching layout.
        let status = unsafe {
            AudioFileGetProperty(file, prop, &mut size, &mut value as *mut _ as *mut c_void)
        };
        if status != Consts::noErr {
            return Err(DecodeError::InvalidData(format!(
                "property 0x{prop:08x} failed: {}",
                os_status_to_string(status)
            )));
        }
        Ok(value)
    }

    fn get_property_u32(file: AudioFileID, prop: AudioFilePropertyID) -> DecodeResult<u32> {
        Self::get_property_scalar::<UInt32>(file, prop)
    }

    fn get_property_f64(file: AudioFileID, prop: AudioFilePropertyID) -> DecodeResult<f64> {
        Self::get_property_scalar::<Float64>(file, prop)
    }
}

impl PacketReader for AudioFileReader {
    fn format(&self) -> AudioStreamBasicDescription {
        self.format
    }

    fn magic_cookie(&self) -> Option<&[u8]> {
        self.magic_cookie.as_deref()
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn read_next_packet(&mut self) -> DecodeResult<Option<PacketRef<'_>>> {
        let capacity = self.packet_upper_bound as usize;
        if self.packet_buf.len() < capacity {
            self.packet_buf.resize(capacity, 0);
        }
        let mut num_bytes: UInt32 = self.packet_upper_bound;
        let mut num_packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();
        let packet_index = self.next_packet as SInt64;

        // SAFETY: `self.file` is live; buffer has `capacity` writable bytes;
        // out-params are writable stack values.
        let status = unsafe {
            AudioFileReadPacketData(
                self.file,
                0,
                &mut num_bytes,
                &mut desc,
                packet_index,
                &mut num_packets,
                self.packet_buf.as_mut_ptr() as *mut c_void,
            )
        };

        if status == Consts::kAudioFileEndOfFileError || num_packets == 0 {
            return Ok(None);
        }
        if status != Consts::noErr {
            let err = os_status_to_string(status);
            warn!(
                status,
                err = %err,
                packet_index,
                "AudioFileReadPacketData failed"
            );
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!("AudioFileReadPacketData failed: {err}"),
            ))));
        }

        self.next_packet += u64::from(num_packets);
        self.last_num_bytes = num_bytes;
        self.last_desc = desc;
        Ok(Some(PacketRef {
            data: &self.packet_buf[..num_bytes as usize],
            description: desc,
        }))
    }

    fn seek_to_frame(&mut self, target_frame: u64) -> DecodeResult<u64> {
        let frames_per_packet = u64::from(self.format.mFramesPerPacket);
        let (packet, aligned_frame) = if frames_per_packet > 0 {
            let packet = target_frame / frames_per_packet;
            (packet, packet * frames_per_packet)
        } else {
            let mut translation = AudioFramePacketTranslation {
                mFrame: target_frame as SInt64,
                ..Default::default()
            };
            let mut size =
                UInt32::try_from(size_of::<AudioFramePacketTranslation>()).map_err(|_| {
                    DecodeError::SeekFailed(
                        "AudioFramePacketTranslation size exceeds UInt32".into(),
                    )
                })?;
            // SAFETY: live AudioFileID + stack-allocated struct.
            let status = unsafe {
                AudioFileGetProperty(
                    self.file,
                    Consts::kAudioFilePropertyFrameToPacket,
                    &mut size,
                    &mut translation as *mut _ as *mut c_void,
                )
            };
            if status != Consts::noErr || translation.mPacket < 0 {
                return Err(DecodeError::SeekFailed(format!(
                    "FrameToPacket failed: {}",
                    os_status_to_string(status)
                )));
            }
            let packet = translation.mPacket.cast_unsigned();
            let aligned = target_frame.saturating_sub(u64::from(translation.mFrameOffsetInPacket));
            (packet, aligned)
        };
        self.next_packet = packet;
        Ok(aligned_frame)
    }
}

// SAFETY: The owned `AudioFileID` + `SourceCtx` are only touched on the
// decoder's owner thread. Thread-affinity is enforced at the `AppleInner`
// level via `assert_thread_affinity`.
unsafe impl Send for AudioFileReader {}

impl Drop for AudioFileReader {
    fn drop(&mut self) {
        if !self.file.is_null() {
            // SAFETY: `self.file` came from `AudioFileOpenWithCallbacks`
            // and has not been closed (Drop runs at most once).
            unsafe {
                AudioFileClose(self.file);
            }
        }
    }
}
