#![allow(unsafe_code)]

use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ContainerFormat, PrerollHint};

use super::{
    audio_file::AppleAudioFile,
    consts::Consts,
    ffi::{AudioStreamBasicDescription, AudioStreamPacketDescription},
};
use crate::{
    GaplessInfo,
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::{DecodeError, DecodeResult},
    pcm_time::{duration_for_frames, frames_for_duration},
    traits::BoxedSource,
};

/// Clamp a `kAudioFilePropertyDataFormat`'s `mSampleRate` (f64) into a
/// non-negative `u32`. Container metadata always carries a positive,
/// integer-valued rate (44100 / 48000 / 96000); the conversion is
/// lossless in practice but a pathological negative / `NaN` / overflow
/// value clamps to 0 / `u32::MAX` (downstream code already treats 0
/// as "unknown rate").
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "explicit guards clamp NaN/negative/overflow; final `as u32` is lossless for 0..u32::MAX"
)]
fn sample_rate_from_asbd(rate: f64) -> u32 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    if rate >= f64::from(u32::MAX) {
        return u32::MAX;
    }
    rate as u32
}

/// [`Demuxer`] over [`AppleAudioFile`] for standalone (non-fMP4)
/// container formats. Currently wires WAV/PCM, MP3, ALAC-in-M4A and
/// ALAC-in-CAF; extends via additional file-type hints.
///
/// The [`AppleAudioFile`] packet descriptor (a `#[repr(C)]` POD) is
/// serialized into `last_packet_desc_blob` and exposed to the codec
/// layer through `Frame::packet_desc`. CBR codecs ignore it; VBR
/// codecs (MP3, ALAC) reinterpret the bytes back into
/// [`AudioStreamPacketDescription`].
pub(crate) struct AppleAudioFileDemuxer {
    file: AppleAudioFile,
    /// `Some(packets_per_call)` for CBR (`LinearPCM`) — every `next_frame`
    /// issues one batched `AudioFileReadPacketData` for that many
    /// packets. `None` for VBR (MP3, ALAC) — one packet per call so
    /// each `Frame` carries its own `AudioStreamPacketDescription`.
    cbr_batch_packets: Option<u32>,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_packet_desc_blob: [u8; size_of::<AudioStreamPacketDescription>()],
    frames_per_packet: u32,
    next_packet: u64,
    total_packets: u64,
    last_read_len: usize,
}

impl AppleAudioFileDemuxer {
    /// Target ~16 `KiB` per CBR read — large enough to amortise the
    /// source `wait_range` cost on streamed sources (HLS), small enough
    /// to keep the in-flight buffer bounded.
    const CBR_BATCH_TARGET_BYTES: u32 = 16 * 1024;

    /// Single source of truth: maps `(codec, container)` to the
    /// `kAudioFileXxxType` four-cc hint `AudioFileServices` needs.
    /// Returns `None` when Apple's standalone file path can't handle the
    /// combination — the factory consults this through [`Self::supports`]
    /// before dispatching, so any new (codec, container) only needs one
    /// match arm here.
    fn file_type_id(codec: AudioCodec, container: ContainerFormat) -> Option<u32> {
        Some(match (codec, container) {
            (AudioCodec::Pcm, ContainerFormat::Wav) => Consts::kAudioFileWAVEType,
            (AudioCodec::Mp3, ContainerFormat::MpegAudio) => Consts::kAudioFileMP3Type,
            (AudioCodec::Alac, ContainerFormat::Mp4) => Consts::kAudioFileM4AType,
            (AudioCodec::Alac, ContainerFormat::Caf) => Consts::kAudioFileCAFType,
            (AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2, ContainerFormat::Mp4) => {
                Consts::kAudioFileM4AType
            }
            (
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
                ContainerFormat::Adts,
            ) => Consts::kAudioFileAAC_ADTSType,
            _ => return None,
        })
    }

    fn open(source: BoxedSource, hint: Option<u32>, codec: AudioCodec) -> DecodeResult<Self> {
        let file = AppleAudioFile::open(source, hint)?;
        let asbd = file.data_format();
        let total_packets = file.packet_count();
        let frames_per_packet = if asbd.mFramesPerPacket > 0 {
            asbd.mFramesPerPacket
        } else {
            4096
        };

        let extra_data = match codec {
            AudioCodec::Pcm => serialize_asbd(&asbd),
            _ => file.magic_cookie().unwrap_or_default(),
        };

        let channels = u16::try_from(asbd.mChannelsPerFrame).unwrap_or(2);
        let sample_rate = sample_rate_from_asbd(asbd.mSampleRate);
        if sample_rate == 0 {
            return Err(DecodeError::InvalidSampleRate {
                resource: "apple.audio_file",
            });
        }
        let duration = if total_packets > 0 {
            let frames = total_packets.saturating_mul(u64::from(frames_per_packet));
            Some(duration_for_frames(sample_rate, frames))
        } else {
            None
        };

        let track_info = TrackInfo {
            codec,
            duration,
            extra_data,
            channels,
            sample_rate,
            gapless: None,
        };

        let (cbr_batch_packets, buf_cap) = Self::CBR_BATCH_TARGET_BYTES
            .checked_div(asbd.mBytesPerPacket)
            .map_or_else(
                || {
                    (
                        None,
                        usize::try_from(file.max_packet_size().max(4096)).unwrap_or(64 * 1024),
                    )
                },
                |packets| {
                    let packets = packets.max(1);
                    let bytes = packets.saturating_mul(asbd.mBytesPerPacket);
                    (
                        Some(packets),
                        usize::try_from(bytes).unwrap_or(Self::CBR_BATCH_TARGET_BYTES as usize),
                    )
                },
            );

        Ok(Self {
            file,
            track_info,
            total_packets,
            frames_per_packet,
            cbr_batch_packets,
            read_buf: vec![0u8; buf_cap],
            last_read_len: 0,
            last_packet_desc_blob: [0u8; size_of::<AudioStreamPacketDescription>()],
            next_packet: 0,
        })
    }

