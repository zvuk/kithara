#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
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
    size: i64,
}

/// Safe wrapper around an Apple `AudioFile` handle backed by an
/// arbitrary `Read + Seek` source via `AudioFileOpenWithCallbacks`.
pub(crate) struct AppleAudioFile {
    handle: AudioFileID,
    _ctx: Box<CallbackCtx>,
    data_format: AudioStreamBasicDescription,
    packet_count: u64,
    max_packet_size: u32,
}

// SAFETY: `AudioFileID` is an opaque kernel handle accessed only through
// the owning `AppleAudioFile`; we never share without `&mut`.
unsafe impl Send for AppleAudioFile {}

impl AppleAudioFile {
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
        let mut ctx = Box::new(CallbackCtx { source, size });
        let mut handle: AudioFileID = ptr::null_mut();

        // SAFETY: `ctx` is boxed (stable address) and outlives the
        // `AudioFile` handle (dropped together below). Callbacks
        // dereference it as `*mut CallbackCtx`. `handle` is an out-param
        // and is written before any other use.
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
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!(
                    "AudioFileOpenWithCallbacks failed: {}",
                    os_status_to_string(status)
                ),
            ))));
        }

        let data_format = read_data_format(handle)?;
        let packet_count = read_packet_count(handle)?;
        let max_packet_size = read_max_packet_size(handle).unwrap_or(0);

        Ok(Self {
            handle,
            _ctx: ctx,
            data_format,
            packet_count,
            max_packet_size,
        })
    }

    pub(crate) fn data_format(&self) -> AudioStreamBasicDescription {
        self.data_format
    }

    pub(crate) fn packet_count(&self) -> u64 {
        self.packet_count
    }

    pub(crate) fn max_packet_size(&self) -> u32 {
        self.max_packet_size
    }

    pub(crate) fn magic_cookie(&self) -> Option<Vec<u8>> {
        read_magic_cookie(self.handle)
    }

    /// Read up to `buf.len()` bytes starting at `starting_packet`.
    /// Returns `Ok(Some((bytes_written, packet_desc)))` for a real
    /// packet, `Ok(None)` at EOF. `buf` must be sized to fit at least
    /// one packet (use [`Self::max_packet_size`]).
    pub(crate) fn read_packet(
        &mut self,
        starting_packet: u64,
        buf: &mut [u8],
    ) -> DecodeResult<Option<(u32, AudioStreamPacketDescription)>> {
        let mut bytes =
            UInt32::try_from(buf.len()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
        let mut packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();

        // SAFETY: `self.handle` is non-null (constructor validated it);
        // all out-params are exclusively borrowed; `buf` is a valid
        // exclusive slice with capacity `buf.len()`.
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
            return Err(DecodeError::Backend(Box::new(std::io::Error::other(
                format!(
                    "AudioFileReadPacketData failed: {}",
                    os_status_to_string(status)
                ),
            ))));
        }
        if packets == 0 {
            return Ok(None);
        }
        Ok(Some((bytes, desc)))
    }
}

impl Drop for AppleAudioFile {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` came from `AudioFileOpenWithCallbacks` and
            // is non-null; we are the sole owner.
            let _ = unsafe { AudioFileClose(self.handle) };
        }
    }
}

fn read_data_format(handle: AudioFileID) -> DecodeResult<AudioStreamBasicDescription> {
    let mut asbd = AudioStreamBasicDescription::default();
    let mut size = UInt32::try_from(size_of::<AudioStreamBasicDescription>())
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` is live; `asbd` and `size` are exclusively
    // borrowed out-params; `size` matches the ASBD struct size.
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyDataFormat,
            &mut size,
            &mut asbd as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(std::io::Error::other(
            format!(
                "AudioFileGetProperty(DataFormat) failed: {}",
                os_status_to_string(status)
            ),
        ))));
    }
    Ok(asbd)
}

