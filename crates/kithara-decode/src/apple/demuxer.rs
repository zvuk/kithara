use std::{
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_apple::audio_toolbox::{AudioStreamPacketDescription, pod_to_vec, pod_write_to_slice};
use kithara_bufpool::{ByteBuffer, HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, FrameCount};
use kithara_stream::{AudioCodec, ContainerFormat, PendingReason, PrerollHint};
use num_traits::ToPrimitive;

use super::{consts::Consts, file::AppleAudioFile, flac::StreamInfo};
use crate::{
    GaplessInfo,
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PreparedPacket, TrackInfo},
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
    types::checked_audio_spec,
};

fn sample_rate_from_asbd(rate: f64) -> Option<u32> {
    if !rate.is_finite() || rate <= 0.0 || rate > f64::from(u32::MAX) {
        return None;
    }
    rate.to_u32()
}

/// [`Demuxer`] over [`AppleAudioFile`] for standalone (non-fMP4)
/// container formats. Currently wires WAV/PCM, FLAC, AAC (M4A/ADTS) and
/// ALAC (M4A/CAF); extends via additional file-type hints.
///
/// The [`AppleAudioFile`] packet descriptor (a `#[repr(C)]` POD) is
/// serialized into `last_packet_desc_blob` and exposed to the codec
/// layer through `Frame::packet_desc`. CBR codecs ignore it; VBR
/// codecs reinterpret the bytes back into
/// [`AudioStreamPacketDescription`].
pub(crate) struct AppleAudioFileDemuxer {
    file: AppleAudioFile,
    read_buf: ByteBuffer,
    /// Live source byte length (total), shared with the pipeline. Lets a
    /// size-less seek report an estimated `landed_byte` so the stream's byte
    /// cursor tracks where the decoder resumes: a size-less open is the one
    /// case where `AudioFile` refuses to map packet→byte itself
    /// (`kAudioFileInvalidPacketOffsetError`), and without an answer a
    /// size-less seek leaves the stream position stale and the reopen read
    /// mis-classifies as EOF. `None` / `0` when the total is unknown.
    byte_len: Option<Arc<AtomicU64>>,
    /// `Some(packets_per_call)` for CBR (`LinearPCM`) — every `next_frame`
    /// issues one batched `audio_file_read_packet_data` for that many
    /// packets. `None` for VBR: one packet per call so
    /// each `Frame` carries its own `AudioStreamPacketDescription`.
    cbr_batch_packets: Option<u32>,
    prepared: Option<PreparedPacket>,
    total_packets: Option<u64>,
    track_info: TrackInfo,
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
    pub(super) const CBR_BATCH_TARGET_BYTES: u32 = 16 * 1024;

    fn audio_spec(&self) -> DecodeResult<AudioSpec> {
        checked_audio_spec(
            self.track_info.channels,
            self.track_info.sample_rate,
            "apple.audio_file",
        )
    }

    /// Estimate the source byte offset the decoder resumes reading at after a
    /// seek that landed at `landed_at`, from the linear ratio of the landed
    /// time to the track duration scaled by the total byte length. `None`
    /// unless both the total byte length (live handle) and a positive track
    /// duration are known — callers that get `None` leave the stream cursor
    /// untouched (the pre-existing size-less behavior).
    fn estimate_landed_byte(&self, landed_at: Duration) -> Option<u64> {
        let total_bytes = self.byte_len.as_ref()?.load(Ordering::Acquire);
        if total_bytes == 0 {
            return None;
        }
        let total = self.track_info.duration?.as_nanos();
        if total == 0 {
            return None;
        }
        let landed = landed_at.as_nanos().min(total);
        let byte = u128::from(total_bytes).saturating_mul(landed) / total;
        Some(u64::try_from(byte).unwrap_or(total_bytes).min(total_bytes))
    }