    /// Open a track for the given `(codec, container)` pair, picking the
    /// `AudioFileServices` file-type hint internally. The caller is
    /// expected to have checked [`Self::supports`] (the factory does);
    /// unsupported combinations return [`DecodeError::UnsupportedCodec`].
    pub(crate) fn open_for(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> DecodeResult<Self> {
        let hint = container
            .and_then(|c| Self::file_type_id(codec, c))
            .ok_or(DecodeError::UnsupportedCodec(codec))?;
        Self::open(source, Some(hint), codec)
    }

    /// Inject encoder priming/padding metadata probed by the factory
    /// layer (e.g. Xing/Info+LAME for MP3, `iTunSMPB`/`elst` for AAC).
    /// `AudioFileServices` does not expose Xing/LAME or MP4 edit lists,
    /// so the factory probes the source separately and pipes the
    /// captured trim counts through here.
    pub(crate) fn set_gapless(&mut self, gapless: Option<GaplessInfo>) {
        self.track_info.gapless = gapless;
    }

    /// Whether Apple's standalone file path supports this `(codec,
    /// container)` pair. Used by the factory to gate dispatch into
    /// [`Self::open_for`]; mirrors [`Self::file_type_id`].
    #[must_use]
    pub(crate) fn supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
        container.is_some_and(|c| Self::file_type_id(codec, c).is_some())
    }
}

impl Demuxer for AppleAudioFileDemuxer {
    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if self.next_packet >= self.total_packets {
            return Ok(DemuxOutcome::Eof);
        }

        let sample_rate = self.track_info.sample_rate;
        let start_packet = self.next_packet;
        let frame_idx = start_packet.saturating_mul(u64::from(self.frames_per_packet));
        let pts = duration_for_frames(sample_rate, frame_idx);

        if let Some(batch_packets) = self.cbr_batch_packets {
            let remaining =
                u32::try_from(self.total_packets.saturating_sub(start_packet)).unwrap_or(u32::MAX);
            let want = batch_packets.min(remaining);
            // Contract: "data not ready" surfaces as `Pending`, never `Err` —
            // an `Err` classifies as `Interrupted` upstream and the decode
            // loop retries it hot instead of parking the worker. The packet
            // cursor was not advanced, so the next call re-reads the same
            let (bytes, packets_read) =
                match self
                    .file
                    .read_packets_cbr(start_packet, want, &mut self.read_buf)
                {
                    Ok(read) => read,
                    Err(e) => {
                        if let Some(reason) = e.pending_reason() {
                            return Ok(DemuxOutcome::Pending(reason));
                        }
                        return Err(e);
                    }
                };
            if packets_read == 0 {
                return Ok(DemuxOutcome::Eof);
            }
            self.last_read_len = bytes as usize;
            let total_frames =
                u64::from(packets_read).saturating_mul(u64::from(self.frames_per_packet));
            let dur = duration_for_frames(sample_rate, total_frames);
            self.next_packet = start_packet.saturating_add(u64::from(packets_read));
            return Ok(DemuxOutcome::Frame(Frame {
                pts,
                data: &self.read_buf[..self.last_read_len],
                duration: dur,
                packet_desc: &[],
            }));
        }

        let read = match self.file.read_packet(start_packet, &mut self.read_buf) {
            Ok(read) => read,
            Err(e) => {
                if let Some(reason) = e.pending_reason() {
                    return Ok(DemuxOutcome::Pending(reason));
                }
                return Err(e);
            }
        };
        let Some((bytes, desc)) = read else {
            return Ok(DemuxOutcome::Eof);
        };

        self.last_read_len = bytes as usize;
        // SAFETY: `AudioStreamPacketDescription` is `#[repr(C)]` POD
        unsafe {
            std::ptr::copy_nonoverlapping(
                &desc as *const _ as *const u8,
                self.last_packet_desc_blob.as_mut_ptr(),
                size_of::<AudioStreamPacketDescription>(),
            );
        }

