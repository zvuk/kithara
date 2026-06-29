#![allow(unsafe_code)]

use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ContainerFormat, PrerollHint};
use num_traits::ToPrimitive;

use super::{
    audio_file::AppleAudioFile,
    consts::Consts,
    ffi::{AudioStreamBasicDescription, AudioStreamPacketDescription},
    flac::StreamInfo,
};
use crate::{
    GaplessInfo,
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::{DecodeError, DecodeResult},
    pcm_time::{duration_for_frames, frames_for_duration},
    traits::BoxedSource,
};

fn sample_rate_from_asbd(rate: f64) -> Option<u32> {
    if !rate.is_finite() || rate <= 0.0 || rate > f64::from(u32::MAX) {
        return None;
    }
    rate.to_u32()
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
    total_packets: Option<u64>,
    track_info: TrackInfo,
    read_buf: Vec<u8>,
    last_packet_desc_blob: [u8; size_of::<AudioStreamPacketDescription>()],
    frames_per_packet: u32,
    next_packet: u64,
    last_read_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceOpenMode {
    Complete,
    Streaming,
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
            (AudioCodec::Flac, ContainerFormat::Flac) => Consts::kAudioFileFLACType,
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

    fn open(
        source: BoxedSource,
        hint: Option<u32>,
        codec: AudioCodec,
        open_mode: SourceOpenMode,
        duration_hint: Option<Duration>,
    ) -> DecodeResult<Self> {
        // MP3 and FLAC are VBR with no on-disk packet index, so a complete
        // open would query `packet_count()` — forcing `AudioFileServices` to
        // scan the WHOLE file to build a packet table before the first frame
        // (3–37 s on device for large lossless tracks, and a full download
        // wait on a streamed source). The streaming opens skip that scan;
        // duration and the read-buffer size come from cheap header metadata
        // instead (Xing for MP3, STREAMINFO for FLAC).
        //
        // FLAC additionally needs the real file size handed to AudioFile
        // (`open_sized_streaming`): without it a not-ready read past the
        // download boundary is mistaken for EOF (track ends mid-stream) and
        // seeks degrade to an O(N) forward frame-scan. MP3 stays size-less —
        // it must not probe tail bytes at open (`open_mp3_demuxer_*`).
        let file = match (open_mode, codec) {
            (SourceOpenMode::Streaming, AudioCodec::Flac) => {
                AppleAudioFile::open_sized_streaming(source, hint)?
            }
            (SourceOpenMode::Streaming, AudioCodec::Mp3) => {
                AppleAudioFile::open_streaming(source, hint, None)?
            }
            _ => AppleAudioFile::open(source, hint)?,
        };
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

        // FLAC's magic cookie carries STREAMINFO: `total_samples` yields the
        // exact duration and `max_frame_size` bounds the VBR read buffer when
        // the streaming open leaves `max_packet_size()` at 0.
        let flac_info = (codec == AudioCodec::Flac)
            .then(|| StreamInfo::parse(&extra_data).ok())
            .flatten();

        let channels = u16::try_from(asbd.mChannelsPerFrame).map_err(|_| {
            DecodeError::backend_msg(format!(
                "apple.audio_file: invalid channel count {}",
                asbd.mChannelsPerFrame
            ))
        })?;
        if channels == 0 {
            return Err(DecodeError::backend_msg(
                "apple.audio_file: invalid zero channel count",
            ));
        }
        let Some(sample_rate) = sample_rate_from_asbd(asbd.mSampleRate) else {
            return Err(DecodeError::InvalidSampleRate {
                resource: "apple.audio_file",
            });
        };
        let flac_duration = flac_info
            .filter(|info| info.total_samples > 0)
            .map(|info| duration_for_frames(sample_rate, info.total_samples));
        let duration = total_packets
            .filter(|count| *count > 0)
            .map(|total_packets| {
                let frames = total_packets.saturating_mul(u64::from(frames_per_packet));
                duration_for_frames(sample_rate, frames)
            })
            .or(flac_duration)
            .or(duration_hint);

        let track_info = TrackInfo {
            codec,
            duration,
            extra_data,
            channels,
            sample_rate,
            gapless: None,
        };

        let (cbr_batch_packets, buf_cap) = if asbd.mBytesPerPacket == 0 {
            // VBR. A streaming open reports no max packet size, so fall back
            // to the FLAC STREAMINFO frame bound (FLAC frames reach ~16-19
            // KiB, far past the 4 KiB floor) when AudioFile can't supply one.
            let reported = usize::try_from(file.max_packet_size()).map_err(DecodeError::backend)?;
            let flac_bound = flac_info.map_or(0, StreamInfo::max_frame_bytes);
            (None, reported.max(flac_bound).max(4096))
        } else {
            let packets = Self::CBR_BATCH_TARGET_BYTES
                .checked_div(asbd.mBytesPerPacket)
                .map_or(1, |packets| packets.max(1));
            let bytes = packets.saturating_mul(asbd.mBytesPerPacket);
            (
                Some(packets),
                usize::try_from(bytes).map_err(DecodeError::backend)?,
            )
        };

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
    pub(crate) fn open_for_with_mode(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        open_mode: SourceOpenMode,
        duration_hint: Option<Duration>,
    ) -> DecodeResult<Self> {
        let hint = container
            .and_then(|c| Self::file_type_id(codec, c))
            .ok_or(DecodeError::UnsupportedCodec(codec))?;
        Self::open(source, Some(hint), codec, open_mode, duration_hint)
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
    /// [`Self::open_for_with_mode`]; mirrors [`Self::file_type_id`].
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
        if self
            .total_packets
            .is_some_and(|total_packets| self.next_packet >= total_packets)
        {
            return Ok(DemuxOutcome::Eof);
        }

        let sample_rate = self.track_info.sample_rate;
        let start_packet = self.next_packet;
        let frame_idx = start_packet.saturating_mul(u64::from(self.frames_per_packet));
        let pts = duration_for_frames(sample_rate, frame_idx);

        if let Some(batch_packets) = self.cbr_batch_packets {
            let want = if let Some(total_packets) = self.total_packets {
                let remaining = total_packets.saturating_sub(start_packet);
                if remaining >= u64::from(batch_packets) {
                    batch_packets
                } else {
                    u32::try_from(remaining).map_err(DecodeError::backend)?
                }
            } else {
                batch_packets
            };
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
            self.last_read_len = usize::try_from(bytes).map_err(DecodeError::backend)?;
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

        self.last_read_len = usize::try_from(bytes).map_err(DecodeError::backend)?;
        // SAFETY: `AudioStreamPacketDescription` is `#[repr(C)]` POD
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(&desc).cast::<u8>(),
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
        if let Some(total_packets) = self.total_packets {
            let total_frames = total_packets.saturating_mul(u64::from(self.frames_per_packet));
            let total_duration = duration_for_frames(sample_rate, total_frames);
            if target >= total_duration {
                return Ok(DemuxSeekOutcome::PastEof {
                    duration: total_duration,
                });
            }
        }

        if self.total_packets == Some(0) {
            return Ok(DemuxSeekOutcome::PastEof {
                duration: Duration::ZERO,
            });
        }

        let target_frame = u64::try_from(frames_for_duration(sample_rate, target))
            .map_err(DecodeError::backend)?;
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
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(asbd).cast::<u8>(),
            out.as_mut_ptr(),
            out.len(),
        );
    }
    out
}

#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod tests {
    use std::{
        io::{self, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use kithara_stream::{
        AudioCodec, ContainerFormat, NotReadyCause, PendingReason, SourcePhase, StreamPending,
    };
    use kithara_test_utils::kithara;

    use super::{AppleAudioFileDemuxer, SourceOpenMode};
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
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
            None,
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

    /// `supports` is the exact predicate the decoder factory gates
    /// standalone `AudioFileServices` dispatch on (`apple_standalone_supports`).
    /// Native FLAC (`fLaC`, `audio/flac`) is the regression contract: the iOS
    /// build ships no Symphonia fallback, so a `false` here is precisely the
    /// `Unsupported codec: Flac` the device hit on every `streamfl` track.
    /// fMP4-FLAC and container-less FLAC must stay `false` — those route
    /// through the segment-aware path, not this standalone one.
    #[kithara::test]
    #[case(AudioCodec::Pcm, Some(ContainerFormat::Wav), true)]
    #[case(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio), true)]
    #[case(AudioCodec::Flac, Some(ContainerFormat::Flac), true)]
    #[case(AudioCodec::Alac, Some(ContainerFormat::Mp4), true)]
    #[case(AudioCodec::AacLc, Some(ContainerFormat::Mp4), true)]
    #[case(AudioCodec::Flac, Some(ContainerFormat::Fmp4), false)]
    #[case(AudioCodec::Flac, None, false)]
    fn supports_covers_standalone_dispatch_matrix(
        #[case] codec: AudioCodec,
        #[case] container: Option<ContainerFormat>,
        #[case] expected: bool,
    ) {
        assert_eq!(
            AppleAudioFileDemuxer::supports(codec, container),
            expected,
            "supports({codec:?}, {container:?})"
        );
    }

    /// Streamed source: bytes past `ready` are not delivered yet. Mirrors
    /// `Stream::probe_read` — a read at/past the boundary fails with an
    /// `Interrupted` `io::Error` carrying a typed [`StreamPending`] payload.
    struct NotReadySource {
        inner: Cursor<Vec<u8>>,
        notify_not_ready: Option<Arc<AtomicBool>>,
        ready: u64,
    }

    impl NotReadySource {
        fn new(bytes: Vec<u8>, ready: u64, notify_not_ready: Option<Arc<AtomicBool>>) -> Self {
            let inner = Cursor::new(bytes);
            Self {
                inner,
                notify_not_ready,
                ready,
            }
        }
    }

    impl Read for NotReadySource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.inner.position();
            let want = u64::try_from(buf.len())
                .map_err(|_| Error::other("NotReadySource request exceeds u64"))?;
            // `probe_read` waits on the WHOLE requested range — a request
            // crossing the delivery boundary fails outright, it is never
            if pos.saturating_add(want) > self.ready {
                if let Some(notify_not_ready) = &self.notify_not_ready {
                    notify_not_ready.store(true, Ordering::Release);
                }
                return Err(not_ready_error(
                    pos,
                    buf.len(),
                    u64::try_from(self.inner.get_ref().len()).ok(),
                ));
            }
            self.inner.read(buf)
        }
    }

    impl Seek for NotReadySource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    fn not_ready_error(pos: u64, want: usize, len: Option<u64>) -> Error {
        Error::new(
            ErrorKind::Interrupted,
            StreamPending {
                pos,
                want,
                len,
                reason: PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
                phase: SourcePhase::Waiting,
                epoch: 0,
                flushing: false,
                variant_fence: false,
            },
        )
    }

    /// Contract pin (#110): "data not ready" from the source must surface
    /// as `DemuxOutcome::Pending`, never as `Err`. An `Err` here is
    /// classified `Interrupted` by the decode loop, which retries
    /// immediately — a hot spin that burns a core for as long as the
    /// stall lasts instead of parking the worker.
    #[kithara::test]
    fn next_frame_surfaces_not_ready_as_pending_not_err() {
        let bytes = read_asset("silence_1s.wav");
        let ready = u64::try_from(bytes.len() / 2).expect("fixture length fits in u64");
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(bytes, ready, None)),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
            None,
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

    #[kithara::test]
    fn open_mp3_demuxer_does_not_require_tail_bytes() {
        let bytes = read_asset("test.mp3");
        let ready = 16_u64 * 1024;
        let tail_read_attempted = Arc::new(AtomicBool::new(false));
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(
                bytes,
                ready,
                Some(Arc::clone(&tail_read_attempted)),
            )),
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
            SourceOpenMode::Streaming,
            Some(kithara_platform::time::Duration::from_secs(2)),
        )
        .expect("MP3 streaming open must not require tail bytes");
        assert!(
            !tail_read_attempted.load(Ordering::Acquire),
            "MP3 streaming open must not probe bytes beyond the startup prefix"
        );
        assert_eq!(
            dx.duration(),
            Some(kithara_platform::time::Duration::from_secs(2)),
            "MP3 streaming open must use the caller's prefix duration hint"
        );

        match dx
            .next_frame()
            .expect("first MP3 frame read returns a status")
        {
            DemuxOutcome::Frame(frame) => assert!(!frame.data.is_empty()),
            DemuxOutcome::Pending(PendingReason::NotReady(_)) => {}
            other => panic!("unexpected first MP3 outcome: {other:?}"),
        }
    }

    /// Regression (#device-flac-slow-load): a streamed FLAC must open
    /// without `AudioFileServices` scanning the whole file to build a packet
    /// table (the `packet_count()` query a complete open issues). The scan
    /// reads to EOF — 3–37 s of startup latency on device and a full
    /// download wait on a streamed source. Mirrors the MP3 contract above.
    #[kithara::test]
    fn open_flac_demuxer_does_not_require_tail_bytes() {
        let bytes = read_asset("sawtooth.flac");
        // A bounded streaming open reads the header + first frame (~27 KiB)
        // regardless of file size; this prefix covers that. The fixture is
        // ~204 KiB, so a full-file packet-table scan would cross this bound.
        let ready = 64_u64 * 1024;
        let tail_read_attempted = Arc::new(AtomicBool::new(false));
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(
                bytes,
                ready,
                Some(Arc::clone(&tail_read_attempted)),
            )),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
            None,
        )
        .expect("FLAC streaming open must not require tail bytes");
        assert!(
            !tail_read_attempted.load(Ordering::Acquire),
            "FLAC streaming open must not scan past the startup prefix"
        );

        match dx
            .next_frame()
            .expect("first FLAC frame read returns a status")
        {
            DemuxOutcome::Frame(frame) => assert!(!frame.data.is_empty()),
            DemuxOutcome::Pending(PendingReason::NotReady(_)) => {}
            other => panic!("unexpected first FLAC outcome: {other:?}"),
        }
    }

    #[kithara::test]
    fn open_mp3_demuxer_complete_source_reports_duration() {
        let bytes = read_asset("test.mp3");
        let dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
            SourceOpenMode::Complete,
            None,
        )
        .expect("MP3 complete open must succeed");

        let duration = dx
            .duration()
            .expect("complete MP3 open must report packet-count duration");
        assert!(
            duration.as_secs_f64() > 1.0,
            "complete MP3 duration is suspiciously short: {duration:?}"
        );
    }

    /// Streaming FLAC regression (#device-flac-stall): when a read crosses
    /// the not-yet-downloaded boundary, the demuxer must surface `Pending`
    /// so the worker parks and wakes on more data. `AudioFileServices` masks
    /// the read-callback failure as a graceful EOF (noErr, 0 packets) for
    /// FLAC; before the fix that ended the track mid-stream — on device the
    /// track played a fraction of a second then stalled, advancing only by
    /// another fraction on a manual play kick (and seeks skipped to the next
    /// track). The fix needs both the sized streaming open (`AudioFile` knows
    /// more data exists) and `read_packet` consulting the stashed error.
    #[kithara::test]
    fn flac_streaming_not_ready_surfaces_pending_not_eof() {
        let bytes = read_asset("sawtooth.flac");
        // Header + several frames are ready; the rest is "not downloaded".
        let ready = 64_u64 * 1024;
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(bytes, ready, None)),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
            None,
        )
        .expect("streaming FLAC open");

        let mut produced = 0usize;
        loop {
            match dx.next_frame() {
                Ok(DemuxOutcome::Frame(_)) => {
                    produced += 1;
                    assert!(
                        produced < 5000,
                        "drained the whole fixture without reaching the not-ready boundary"
                    );
                }
                Ok(DemuxOutcome::Pending(PendingReason::NotReady(_))) => break,
                Ok(DemuxOutcome::Eof) => panic!(
                    "not-ready boundary surfaced as EOF after {produced} frames — \
                     the track would end mid-stream"
                ),
                other => panic!("unexpected outcome at the not-ready boundary: {other:?}"),
            }
        }
        assert!(
            produced > 0,
            "should decode the ready prefix before parking"
        );
    }
}
