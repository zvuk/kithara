#![allow(unsafe_code)]

use std::{
    cell::Cell,
    ffi::c_void,
    io::{Error, ErrorKind, Read, Seek, SeekFrom},
    ptr,
};

use super::{
    consts::{Consts, os_status_to_string},
    ffi::{
        AudioFile_GetSizeProc, AudioFileClose, AudioFileGetProperty, AudioFileGetPropertyInfo,
        AudioFileID, AudioFileOpenWithCallbacks, AudioFileReadPacketData,
        AudioStreamBasicDescription, AudioStreamPacketDescription, OSStatus, SInt64, UInt32,
    },
};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// How `AppleAudioFile` answers `AudioFileServices`' file-size query.
enum SizeMode {
    /// Total length is unknown (no `Content-Length`). `AudioFile` gets no
    /// `get_size` proc and a tail probe is answered with EOF; reads run to
    /// the source's natural end. Used for MP3 streaming.
    Unknown,
    /// A known total length. For a complete local file
    /// ([`AppleAudioFile::open`]) it is the file size; for a streamed source
    /// ([`AppleAudioFile::open_sized_streaming`]) it is the authoritative
    /// total the source reports at open (`Content-Length` / committed size —
    /// the source never reports a partial in-flight length as the total, so
    /// this is correct, not a prefix). `get_size` returns it directly — cheap
    /// (called ~per packet by `AudioFileServices`) and stable.
    Snapshot(i64),
}

struct CallbackCtx {
    source: BoxedSource,
    /// Last `io::Error` raised by the read callback while filling the
    /// current `AudioFileReadPacketData` request. `AudioFile` collapses
    /// callback failures into a generic non-zero `OSStatus`, losing the
    /// underlying `PendingReason`/`VariantChangeError` chain that
    /// `DecodeError::classify` walks to decide retry vs terminal. We
    /// stash the original error here so the wrapper can rewrap it into
    /// `DecodeError::Backend` with the chain intact.
    last_error: Cell<Option<Error>>,
    size: SizeMode,
}

/// Safe wrapper around an Apple `AudioFile` handle backed by an
/// arbitrary `Read + Seek` source via `AudioFileOpenWithCallbacks`.
pub(crate) struct AppleAudioFile {
    handle: AudioFileID,
    data_format: AudioStreamBasicDescription,
    _ctx: Box<CallbackCtx>,
    packet_count: Option<u64>,
    max_packet_size: u32,
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

    /// Open a complete local `source` as an audio file. `hint` is one of the
    /// `kAudioFile*Type` four-cc constants in [`Consts`]; pass `None` to let
    /// `AudioFileServices` auto-detect. The total length is fully present, so
    /// the size is frozen ([`SizeMode::Snapshot`]) and the packet count /
    /// max packet size are resolved eagerly — for VBR formats with no on-disk
    /// index (FLAC) that triggers a full-file scan, so streamed sources must
    /// use [`Self::open_sized_streaming`] / [`Self::open_streaming`] instead.
    pub(crate) fn open(source: BoxedSource, hint: Option<u32>) -> DecodeResult<Self> {
        let (source, end) = Self::probe_end(source)?;
        let size = match end {
            Some(end) => SizeMode::Snapshot(i64::try_from(end).map_err(DecodeError::backend)?),
            None => SizeMode::Unknown,
        };
        Self::open_inner(source, hint, size, true)
    }

    /// Open a streamed `source` whose total length the source reports at open
    /// (`Content-Length` / committed size). Skips the eager packet-count scan
    /// (the demuxer sources duration / read-buffer size from header metadata);
    /// otherwise identical to [`Self::open`]. The known total is what lets the
    /// codec treat a not-ready read as a transient `Pending` (not EOF) and
    /// resolve seeks by size-estimation instead of an O(N) forward frame-scan.
    /// If the source reports no length (no `Content-Length`, not yet
    /// committed), falls back to the size-less [`SizeMode::Unknown`] path
    /// ([`Self::open_streaming`]).
    ///
    /// The source must report its TRUE total here, never a partial in-flight
    /// length: `AudioFileServices` never reads past the size `get_size`
    /// reports, so a partial would pin a false EOF at the open-time prefix
    /// (the "plays a fraction then freezes" device bug). `FileSource::len`
    /// owns that guarantee.
    pub(crate) fn open_sized_streaming(
        source: BoxedSource,
        hint: Option<u32>,
    ) -> DecodeResult<Self> {
        let (mut source, end) = Self::probe_end(source)?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(DecodeError::backend)?;
        let size = match end {
            Some(end) => SizeMode::Snapshot(i64::try_from(end).map_err(DecodeError::backend)?),
            None => SizeMode::Unknown,
        };
        Self::open_inner(source, hint, size, false)
    }