fn read_packet_count(handle: AudioFileID) -> DecodeResult<u64> {
    let mut count: u64 = 0;
    let mut size =
        UInt32::try_from(size_of::<u64>()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` live; `count`/`size` exclusively borrowed; size
    // matches the property type.
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyAudioDataPacketCount,
            &mut size,
            &mut count as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(std::io::Error::other(
            format!(
                "AudioFileGetProperty(PacketCount) failed: {}",
                os_status_to_string(status)
            ),
        ))));
    }
    Ok(count)
}

fn read_max_packet_size(handle: AudioFileID) -> DecodeResult<u32> {
    let mut sz: u32 = 0;
    let mut size =
        UInt32::try_from(size_of::<u32>()).map_err(|e| DecodeError::Backend(Box::new(e)))?;
    // SAFETY: `handle` live; `sz`/`size` exclusively borrowed; size
    // matches the property type.
    let status = unsafe {
        AudioFileGetProperty(
            handle,
            Consts::kAudioFilePropertyMaximumPacketSize,
            &mut size,
            &mut sz as *mut _ as *mut c_void,
        )
    };
    if status != Consts::noErr {
        return Err(DecodeError::Backend(Box::new(std::io::Error::other(
            format!(
                "AudioFileGetProperty(MaxPacketSize) failed: {}",
                os_status_to_string(status)
            ),
        ))));
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
    // the alloc length and the actual cookie length reported by NDK.
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
    // `AppleAudioFile::open` and outlives every read callback invocation;
    // we hold an exclusive borrow for the body. `buffer` and `request`
    // come from AudioToolbox; the buffer is valid for `request` bytes.
    // `actual` is an out-param exclusively owned by us for the call.
    let (ctx, slice, actual) = unsafe {
        (
            &mut *(user_data as *mut CallbackCtx),
            std::slice::from_raw_parts_mut(buffer as *mut u8, request as usize),
            &mut *actual,
        )
    };

    let Ok(pos) = u64::try_from(position) else {
        *actual = 0;
        return -1;
    };
    if ctx.source.seek(SeekFrom::Start(pos)).is_err() {
        *actual = 0;
        return -1;
    }
    let Ok(n) = ctx.source.read(slice) else {
        *actual = 0;
        return -1;
    };
    let Ok(n_u32) = UInt32::try_from(n) else {
        *actual = 0;
        return -1;
    };
    *actual = n_u32;
    Consts::noErr
}

extern "C" fn get_size_callback(user_data: *mut c_void) -> SInt64 {
    // SAFETY: `user_data` is the boxed `CallbackCtx` we pinned in
    // `AppleAudioFile::open`.
    let ctx = unsafe { &*(user_data as *const CallbackCtx) };
    ctx.size
}

#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod tests {
    use std::io::Cursor;

    use kithara_test_utils::{ensure_silence_1s_alac_m4a, kithara};

    use super::{AppleAudioFile, Consts};

    #[kithara::test]
    fn open_wav_silence_returns_linear_pcm_asbd() {
        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("assets/silence_1s.wav"),
        )
        .expect("read silence_1s.wav");
        let cursor = Cursor::new(bytes);
        let file = AppleAudioFile::open(Box::new(cursor), Some(Consts::kAudioFileWAVEType))
            .expect("AppleAudioFile::open WAV must succeed");

        let asbd = file.data_format();
        assert_eq!(asbd.mFormatID, Consts::kAudioFormatLinearPCM);
        assert_eq!(asbd.mChannelsPerFrame, 2);
        assert!(asbd.mSampleRate > 0.0);
        assert!(file.packet_count() > 0);
    }

    #[kithara::test]
    fn open_mp3_reports_mpeg_layer3_format() {
        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("assets/test.mp3"),
        )
        .expect("read test.mp3");
        let file = AppleAudioFile::open(
            Box::new(Cursor::new(bytes)),
            Some(Consts::kAudioFileMP3Type),
        )
        .expect("open MP3");
        assert_eq!(file.data_format().mFormatID, Consts::kAudioFormatMPEGLayer3);
        assert!(file.packet_count() > 0);
    }

    #[kithara::test]
    fn open_alac_m4a_reports_magic_cookie() {
        let path = ensure_silence_1s_alac_m4a();
        let bytes = std::fs::read(&path).expect("read alac fixture");
        let file = AppleAudioFile::open(
            Box::new(Cursor::new(bytes)),
            Some(Consts::kAudioFileM4AType),
        )
        .expect("open ALAC m4a");
        assert_eq!(
            file.data_format().mFormatID,
            Consts::kAudioFormatAppleLossless
        );
        let cookie = file.magic_cookie().expect("ALAC must have a magic cookie");
        assert!(cookie.len() >= 24);
    }
}
