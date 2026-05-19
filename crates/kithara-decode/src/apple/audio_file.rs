#![allow(unsafe_code)]

use std::{
    cell::Cell,
    ffi::c_void,
    io::{self, Read, Seek, SeekFrom},
    ptr,
};

use super::{
    consts::{Consts, os_status_to_string},
    ffi::{
        AudioFileClose, AudioFileGetProperty, AudioFileGetPropertyInfo, AudioFileID,
        AudioFileOpenWithCallbacks, AudioFileReadPacketData, AudioStreamBasicDescription,
        AudioStreamPacketDescription, OSStatus, SInt64, UInt32,
    },
};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

struct CallbackCtx {
    source: BoxedSource,
    /// Last `io::Error` raised by the read callback while filling the
    /// current `AudioFileReadPacketData` request. `AudioFile` collapses
    /// callback failures into a generic non-zero `OSStatus`, losing the
    /// underlying `PendingReason`/`VariantChangeError` chain that
    /// `DecodeError::classify` walks to decide retry vs terminal. We
    /// stash the original error here so the wrapper can rewrap it into
    /// `DecodeError::Backend` with the chain intact.
    last_error: Cell<Option<io::Error>>,
    size: i64,
}

/// Safe wrapper around an Apple `AudioFile` handle backed by an
/// arbitrary `Read + Seek` source via `AudioFileOpenWithCallbacks`.
pub(crate) struct AppleAudioFile {
    handle: AudioFileID,
    data_format: AudioStreamBasicDescription,
    _ctx: Box<CallbackCtx>,
    max_packet_size: u32,
    packet_count: u64,
}

// SAFETY: `AudioFileID` is an opaque kernel handle accessed only through
unsafe impl Send for AppleAudioFile {}

impl AppleAudioFile {
    pub(crate) fn data_format(&self) -> AudioStreamBasicDescription {
        self.data_format
    }

    pub(crate) fn magic_cookie(&self) -> Option<Vec<u8>> {
        read_magic_cookie(self.handle)
    }

    pub(crate) fn max_packet_size(&self) -> u32 {
        self.max_packet_size
    }

    /// Open `source` as an audio file. `hint` is one of the
    /// `kAudioFile*Type` four-cc constants in [`Consts`]; pass `None`
    /// to let `AudioFileServices` auto-detect.
    pub(crate) fn open(mut source: BoxedSource, hint: Option<u32>) -> DecodeResult<Self> {
        let end = source
            .seek(SeekFrom::End(0))
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;
        let size = i64::try_from(end).map_err(|e| DecodeError::Backend(Box::new(e)))?;
        let mut ctx = Box::new(CallbackCtx {
            source,
            size,
            last_error: Cell::new(None),
        });
        let mut handle: AudioFileID = ptr::null_mut();

        // SAFETY: `ctx` is boxed (stable address) and outlives the
        let status = unsafe {
            AudioFileOpenWithCallbacks(
                ctx.as_mut() as *mut CallbackCtx as *mut c_void,
                read_callback,
                ptr::null(),
                get_size_callback,
                ptr::null(),
                hint.unwrap_or(0),
                &mut handle,
            )
        };
        if status != Consts::noErr {
            return Err(DecodeError::Backend(Box::new(io::Error::other(format!(
                "AudioFileOpenWithCallbacks failed: {}",
                os_status_to_string(status)
            )))));
        }

        let data_format = read_data_format(handle)?;
        let packet_count = read_packet_count(handle)?;
        let max_packet_size = read_max_packet_size(handle).unwrap_or(0);

        Ok(Self {
            handle,
            data_format,
            packet_count,
            max_packet_size,
            _ctx: ctx,
        })
    }

    pub(crate) fn packet_count(&self) -> u64 {
        self.packet_count
    }

    /// Translate an `AudioFileReadPacketData` failure into a
    /// `DecodeError::Backend`. When the source read callback stashed an
    /// `io::Error` (transient `PendingReason` / `VariantChangeError`
    /// from `Stream::read`), we forward that error through so
    /// `DecodeError::classify` can walk the chain and tag the failure
    /// as `Interrupted` / `VariantChange`. Otherwise we surface the raw
    /// `OSStatus` for terminal diagnostics.
    fn read_failure_error(&self, op: &str, status: OSStatus) -> DecodeError {
        let io_err = self._ctx.last_error.take().unwrap_or_else(|| {
            io::Error::other(format!("{op} failed: {}", os_status_to_string(status)))
        });
        DecodeError::Backend(Box::new(io_err))
    }