    /// Single source of truth: maps `(codec, container)` to the
    /// `kAudioFileXxxType` four-cc hint `AudioFileServices` needs.
    /// Returns `None` when Apple's standalone file path can't handle the
    /// combination — the factory consults this through [`Self::supports`]
    /// before dispatching, so any new (codec, container) only needs one
    /// match arm here.
    const fn file_type_id(codec: AudioCodec, container: ContainerFormat) -> Option<u32> {
        Some(match (codec, container) {
            (AudioCodec::Pcm, ContainerFormat::Wav) => Consts::FILE_WAVE_TYPE,
            (AudioCodec::Flac, ContainerFormat::Flac) => Consts::FILE_FLAC_TYPE,
            (AudioCodec::Alac, ContainerFormat::Mp4) => Consts::FILE_M4A_TYPE,
            (AudioCodec::Alac, ContainerFormat::Caf) => Consts::FILE_CAF_TYPE,
            (AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2, ContainerFormat::Mp4) => {
                Consts::FILE_M4A_TYPE
            }
            (
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
                ContainerFormat::Adts,
            ) => Consts::FILE_AAC_ADTS_TYPE,
            _ => return None,
        })
    }

    /// A streaming FLAC open skips the packet-count scan, taking duration and buffer size from
    /// STREAMINFO instead, and keeps the real file size for correct EOF and seek behavior.
    fn open<S>(
        source: BoxedSource,
        hint: Option<u32>,
        codec: AudioCodec,
        open_mode: SourceOpenMode,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let file = match (open_mode, codec) {
            (SourceOpenMode::Streaming, AudioCodec::Flac) => {
                AppleAudioFile::open_sized_streaming(source, hint)?
            }
            _ => AppleAudioFile::open(source, hint)?,
        };
        let asbd = file.data_format;
        let total_packets = file.packet_count;
        let frames_per_packet = if asbd.frames_per_packet > 0 {
            asbd.frames_per_packet
        } else {
            4096
        };

        let extra_data = match codec {
            AudioCodec::Pcm => pod_to_vec(&asbd),
            _ => file.magic_cookie().unwrap_or_default(),
        };

        let flac_info = (codec == AudioCodec::Flac)
            .then(|| StreamInfo::parse(&extra_data).ok())
            .flatten();

        let channels =
            u16::try_from(asbd.channels_per_frame).map_err(|_| DecodeError::InvalidData {
                detail: "apple.audio_file: invalid channel count",
            })?;
        if channels == 0 {
            return Err(DecodeError::InvalidData {
                detail: "apple.audio_file: invalid zero channel count",
            });
        }
        let Some(sample_rate) = sample_rate_from_asbd(asbd.sample_rate) else {
            return Err(DecodeError::InvalidSampleRate {
                resource: "apple.audio_file",
            });
        };
        let spec = checked_audio_spec(channels, sample_rate, "apple.audio_file")?;
        let flac_duration = flac_info.filter(|info| info.total_samples > 0).map(|info| {
            spec.duration_for(info.total_samples)
                .unwrap_or(Duration::from_nanos(u64::MAX))
        });
        let duration = total_packets
            .filter(|count| *count > 0)
            .map(|total_packets| {
                let frames = total_packets.saturating_mul(u64::from(frames_per_packet));
                spec.duration_for(frames)
                    .unwrap_or(Duration::from_nanos(u64::MAX))
            })
            .or(flac_duration);

        let track_info = TrackInfo {
            codec,
            duration,
            extra_data,
            channels,
            sample_rate,
            gapless: None,
        };

        let (cbr_batch_packets, buf_cap) = if asbd.bytes_per_packet == 0 {
            let reported = usize::try_from(file.max_packet_size).map_err(DecodeError::backend)?;
            let flac_bound = flac_info.map_or(0, StreamInfo::max_frame_bytes);
            (None, reported.max(flac_bound).max(4096))
        } else {
            let packets = Self::CBR_BATCH_TARGET_BYTES
                .checked_div(asbd.bytes_per_packet)
                .map_or(1, |packets| packets.max(1));
            let bytes = packets.saturating_mul(asbd.bytes_per_packet);
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
            read_buf: pools.get_with_len::<u8>(buf_cap)?,
            last_read_len: 0,
            prepared: None,
            last_packet_desc_blob: [0u8; size_of::<AudioStreamPacketDescription>()],
            next_packet: 0,
            byte_len: None,
        })
    }

    /// Open a track for the given `(codec, container)` pair, picking the
    /// `AudioFileServices` file-type hint internally. The caller is
    /// expected to have checked [`Self::supports`] (the factory does);
    /// unsupported combinations return [`DecodeError::UnsupportedCodec`].

    #[cfg(test)]
    pub(crate) fn open_for_with_mode(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        open_mode: SourceOpenMode,
    ) -> DecodeResult<Self> {
        let pools = crate::test_pools::pools();
        Self::open_for_with_mode_and_pool(source, codec, container, open_mode, &pools)
    }

    pub(crate) fn open_for_with_mode_and_pool<S>(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        open_mode: SourceOpenMode,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let hint = container
            .and_then(|c| Self::file_type_id(codec, c))
            .ok_or(DecodeError::UnsupportedCodec { codec })?;
        Self::open(source, Some(hint), codec, open_mode, pools)
    }

    /// Attach the shared live byte-length handle so a size-less seek can
    /// report an estimated `landed_byte` (see the `byte_len` field).
    pub(crate) fn set_byte_len_handle(&mut self, handle: Option<Arc<AtomicU64>>) {
        self.file.set_byte_len_handle(handle.clone());
        self.byte_len = handle;
    }

    /// Inject encoder priming/padding metadata probed by the factory
    /// layer (e.g. `iTunSMPB`/`elst` for AAC).
    /// `AudioFileServices` does not expose MP4 edit lists,
    /// so the factory probes the source separately and pipes the
    /// captured trim counts through here.
    pub(crate) const fn set_gapless(&mut self, gapless: Option<GaplessInfo>) {
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
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        Ok(match self.prepared.take() {
            Some(PreparedPacket::Frame { pts, duration }) => DemuxOutcome::Frame(Frame {
                pts,
                duration,
                data: &self.read_buf[..self.last_read_len],
                packet_desc: if self.cbr_batch_packets.is_some() {
                    &[]
                } else {
                    &self.last_packet_desc_blob
                },
            }),
            Some(PreparedPacket::Pending(reason)) => DemuxOutcome::Pending(reason),
            Some(PreparedPacket::Eof) => DemuxOutcome::Eof,
            None => DemuxOutcome::Pending(PendingReason::Retry),
        })
    }

    fn prepare_frame(&mut self) -> DecodeResult<()> {
        if self.prepared.is_none() {
            self.prepared = Some(self.read_frame()?.into());
        }
        Ok(())
    }

    /// Apple's own packet-to-byte mapping is preferred so `landed_byte` matches the offset its
    /// packet read seeks to; a size-less open rejects it, so that open falls back to a linear
    /// estimate.
    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.prepared = None;
        let spec = self.audio_spec()?;
        if let Some(total_packets) = self.total_packets {
            let total_frames = total_packets.saturating_mul(u64::from(self.frames_per_packet));
            let total_duration = spec
                .duration_for(total_frames)
                .unwrap_or(Duration::from_nanos(u64::MAX));
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

        let target_frames = spec.frames_for(target).map_or(usize::MAX, FrameCount::get);
        let target_frame = u64::try_from(target_frames).map_err(DecodeError::backend)?;
        let target_packet = target_frame / u64::from(self.frames_per_packet.max(1));
        let backup = u64::from(priming.packets).min(target_packet);
        let landed_packet = target_packet.saturating_sub(backup);
        self.next_packet = landed_packet;

        let landed_frame = landed_packet.saturating_mul(u64::from(self.frames_per_packet));
        let landed_at = spec
            .duration_for(landed_frame)
            .unwrap_or(Duration::from_nanos(u64::MAX));

        let landed_byte = self
            .file
            .packet_to_byte(landed_packet)
            .or_else(|| self.estimate_landed_byte(landed_at));

        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

impl AppleAudioFileDemuxer {
    /// Data not ready surfaces as `Pending`, never `Err`, since an `Err` is classified as
    /// `Interrupted` upstream and retried hot instead of parking the worker; the packet cursor is
    /// left unadvanced.
    fn read_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if self
            .total_packets
            .is_some_and(|total_packets| self.next_packet >= total_packets)
        {
            return Ok(DemuxOutcome::Eof);
        }

        let spec = self.audio_spec()?;
        let start_packet = self.next_packet;
        let frame_idx = start_packet.saturating_mul(u64::from(self.frames_per_packet));
        let pts = spec
            .duration_for(frame_idx)
            .unwrap_or(Duration::from_nanos(u64::MAX));

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
            let dur = spec
                .duration_for(total_frames)
                .unwrap_or(Duration::from_nanos(u64::MAX));
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
        if !pod_write_to_slice(&desc, &mut self.last_packet_desc_blob) {
            return Err(DecodeError::InvalidData {
                detail: "packet descriptor buffer has invalid Apple ABI size",
            });
        }

        let frames = if desc.variable_frames_in_packet > 0 {
            u64::from(desc.variable_frames_in_packet)
        } else {
            u64::from(self.frames_per_packet)
        };
        let dur = spec
            .duration_for(frames)
            .unwrap_or(Duration::from_nanos(u64::MAX));

        let frame = Frame {
            pts,
            data: &self.read_buf[..self.last_read_len],
            duration: dur,
            packet_desc: &self.last_packet_desc_blob,
        };

        self.next_packet = start_packet.saturating_add(1);
        Ok(DemuxOutcome::Frame(frame))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use kithara_platform::sync::Arc;
    use kithara_stream::{
        AudioCodec, ContainerFormat, NotReadyCause, PendingReason, SourcePhase, StreamPending,
    };
    use kithara_test_fixtures::{fixtures::tone_wav, unit_fixtures::flac_saw};
    use kithara_test_utils::kithara;

    use super::{AppleAudioFileDemuxer, Duration, SourceOpenMode};
    use crate::{
        codec::CodecPriming,
        demuxer::{DemuxOutcome, Demuxer},
    };

    #[kithara::test]
    fn open_wav_demuxer_track_info_and_first_frame(tone_wav: &'static [u8]) {
        let bytes = tone_wav.to_vec();
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
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
    /// standalone `AudioFileServices` dispatch on.
    /// Native FLAC (`fLaC`, `audio/flac`) is the regression contract: the iOS
    /// build ships no Symphonia fallback, so a `false` here is precisely the
    /// `Unsupported codec: Flac` the device hit on every `streamfl` track.
    /// fMP4-FLAC and container-less FLAC must stay `false` — those route
    /// through the segment-aware path, not this standalone one.
    #[kithara::test]
    #[case(AudioCodec::Pcm, Some(ContainerFormat::Wav), true)]
    #[case(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio), false)]
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
        delegate::delegate! {
            to self.inner {
                fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
            }
        }
    }

    fn not_ready_error(pos: u64, want: usize, len: Option<u64>) -> Error {
        Error::new(
            ErrorKind::Interrupted,
            StreamPending::new(
                PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted),
                pos,
                want,
                len,
                SourcePhase::Waiting,
                0,
                false,
            ),
        )
    }

    /// Contract pin (#110): "data not ready" from the source must surface
    /// as `DemuxOutcome::Pending`, never as `Err`. An `Err` here is
    /// classified `Interrupted` by the decode loop, which retries
    /// immediately — a hot spin that burns a core for as long as the
    /// stall lasts instead of parking the worker.
    #[kithara::test]
    fn next_frame_surfaces_not_ready_as_pending_not_err(tone_wav: &'static [u8]) {
        let bytes = tone_wav.to_vec();
        let ready = u64::try_from(bytes.len() / 2).expect("fixture length fits in u64");
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(bytes, ready, None)),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
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

    /// Regression (#device-flac-slow-load): a streamed FLAC must open
    /// without `AudioFileServices` scanning the whole file to build a packet
    /// table (the `packet_count()` query a complete open issues). The scan
    /// reads to EOF — 3–37 s of startup latency on device and a full
    /// download wait on a streamed source.
    #[kithara::test]
    fn open_flac_demuxer_does_not_require_tail_bytes(flac_saw: &'static [u8]) {
        let bytes = flac_saw.to_vec();
        // A bounded streaming open reads the header + first frame (~27 KiB)
        // regardless of file size; this prefix covers that.
        let ready = 64_u64 * 1024;
        assert!(
            u64::try_from(bytes.len()).is_ok_and(|len| len > ready),
            "the fixture must outlast the ready prefix, or a full-file scan \
             would not cross it and this test would pass for nothing",
        );
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
    fn flac_streaming_not_ready_surfaces_pending_not_eof(flac_saw: &'static [u8]) {
        let bytes = flac_saw.to_vec();
        // Header + several frames are ready; the rest is "not downloaded".
        let ready = 64_u64 * 1024;
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(bytes, ready, None)),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
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

    /// Streaming source that counts `seek(End)` calls — one per `get_size`
    /// re-query — to pin the size-query cost. `seek(End)` returns the full,
    /// fixed file length (the realistic case where `Content-Length` is known
    /// at open), so a correct decoder needs the size exactly once.
    struct CountingSource {
        calls: Arc<AtomicUsize>,
        end_seeks: Arc<AtomicUsize>,
        inner: Cursor<Vec<u8>>,
    }

    impl Read for CountingSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.inner.read(buf)
        }
    }

    impl Seek for CountingSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if matches!(pos, SeekFrom::End(_)) {
                self.end_seeks.fetch_add(1, Ordering::Release);
            }
            self.inner.seek(pos)
        }
    }

    #[kithara::test]
    fn prepared_apple_packet_consumption_does_not_touch_source(tone_wav: &'static [u8]) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut prepared = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(CountingSource {
                inner: Cursor::new(tone_wav.to_vec()),
                end_seeks: Arc::default(),
                calls: Arc::clone(&calls),
            }),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
        )
        .expect("open prepared WAV");
        let mut direct = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(tone_wav.to_vec())),
            AudioCodec::Pcm,
            Some(ContainerFormat::Wav),
            SourceOpenMode::Complete,
        )
        .expect("open reference WAV");
        for target in [Duration::ZERO, Duration::from_secs(1), Duration::ZERO] {
            prepared
                .seek(target, CodecPriming::default())
                .expect("seek prepared");
            direct
                .seek(target, CodecPriming::default())
                .expect("seek reference");
            prepared.prepare_frame().expect("prepare packet");
            let before = calls.load(Ordering::Relaxed);
            prepared.prepare_frame().expect("preparation is idempotent");
            let DemuxOutcome::Frame(actual) =
                prepared.next_frame_prepared().expect("consume packet")
            else {
                panic!("expected prepared packet");
            };
            let DemuxOutcome::Frame(expected) = direct.next_frame().expect("reference packet")
            else {
                panic!("expected reference packet");
            };
            assert_eq!(actual.data, expected.data);
            assert_eq!(actual.pts, expected.pts);
            assert_eq!(actual.duration, expected.duration);
            assert_eq!(calls.load(Ordering::Relaxed), before);
        }
    }

    /// Perf contract (#device-flac-stall, regression guard): the streamed-FLAC
    /// size query must be BOUNDED, not issued per packet. The first live-size
    /// fix re-read the source length on every `get_size`, and
    /// `AudioFileServices` calls `get_size` ~per packet — on device that turned
    /// each `seek(End)` (priming + `phase_at`/`contains_range`) into per-packet
    /// work, ballooning `step_track` to 10–77 ms and starving the audio worker
    /// (`step_track took too long`), i.e. a fresh stall. With the full length
    /// known at open, the decoder must resolve the size O(1), not O(packets).
    #[kithara::test]
    fn flac_streaming_size_query_is_bounded(flac_saw: &'static [u8]) {
        let bytes = flac_saw.to_vec();
        let end_seeks = Arc::new(AtomicUsize::new(0));
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(CountingSource {
                inner: Cursor::new(bytes),
                end_seeks: Arc::clone(&end_seeks),
                calls: Arc::default(),
            }),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
        )
        .expect("streaming FLAC open");

        let mut frames = 0usize;
        loop {
            match dx.next_frame() {
                Ok(DemuxOutcome::Frame(_)) => {
                    frames += 1;
                    assert!(frames < 100_000, "runaway decode");
                }
                Ok(DemuxOutcome::Eof) => break,
                Ok(DemuxOutcome::Pending(reason)) => {
                    panic!("fully-readable source must never surface Pending: {reason:?}")
                }
                Err(e) => panic!("decode error: {e}"),
            }
        }

        let count = end_seeks.load(Ordering::Acquire);
        assert!(
            count <= 4,
            "size query not bounded: {count} seek(End) calls over {frames} frames — \
             get_size must not re-read the source length per packet (the device perf \
             regression that starved step_track). Expected O(1)."
        );
    }
}
