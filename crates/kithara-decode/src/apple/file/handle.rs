use std::{
    io::{ErrorKind, SeekFrom},
    sync::atomic::AtomicU64,
};

use kithara_apple::audio_toolbox::{
    AUDIO_FILE_PROPERTY_AUDIO_DATA_PACKET_COUNT, AUDIO_FILE_PROPERTY_DATA_FORMAT,
    AUDIO_FILE_PROPERTY_DATA_OFFSET, AUDIO_FILE_PROPERTY_MAGIC_COOKIE_DATA,
    AUDIO_FILE_PROPERTY_MAXIMUM_PACKET_SIZE, AUDIO_FILE_PROPERTY_PACKET_SIZE_UPPER_BOUND,
    AUDIO_FILE_PROPERTY_PACKET_TABLE_INFO, AUDIO_FILE_PROPERTY_PACKET_TO_BYTE, AUDIO_FORMAT_FLAC,
    AudioBytePacketTranslation, AudioFile, AudioFilePacketRead, AudioFilePacketTableInfo,
    AudioStreamBasicDescription, AudioStreamPacketDescription, SInt64, UInt32,
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;

use super::callbacks::{CallbackCtx, SizeMode};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// Safe wrapper around an Apple `AudioFile` handle backed by an
/// arbitrary `Read + Seek` source via `audio_file_open_with_callbacks`.
pub(crate) struct AppleAudioFile {
    pub(in crate::apple) data_format: AudioStreamBasicDescription,
    pub(in crate::apple) packet_count: Option<u64>,
    pub(in crate::apple) max_packet_size: u32,
    handle: AudioFile<CallbackCtx>,
    flac_frontier: Option<FlacFrontier>,
}

#[derive(Clone, Copy)]
struct FlacFrontier {
    packet: u64,
    byte: u64,
}

impl FlacFrontier {
    fn advance(self, bytes: UInt32) -> DecodeResult<Self> {
        let packet = self.packet.checked_add(1).ok_or(DecodeError::InvalidData {
            detail: "native FLAC packet index overflow",
        })?;
        let byte = self
            .byte
            .checked_add(u64::from(bytes))
            .ok_or(DecodeError::InvalidData {
                detail: "native FLAC byte offset overflow",
            })?;
        Ok(Self { packet, byte })
    }
}

impl AppleAudioFile {
    pub(crate) fn valid_frames(&self) -> DecodeResult<u64> {
        let table: AudioFilePacketTableInfo = self
            .handle
            .get_property(AUDIO_FILE_PROPERTY_PACKET_TABLE_INFO)
            .map_err(|status| DecodeError::BackendStatus {
                code: status,
                op: "AudioFileGetProperty(PacketTableInfo)",
            })?;
        u64::try_from(table.number_valid_frames).map_err(|_| DecodeError::InvalidData {
            detail: "negative valid frame count",
        })
    }

    pub(crate) fn magic_cookie(&self) -> Option<Vec<u8>> {
        read_magic_cookie(&self.handle)
    }

    /// Open a complete local `source` as an audio file. `hint` is one of the
    /// `kAudioFile*Type` four-cc constants in [`crate::apple::consts`]; pass `None` to let
    /// `AudioFileServices` auto-detect. The total length is fully present, so
    /// the size is frozen ([`SizeMode::Snapshot`]) and the packet count /
    /// max packet size are resolved eagerly — for VBR formats with no on-disk
    /// index (FLAC) that triggers a full-file scan, so streamed sources must
    /// use [`Self::open_sized_streaming`] instead.
    pub(crate) fn open<S>(
        source: BoxedSource,
        hint: Option<u32>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let (source, end) = Self::probe_end(source)?;
        let size = match end {
            Some(end) => SizeMode::Snapshot(i64::try_from(end).map_err(DecodeError::backend)?),
            None => SizeMode::Unknown,
        };
        Self::open_inner(source, hint, size, true, pools)
    }

    /// `read_packet_count` and `read_max_packet_size` force a full-file scan for VBR formats with
    /// no on-disk packet index, such as FLAC.
    /// Discard stale callback errors after a successful open: optional streamed tail probes
    /// may fail without invalidating the opened file.
    fn open_inner<S>(
        source: BoxedSource,
        hint: Option<u32>,
        size: SizeMode,
        scan_packets: bool,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let has_size = !matches!(size, SizeMode::Unknown);
        let ctx = CallbackCtx::new(source, size);
        let handle = AudioFile::open_with_callbacks(ctx, hint, has_size).map_err(|status| {
            DecodeError::BackendStatus {
                code: status,
                op: "AudioFileOpenWithCallbacks",
            }
        })?;
        handle.callbacks().last_error.set(None);
        let data_format = read_data_format(&handle)?;
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

        let flac_frontier = if !scan_packets && data_format.format_id == AUDIO_FORMAT_FLAC {
            Some(Self::init_flac_frontier(&handle, pools)?)
        } else {
            None
        };

        Ok(Self {
            data_format,
            packet_count,
            max_packet_size,
            handle,
            flac_frontier,
        })
    }

    fn init_flac_frontier<S>(
        handle: &AudioFile<CallbackCtx>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<FlacFrontier>
    where
        S: HasPool<u8>,
    {
        // A header has four fixed bytes, up to seven counter bytes, four optional size/rate
        // bytes and a CRC. The native body scan and its next-header probe need this headroom.
        const MAX_FRAME_HEADER_BYTES: usize = 16;
        let upper_bound: UInt32 = handle
            .get_property(AUDIO_FILE_PROPERTY_PACKET_SIZE_UPPER_BOUND)
            .map_err(|code| DecodeError::BackendStatus {
                code,
                op: "AudioFileGetProperty(PacketSizeUpperBound)",
            })?;
        let window_size = usize::try_from(upper_bound)
            .map_err(DecodeError::backend)?
            .checked_add(MAX_FRAME_HEADER_BYTES)
            .ok_or(DecodeError::InvalidData {
                detail: "native FLAC read window size overflow",
            })?;
        let offset: SInt64 = handle
            .get_property(AUDIO_FILE_PROPERTY_DATA_OFFSET)
            .map_err(|code| DecodeError::BackendStatus {
                code,
                op: "AudioFileGetProperty(DataOffset)",
            })?;
        let byte = u64::try_from(offset).map_err(DecodeError::backend)?;
        handle
            .callbacks()
            .set_packet_window(pools.get_with_len::<u8>(window_size)?);
        Ok(FlacFrontier { packet: 0, byte })
    }

    /// Open a streamed source without an eager packet-count scan; headers supply its metadata.
    /// The true total size enables pending reads and estimated seeks; no length uses `SizeMode::Unknown`.
    /// Never report a partial in-flight length: `AudioFile` would freeze that prefix as false EOF.
    pub(crate) fn open_sized_streaming<S>(
        source: BoxedSource,
        hint: Option<u32>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let (mut source, end) = Self::probe_end(source)?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(DecodeError::backend)?;
        let size = match end {
            Some(end) => SizeMode::Snapshot(i64::try_from(end).map_err(DecodeError::backend)?),
            None => SizeMode::Unknown,
        };
        Self::open_inner(source, hint, size, false, pools)
    }

    /// Open a segmented container within its header-declared extent, without
    /// resolving segment sizes or scanning its packets before playback.
    pub(crate) fn open_segmented<S>(
        source: BoxedSource,
        hint: Option<u32>,
        end: u64,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let end = i64::try_from(end).map_err(DecodeError::backend)?;
        Self::open_inner(source, hint, SizeMode::Snapshot(end), false, pools)
    }

    /// The source byte offset `AudioFileServices` maps `packet` to — the same
    /// offset its own packet read seeks to, so a seek can report it as
    /// `landed_byte` and keep the stream's byte cursor consistent with where
    /// the decoder resumes.
    ///
    /// `None` when `AudioFile` cannot map the packet. Measured: a size-less
    /// open ([`SizeMode::Unknown`]) rejects every packet with
    /// `kAudioFileInvalidPacketOffsetError` (`'pck?'`) — with no total it
    /// cannot bound the packet, let alone average one. Only an open with a
    /// known size answers; the size-less path needs its own estimate.
    pub(crate) fn packet_to_byte(&self, packet: u64) -> Option<u64> {
        let query = AudioBytePacketTranslation {
            packet: SInt64::try_from(packet).ok()?,
            ..AudioBytePacketTranslation::default()
        };
        let translated = self
            .handle
            .get_property_with_input(AUDIO_FILE_PROPERTY_PACKET_TO_BYTE, query)
            .ok()?;
        let data_offset: SInt64 = self
            .handle
            .get_property(AUDIO_FILE_PROPERTY_DATA_OFFSET)
            .ok()?;
        u64::try_from(data_offset)
            .ok()?
            .checked_add(u64::try_from(translated.byte).ok()?)
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

    /// Read one VBR packet at `starting_packet` into `buf`. Returns
    /// `Ok(Some((bytes_written, packet_desc)))` or `Ok(None)` at EOF.
    /// Use for codecs whose decoder needs per-packet descriptors
    /// (ALAC, FLAC, AAC).
    ///
    /// A not-ready streamed read is masked by `AudioFile` as either a graceful EOF or a truncated
    /// packet with a stashed callback error, for compressed formats.
    pub(crate) fn read_packet(
        &mut self,
        starting_packet: u64,
        buf: &mut [u8],
    ) -> DecodeResult<Option<(u32, AudioStreamPacketDescription)>> {
        while let Some(frontier) = self.flac_frontier
            && frontier.packet <= starting_packet
        {
            self.handle.callbacks().stage_packet(frontier.byte)?;
            let read = self.read_packet_data(frontier.packet, buf);
            let advanced = match &read {
                Ok(Some((bytes, _))) => frontier.advance(*bytes).map(Some),
                _ => Ok(None),
            };
            let finished = self.handle.callbacks().finish_packet();
            let read = read?;
            if let Some(frontier) = advanced? {
                self.flac_frontier = Some(frontier);
            }
            finished?;
            if read.is_none() || frontier.packet == starting_packet {
                return Ok(read);
            }
        }
        self.read_packet_data(starting_packet, buf)
    }

    fn read_packet_data(
        &mut self,
        starting_packet: u64,
        buf: &mut [u8],
    ) -> DecodeResult<Option<(u32, AudioStreamPacketDescription)>> {
        let mut packets: UInt32 = 1;
        let mut desc = AudioStreamPacketDescription::default();
        let starting_packet = SInt64::try_from(starting_packet).map_err(DecodeError::backend)?;
        let callbacks = self.handle.callbacks();
        callbacks.last_error.set(None);
        callbacks.terminal_short_read.set(None);
        let read =
            self.handle
                .read_packet_data(starting_packet, Some(&mut desc), &mut packets, buf);

        let AudioFilePacketRead { bytes, packets } = self
            .handle
            .callbacks()
            .finish_read(read, "AudioFileReadPacketData")?;
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
        let callbacks = self.handle.callbacks();
        callbacks.last_error.set(None);
        callbacks.terminal_short_read.set(None);
        let read = self
            .handle
            .read_packet_data(starting_packet, None, &mut packets, buf);
        let read = self
            .handle
            .callbacks()
            .finish_read(read, "AudioFileReadPacketData(cbr)")?;
        Ok((read.bytes, read.packets))
    }

    pub(crate) fn set_byte_len_handle(&self, handle: Option<Arc<AtomicU64>>) {
        let Some(handle) = handle else {
            return;
        };
        let byte_len = &self.handle.callbacks().byte_len;
        if let Some(current) = byte_len.get() {
            debug_assert!(Arc::ptr_eq(current, &handle));
        } else {
            let installed = byte_len.set(handle).is_ok();
            debug_assert!(installed, "byte-length handle is installed once");
        }
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