    /// Read one VBR packet at `starting_packet` into `buf`. Returns
    /// `Ok(Some((bytes_written, packet_desc)))` or `Ok(None)` at EOF.
    /// Use for codecs whose decoder needs per-packet descriptors
    /// (MP3 / ALAC).
    pub(crate) fn read_packet(
        &mut self,
        starting_packet: u64,
        buf: &mut [u8],
    ) -> DecodeResult<Option<(u32, AudioStreamPacketDescription)>> {
        let mut bytes =
            UInt32::try_from(buf.len()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
        let mut packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();
        self._ctx.last_error.set(None);

        // SAFETY: `self.handle` is non-null (constructor validated it);
        let status = unsafe {
            AudioFileReadPacketData(
                self.handle,
                0,
                &mut bytes,
                &mut desc,
                SInt64::try_from(starting_packet).unwrap_or(SInt64::MAX),
                &mut packets,
                buf.as_mut_ptr() as *mut c_void,
            )
        };

        if status != Consts::noErr {
            return Err(self.read_failure_error("AudioFileReadPacketData", status));
        }
        if packets == 0 {
            return Ok(None);
        }
        Ok(Some((bytes, desc)))
    }

    /// Read up to `max_packets` CBR packets starting at
    /// `starting_packet`. Returns `Ok((bytes_written, packets_read))`,
    /// where `packets_read == 0` at EOF. No `AudioStreamPacketDescription`
    /// is produced (CBR codecs reconstruct packet boundaries from the
    /// fixed `bytes_per_packet`). Use for CBR codecs (`LinearPCM`) to
    /// amortise the per-source-read cost.
    pub(crate) fn read_packets_cbr(
        &mut self,
        starting_packet: u64,
        max_packets: u32,
        buf: &mut [u8],
    ) -> DecodeResult<(u32, u32)> {
        let mut bytes =
            UInt32::try_from(buf.len()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
        let mut packets: UInt32 = max_packets;
        self._ctx.last_error.set(None);
        // SAFETY: `self.handle` non-null; out-params exclusively
        let status = unsafe {
            AudioFileReadPacketData(
                self.handle,
                0,
                &mut bytes,
                ptr::null_mut(),
                SInt64::try_from(starting_packet).unwrap_or(SInt64::MAX),
                &mut packets,
                buf.as_mut_ptr() as *mut c_void,
            )
        };
        if status != Consts::noErr {
            return Err(self.read_failure_error("AudioFileReadPacketData(cbr)", status));
        }
        Ok((bytes, packets))
    }
}

impl Drop for AppleAudioFile {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` came from `AudioFileOpenWithCallbacks` and
            let _ = unsafe { AudioFileClose(self.handle) };
        }
    }
}

fn read_data_format(handle: AudioFileID) -> DecodeResult<AudioStreamBasicDescription> {
    let mut asbd = AudioStreamBasicDescription::default();
    let mut size = UInt32::try_from(size_of::<AudioStreamBasicDescription>())
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` is live; `asbd` and `size` are exclusively
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyDataFormat,
            &mut size,
            &mut asbd as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(io::Error::other(format!(
            "AudioFileGetProperty(DataFormat) failed: {}",
            os_status_to_string(status)
        )))));
    }
    Ok(asbd)
}

fn read_packet_count(handle: AudioFileID) -> DecodeResult<u64> {
    let mut count: u64 = 0;
    let mut size =
        UInt32::try_from(size_of::<u64>()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` live; `count`/`size` exclusively borrowed; size
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyAudioDataPacketCount,
            &mut size,
            &mut count as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(io::Error::other(format!(
            "AudioFileGetProperty(PacketCount) failed: {}",
            os_status_to_string(status)
        )))));
    }
    Ok(count)
}

fn read_max_packet_size(handle: AudioFileID) -> DecodeResult<u32> {
    let mut sz: u32 = 0;
    let mut size =
        UInt32::try_from(size_of::<u32>()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` live; `sz`/`size` exclusively borrowed; size
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyMaximumPacketSize,
            &mut size,
            &mut sz as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(io::Error::other(format!(
            "AudioFileGetProperty(MaxPacketSize) failed: {}",
            os_status_to_string(status)
        )))));
    }
    Ok(sz)
}

fn read_magic_cookie(handle: AudioFileID) -> Option<Vec<u8>> {
    let mut size: UInt32 = 0;
    let mut writable: UInt32 = 0;
    // SAFETY: `handle` live; out-params exclusively borrowed.
    let status = unsafe {
        AudioFileGetPropertyInfo(
            handle,
            Consts::kAudioFilePropertyMagicCookieData,
            &mut size,
            &mut writable,
        )
    };
    if status != Consts::noErr || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: `handle` live; `buf` exclusively borrowed; `size` reflects
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyMagicCookieData,
            &mut size,
            buf.as_mut_ptr() as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

extern "C" fn read_callback(
    user_data: *mut c_void,
    position: SInt64,
    request: UInt32,
    buffer: *mut c_void,
    actual: *mut UInt32,
) -> OSStatus {
    // SAFETY: `user_data` is the boxed `CallbackCtx` we pinned in
    let (ctx, slice, actual) = unsafe {
        (
            &mut *(user_data as *mut CallbackCtx),
            std::slice::from_raw_parts_mut(buffer as *mut u8, request as usize),
            &mut *actual,
        )
    };

    let Ok(pos) = u64::try_from(position) else {
        *actual = 0;
        ctx.last_error.set(Some(io::Error::other(format!(
            "AppleAudioFile read_callback: negative position {position}"
        ))));
        return -1;
    };
    if let Err(e) = ctx.source.seek(SeekFrom::Start(pos)) {
        *actual = 0;
        ctx.last_error.set(Some(e));
        return -1;
    }
    let n = match ctx.source.read(slice) {
        Ok(n) => n,
        Err(e) => {
            *actual = 0;
            ctx.last_error.set(Some(e));
            return -1;
        }
    };
    let Ok(n_u32) = UInt32::try_from(n) else {
        *actual = 0;
        ctx.last_error.set(Some(io::Error::other(
            "AppleAudioFile read_callback: bytes read > u32::MAX",
        )));
        return -1;
    };
    *actual = n_u32;
    Consts::noErr
}

extern "C" fn get_size_callback(user_data: *mut c_void) -> SInt64 {
    // SAFETY: `user_data` is the boxed `CallbackCtx` we pinned in
    let ctx = unsafe { &*(user_data as *const CallbackCtx) };
    ctx.size
}
