use std::{
    cell::Cell,
    io::{Error, ErrorKind, Read, Seek, SeekFrom},
};

use kithara_apple::audio_toolbox::{
    AUDIO_FILE_PROPERTY_AUDIO_DATA_PACKET_COUNT, AUDIO_FILE_PROPERTY_DATA_FORMAT,
    AUDIO_FILE_PROPERTY_MAGIC_COOKIE_DATA, AUDIO_FILE_PROPERTY_MAXIMUM_PACKET_SIZE, AudioFile,
    AudioFileCallbacks, AudioFilePacketRead, AudioStreamBasicDescription,
    AudioStreamPacketDescription, OSStatus, PARAM_ERR, SInt64, UInt32,
};

use super::consts::os_status_to_string;
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
    /// current `audio_file_read_packet_data` request. `AudioFile` collapses
    /// callback failures into a generic non-zero `OSStatus`, losing the
    /// underlying `PendingReason`/`VariantChangeError` chain that
    /// `DecodeError::classify` walks to decide retry vs terminal. We
    /// stash the original error here so the wrapper can rewrap it into
    /// `DecodeError::Backend` with the chain intact.
    last_error: Cell<Option<Error>>,
    size: SizeMode,
}

/// Safe wrapper around an Apple `AudioFile` handle backed by an
/// arbitrary `Read + Seek` source via `audio_file_open_with_callbacks`.
pub(crate) struct AppleAudioFile {
    handle: AudioFile<CallbackCtx>,
    pub(super) data_format: AudioStreamBasicDescription,
    pub(super) packet_count: Option<u64>,
    pub(super) max_packet_size: u32,
}

impl AppleAudioFile {
    pub(crate) fn magic_cookie(&self) -> Option<Vec<u8>> {
        read_magic_cookie(&self.handle)
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

    fn open_inner(
        source: BoxedSource,
        hint: Option<u32>,
        size: SizeMode,
        scan_packets: bool,
    ) -> DecodeResult<Self> {
        let has_size = !matches!(size, SizeMode::Unknown);
        let ctx = CallbackCtx {
            source,
            size,
            last_error: Cell::new(None),
        };
        let handle = AudioFile::open_with_callbacks(ctx, hint, has_size).map_err(|status| {
            DecodeError::BackendStatus {
                code: status,
                op: "AudioFileOpenWithCallbacks",
            }
        })?;
        if handle.callbacks().last_error.take().is_some() {
            return Err(DecodeError::BackendStatus {
                code: -1,
                op: "AudioFileOpenWithCallbacks",
            });
        }

        let data_format = read_data_format(&handle)?;
        // `read_packet_count` / `read_max_packet_size` force a full-file scan
        // for VBR formats with no on-disk packet index (FLAC). Only the
        // complete open (`Snapshot` + `scan_packets`) pays it; the streaming
        // paths source duration and the read buffer size from header metadata
        // (STREAMINFO) instead.
        let packet_count = if has_size && scan_packets {
            Some(read_packet_count(&handle)?)
        } else {
            None
        };
        let max_packet_size = if has_size && scan_packets {
            read_max_packet_size(&handle).unwrap_or(0)
        } else {
            0
        };

        Ok(Self {
            handle,
            data_format,
            packet_count,
            max_packet_size,
        })
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

    /// Translate an `audio_file_read_packet_data` failure into a
    /// `DecodeError::Backend`. When the source read callback stashed an
    /// `io::Error` (transient `PendingReason` / `VariantChangeError`
    /// from `Stream::read`), we forward that error through so
    /// `DecodeError::classify` can walk the chain and tag the failure
    /// as `Interrupted` / `VariantChange`. Otherwise we surface the raw
    /// `OSStatus` for terminal diagnostics.
    fn read_failure_error(&self, op: &str, status: OSStatus) -> DecodeError {
        let io_err = self
            .handle
            .callbacks()
            .last_error
            .take()
            .unwrap_or_else(|| {
                Error::other(format!("{op} failed: {}", os_status_to_string(status)))
            });
        DecodeError::backend(io_err)
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
        let mut packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();
        let starting_packet = SInt64::try_from(starting_packet).map_err(DecodeError::backend)?;
        self.handle.callbacks().last_error.set(None);
        let read =
            self.handle
                .read_packet_data(starting_packet, Some(&mut desc), &mut packets, buf);

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
        let AudioFilePacketRead { bytes, packets } = match read {
            Ok(read) => read,
            Err(status) => {
                return Err(self.read_failure_error("AudioFileReadPacketData", status));
            }
        };
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
        let mut packets: UInt32 = max_packets;
        let starting_packet = SInt64::try_from(starting_packet).map_err(DecodeError::backend)?;
        self.handle.callbacks().last_error.set(None);
        let read = self
            .handle
            .read_packet_data(starting_packet, None, &mut packets, buf);
        match read {
            Ok(read) => Ok((read.bytes, read.packets)),
            Err(status) => Err(self.read_failure_error("AudioFileReadPacketData(cbr)", status)),
        }
    }

    /// Take the read callback's stashed error and return it as a typed
    /// `DecodeError` ONLY if it is transient (a not-ready / pending read).
    /// `AudioFile` masks such a callback failure as a graceful EOF or a
    /// truncated packet, so the demuxer must surface it as `Pending` rather
    /// than trust the `status`/packets result. A non-transient stashed
    /// error (e.g. an incidental probe past a complete file's true EOF) is
    /// dropped: the genuine `status`/packets outcome stands.
    fn take_pending_callback_error(&self) -> Option<DecodeError> {
        self.handle.callbacks().last_error.take().and_then(|err| {
            let de = DecodeError::backend(err);
            de.pending_reason().is_some().then_some(de)
        })
    }
}

fn read_data_format(handle: &AudioFile<CallbackCtx>) -> DecodeResult<AudioStreamBasicDescription> {
    handle
        .get_property(AUDIO_FILE_PROPERTY_DATA_FORMAT)
        .map_err(|status| DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(DataFormat)",
        })
}

fn read_packet_count(handle: &AudioFile<CallbackCtx>) -> DecodeResult<u64> {
    handle
        .get_property(AUDIO_FILE_PROPERTY_AUDIO_DATA_PACKET_COUNT)
        .map_err(|status| DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(PacketCount)",
        })
}