        let frames = if desc.mVariableFramesInPacket > 0 {
            u64::from(desc.mVariableFramesInPacket)
        } else {
            u64::from(self.frames_per_packet)
        };
        let dur = duration_for_frames(sample_rate, frames);

        let frame = Frame {
            pts,
            data: &self.read_buf[..self.last_read_len],
            duration: dur,
            packet_desc: &self.last_packet_desc_blob,
        };

        self.next_packet = start_packet.saturating_add(1);
        Ok(DemuxOutcome::Frame(frame))
    }

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        let sample_rate = self.track_info.sample_rate;
        let total_frames = self
            .total_packets
            .saturating_mul(u64::from(self.frames_per_packet));
        let total_duration = duration_for_frames(sample_rate, total_frames);

        if target >= total_duration {
            return Ok(DemuxSeekOutcome::PastEof {
                duration: total_duration,
            });
        }

        let target_frame = frames_for_duration(sample_rate, target) as u64;
        let target_packet = target_frame / u64::from(self.frames_per_packet.max(1));
        let backup = u64::from(priming.packets).min(target_packet);
        let landed_packet = target_packet.saturating_sub(backup);
        self.next_packet = landed_packet;

        let landed_frame = landed_packet.saturating_mul(u64::from(self.frames_per_packet));
        let landed_at = duration_for_frames(sample_rate, landed_frame);

        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

fn serialize_asbd(asbd: &AudioStreamBasicDescription) -> Vec<u8> {
    let mut out = vec![0u8; size_of::<AudioStreamBasicDescription>()];
    // SAFETY: `AudioStreamBasicDescription` is `#[repr(C)]` POD; we copy
    unsafe {
        std::ptr::copy_nonoverlapping(asbd as *const _ as *const u8, out.as_mut_ptr(), out.len());
    }
    out
}

#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod tests {
    use std::io::{self, Cursor, Error, ErrorKind, Read, Seek, SeekFrom};

    use kithara_stream::{
        AudioCodec, ContainerFormat, NotReadyCause, PendingReason, SourcePhase, StreamPending,
    };
    use kithara_test_utils::kithara;

    use super::AppleAudioFileDemuxer;
    use crate::demuxer::{DemuxOutcome, Demuxer};

    fn read_asset(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("assets")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    #[kithara::test]
    fn open_wav_demuxer_track_info_and_first_frame() {
        let bytes = read_asset("silence_1s.wav");
        let mut dx = AppleAudioFileDemuxer::open_for(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
        )
        .expect("open_for(Pcm, Wav) must succeed");

        let info = dx.track_info();
        assert_eq!(info.codec, AudioCodec::Pcm);
        assert!(info.channels >= 1);
        assert!(info.sample_rate >= 8000);
        assert!(info.duration.is_some());

        match dx.next_frame().expect("next_frame ok") {
            DemuxOutcome::Frame(frame) => assert!(!frame.data.is_empty()),
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    /// Streamed source: bytes past `ready` are not delivered yet. Mirrors
    /// `Stream::probe_read` — a read at/past the boundary fails with an
    /// `Interrupted` `io::Error` carrying a typed [`StreamPending`] payload.
    struct NotReadyAfter {
        inner: Cursor<Vec<u8>>,
        ready: u64,
    }

    impl Read for NotReadyAfter {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.inner.position();
            let total = self.inner.get_ref().len() as u64;
            // `probe_read` waits on the WHOLE requested range — a request
            // crossing the delivery boundary fails outright, it is never
            if pos.saturating_add(buf.len() as u64) > self.ready {
                return Err(Error::new(
                    ErrorKind::Interrupted,
                    StreamPending {
                        pos,
                        reason: PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
                        want: buf.len(),
                        len: Some(total),
                        phase: SourcePhase::Waiting,
                        epoch: 0,
                        flushing: false,
                        variant_fence: false,
                    },
                ));
            }
            self.inner.read(buf)
        }
    }

    impl Seek for NotReadyAfter {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// Contract pin (#110): "data not ready" from the source must surface
    /// as `DemuxOutcome::Pending`, never as `Err`. An `Err` here is
    /// classified `Interrupted` by the decode loop, which retries
    /// immediately — a hot spin that burns a core for as long as the
    /// stall lasts instead of parking the worker.
    #[kithara::test]
    fn next_frame_surfaces_not_ready_as_pending_not_err() {
        let bytes = read_asset("silence_1s.wav");
        let ready = (bytes.len() / 2) as u64;
        let mut dx = AppleAudioFileDemuxer::open_for(
            Box::new(NotReadyAfter {
                ready,
                inner: Cursor::new(bytes),
            }),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
        )
        .expect("open_for must succeed with the header prefix available");

        loop {
            match dx.next_frame() {
                Ok(DemuxOutcome::Frame(_)) => {}
                Ok(DemuxOutcome::Pending(PendingReason::NotReady(_))) => return,
                Ok(other) => panic!("unexpected outcome before the not-ready boundary: {other:?}"),
                Err(e) => panic!("data-not-ready must surface as Pending, got Err: {e}"),
            }
        }
    }
}
