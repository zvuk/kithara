//! Packet-reader abstraction for the Apple decoder.
//!
//! Two implementations live next to this file:
//! - `audiofile` — wraps `AudioFileOpenWithCallbacks` for atom-aware
//!   containers with real `stsz/stco` tables (MP3, FLAC, ADTS, CAF, WAV,
//!   non-fragmented MP4).
//! - `fmp4` — parses fragmented MP4 via `re_mp4` (HLS init+segments).
//!   `AudioFile` can't read packets out of fMP4 because the `moov` sample
//!   tables are empty — per-fragment descriptors live in `moof/traf/trun`.
//!
//! Both implementations expose packet data through a borrowed `PacketRef`
//! that lives until the next `read_next_packet` call, so the decoder's
//! hot loop avoids per-packet allocations.

#![allow(unsafe_code)]

use std::time::Duration;

use kithara_stream::ContainerFormat;

use super::{
    consts::Consts,
    ffi::{AudioFileTypeID, AudioStreamBasicDescription, AudioStreamPacketDescription},
};
use crate::error::DecodeResult;

/// Container format → `AudioFile` type hint.
pub(super) fn container_to_file_type(container: ContainerFormat) -> Option<AudioFileTypeID> {
    match container {
        // NOTE: Fmp4 is handled by Fmp4Reader; AudioFile cannot read
        // packets out of a fragmented container. Plain MP4 still goes
        // through AudioFile because it has real sample tables.
        ContainerFormat::Mp4 => Some(Consts::kAudioFileMPEG4Type),
        ContainerFormat::Adts => Some(Consts::kAudioFileAAC_ADTSType),
        ContainerFormat::MpegAudio => Some(Consts::kAudioFileMP3Type),
        ContainerFormat::Flac => Some(Consts::kAudioFileFLACType),
        ContainerFormat::Caf => Some(Consts::kAudioFileCAFType),
        ContainerFormat::Wav => Some(Consts::kAudioFileWAVEType),
        _ => None,
    }
}

/// A compressed audio packet borrowed from the reader's internal buffer.
/// The slice is valid until the next `read_next_packet` call.
pub(super) struct PacketRef<'a> {
    pub(super) data: &'a [u8],
    pub(super) description: AudioStreamPacketDescription,
    /// Absolute byte offset of this packet inside the source container,
    /// when known. `AudioFile` exposes it via
    /// `kAudioFilePropertyPacketToByte`; fragmented MP4 has no useful
    /// container-wide offset and reports `None`.
    pub(super) byte_offset: Option<u64>,
}

/// Minimal interface the inner decoder needs from a container parser.
pub(super) trait PacketReader: Send {
    fn format(&self) -> AudioStreamBasicDescription;
    fn magic_cookie(&self) -> Option<&[u8]>;
    fn duration(&self) -> Option<Duration>;
    fn read_next_packet(&mut self) -> DecodeResult<Option<PacketRef<'_>>>;
    /// Seek to the packet containing `target_frame`. Returns the first
    /// frame that will be emitted after the seek (may be less than
    /// `target_frame` when the seek lands on a packet boundary).
    fn seek_to_frame(&mut self, target_frame: u64) -> DecodeResult<u64>;
    /// Absolute byte offset of the *next* packet body inside the
    /// source container after a successful seek. `AudioFile` resolves
    /// it via `kAudioFilePropertyPacketToByte`; `Fmp4Reader` exposes
    /// the shared `MediaSource` read cursor updated by
    /// `IsoMp4Reader::seek`. `None` means the container genuinely
    /// has no resolvable byte offset for the next packet — callers
    /// must surface a typed `SeekFailed` rather than substitute a
    /// derived guess.
    fn landed_byte(&self) -> Option<u64>;
}