fn read_max_packet_size(handle: &AudioFile<CallbackCtx>) -> DecodeResult<u32> {
    handle
        .get_property(AUDIO_FILE_PROPERTY_MAXIMUM_PACKET_SIZE)
        .map_err(|status| DecodeError::BackendStatus {
            code: status,
            op: "AudioFileGetProperty(MaxPacketSize)",
        })
}

fn read_magic_cookie(handle: &AudioFile<CallbackCtx>) -> Option<Vec<u8>> {
    handle
        .get_property_bytes(AUDIO_FILE_PROPERTY_MAGIC_COOKIE_DATA)
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

impl AudioFileCallbacks for CallbackCtx {
    fn read_at(&mut self, position: SInt64, buffer: &mut [u8]) -> Result<UInt32, OSStatus> {
        const UNKNOWN_SIZE_TAIL_PROBE_MIN: SInt64 = SInt64::MAX / 2;
        let Ok(pos) = u64::try_from(position) else {
            self.last_error.set(Some(Error::other(format!(
                "AppleAudioFile read_callback: negative position {position}"
            ))));
            return Err(PARAM_ERR);
        };
        if matches!(self.size, SizeMode::Unknown) && position >= UNKNOWN_SIZE_TAIL_PROBE_MIN {
            return Ok(0);
        }
        if let Err(err) = self.source.seek(SeekFrom::Start(pos)) {
            self.last_error.set(Some(err));
            return Err(PARAM_ERR);
        }
        let n = match self.source.read(buffer) {
            Ok(n) => n,
            Err(err) => {
                self.last_error.set(Some(err));
                return Err(PARAM_ERR);
            }
        };
        let Ok(n_u32) = UInt32::try_from(n) else {
            self.last_error.set(Some(Error::other(
                "AppleAudioFile read_callback: bytes read > u32::MAX",
            )));
            return Err(PARAM_ERR);
        };
        Ok(n_u32)
    }

    fn size(&self) -> SInt64 {
        match &self.size {
            SizeMode::Snapshot(size) => *size,
            SizeMode::Unknown => {
                self.last_error.set(Some(Error::other(
                    "AppleAudioFile get_size_callback called without a known size",
                )));
                0
            }
        }
    }
}