    /// Probe the source length via a seek-to-end, restoring the cursor to the
    /// start. `None` means the source reports no length (`ErrorKind::Unsupported`
    /// — e.g. a chunked stream with no `Content-Length`, or a streamed source
    /// not yet committed). A streamed source reports its TRUE total here (never
    /// a partial in-flight prefix) — see [`Self::open_sized_streaming`].
    fn probe_end(mut source: BoxedSource) -> DecodeResult<(BoxedSource, Option<u64>)> {
        let end = match source.seek(SeekFrom::End(0)) {
            Ok(end) => Some(end),
            Err(err) if err.kind() == ErrorKind::Unsupported => None,
            Err(err) => return Err(DecodeError::backend(err)),
        };
        source
            .seek(SeekFrom::Start(0))
            .map_err(DecodeError::backend)?;
        Ok((source, end))
    }

    fn open_inner(
        source: BoxedSource,
        hint: Option<u32>,
        size: SizeMode,
        scan_packets: bool,
    ) -> DecodeResult<Self> {
        let has_size = !matches!(size, SizeMode::Unknown);
        let mut ctx = Box::new(CallbackCtx {
            source,
            size,
            last_error: Cell::new(None),
        });
        let mut handle: AudioFileID = ptr::null_mut();

        let get_size_fn: AudioFile_GetSizeProc = get_size_callback;
        let get_size = has_size.then_some(get_size_fn);
        // SAFETY: `ctx` is boxed (stable address) and outlives the
        let status = unsafe {
            AudioFileOpenWithCallbacks(
                ctx.as_mut() as *mut CallbackCtx as *mut c_void,
                read_callback,
                ptr::null(),
                get_size,
                ptr::null(),
                hint.unwrap_or(0),
                &mut handle,
            )
        };
        if status != Consts::noErr {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioFileOpenWithCallbacks",
            });
        }

        let data_format = read_data_format(handle)?;
        // `read_packet_count` / `read_max_packet_size` force a full-file scan
        // for VBR formats with no on-disk packet index (FLAC). Only the
        // complete open (`Snapshot` + `scan_packets`) pays it; the streaming
        // paths source duration and the read buffer size from header metadata
        // (STREAMINFO) instead.
        let packet_count = if has_size && scan_packets {
            Some(read_packet_count(handle)?)
        } else {
            None
        };
        let max_packet_size = if has_size && scan_packets {
            read_max_packet_size(handle).unwrap_or(0)
        } else {
            0
        };

        Ok(Self {
            handle,
            data_format,
            packet_count,
            max_packet_size,
            _ctx: ctx,
        })
    }

    pub(crate) fn open_streaming(
        mut source: BoxedSource,
        hint: Option<u32>,
        known_size: Option<u64>,
    ) -> DecodeResult<Self> {
        source
            .seek(SeekFrom::Start(0))
            .map_err(DecodeError::backend)?;
        let size = match known_size {
            Some(known) => SizeMode::Snapshot(i64::try_from(known).map_err(DecodeError::backend)?),
            None => SizeMode::Unknown,
        };
        Self::open_inner(source, hint, size, false)
    }

    pub(crate) fn packet_count(&self) -> Option<u64> {
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
            Error::other(format!("{op} failed: {}", os_status_to_string(status)))
        });
        DecodeError::backend(io_err)
    }

    /// Take the read callback's stashed error and return it as a typed
    /// `DecodeError` ONLY if it is transient (a not-ready / pending read).
    /// `AudioFile` masks such a callback failure as a graceful EOF or a
    /// truncated packet, so the demuxer must surface it as `Pending` rather
    /// than trust the `status`/packets result. A non-transient stashed
    /// error (e.g. an incidental probe past a complete file's true EOF) is
    /// dropped: the genuine `status`/packets outcome stands.
    fn take_pending_callback_error(&self) -> Option<DecodeError> {
        self._ctx.last_error.take().and_then(|err| {
            let de = DecodeError::backend(err);
            de.pending_reason().is_some().then_some(de)
        })
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
        let mut bytes = UInt32::try_from(buf.len()).map_err(DecodeError::backend)?;
        let mut packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();
        let starting_packet = SInt64::try_from(starting_packet).map_err(DecodeError::backend)?;
        self._ctx.last_error.set(None);

        // SAFETY: `self.handle` is non-null (constructor validated it);
        let status = unsafe {
            AudioFileReadPacketData(
                self.handle,
                0,
                &mut bytes,
                &mut desc,
                starting_packet,
                &mut packets,
                buf.as_mut_ptr() as *mut c_void,
            )
        };

        // A not-ready streamed read is masked by AudioFile either as a
        // graceful EOF (noErr, 0 packets) or as a truncated packet (a short
        // `packets >= 1` with a stashed callback error) — both for
        // compressed formats. Surface the transient error BEFORE trusting
        // `status`/`packets` so it classifies as `Pending` and the partial
        // read is discarded. A non-transient stashed error (an incidental
        // probe past a complete file's true EOF) is ignored so genuine
        // end-of-stream still reports `Ok(None)`.
        if let Some(pending) = self.take_pending_callback_error() {
            return Err(pending);
        }
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
        let mut bytes = UInt32::try_from(buf.len()).map_err(DecodeError::backend)?;
        let mut packets: UInt32 = max_packets;
        let starting_packet = SInt64::try_from(starting_packet).map_err(DecodeError::backend)?;
        self._ctx.last_error.set(None);
        // SAFETY: `self.handle` non-null; out-params exclusively
        let status = unsafe {
            AudioFileReadPacketData(
                self.handle,
                0,
                &mut bytes,
                ptr::null_mut(),
                starting_packet,
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
    let mut size =
        UInt32::try_from(size_of::<AudioStreamBasicDescription>()).map_err(DecodeError::backend)?;
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
        return Err(DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(DataFormat)",
        });
    }
    Ok(asbd)
}

fn read_packet_count(handle: AudioFileID) -> DecodeResult<u64> {
    let mut count: u64 = 0;
    let mut size = UInt32::try_from(size_of::<u64>()).map_err(DecodeError::backend)?;
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
        return Err(DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(PacketCount)",
        });
    }
    Ok(count)
}

fn read_max_packet_size(handle: AudioFileID) -> DecodeResult<u32> {
    let mut sz: u32 = 0;
    let mut size = UInt32::try_from(size_of::<u32>()).map_err(DecodeError::backend)?;
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
        return Err(DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(MaxPacketSize)",
        });
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
    const UNKNOWN_SIZE_TAIL_PROBE_MIN: SInt64 = SInt64::MAX / 2;

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
        ctx.last_error.set(Some(Error::other(format!(
            "AppleAudioFile read_callback: negative position {position}"
        ))));
        return -1;
    };
    if matches!(ctx.size, SizeMode::Unknown) && position >= UNKNOWN_SIZE_TAIL_PROBE_MIN {
        *actual = 0;
        return Consts::noErr;
    }
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
        ctx.last_error.set(Some(Error::other(
            "AppleAudioFile read_callback: bytes read > u32::MAX",
        )));
        return -1;
    };
    *actual = n_u32;
    Consts::noErr
}

extern "C" fn get_size_callback(user_data: *mut c_void) -> SInt64 {
    // SAFETY: `user_data` is the boxed `CallbackCtx` we pinned in
    // `open_inner`; `&mut` is sound because `AudioFile` never calls
    // `get_size` and `read_callback` re-entrantly (it is synchronous).
    let ctx = unsafe { &*(user_data as *const CallbackCtx) };
    match &ctx.size {
        SizeMode::Snapshot(size) => *size,
        SizeMode::Unknown => {
            ctx.last_error.set(Some(Error::other(
                "AppleAudioFile get_size_callback called without a known size",
            )));
            0
        }
    }
}
