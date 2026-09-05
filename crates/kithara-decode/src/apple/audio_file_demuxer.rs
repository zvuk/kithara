use std::{
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_apple::audio_toolbox::{AudioStreamPacketDescription, pod_to_vec, pod_write_to_slice};
use kithara_bufpool::{ByteBuffer, HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, FrameCount};
use kithara_stream::{AudioCodec, ContainerFormat, PrerollHint};
use num_traits::ToPrimitive;

use super::{audio_file::AppleAudioFile, consts::Consts, flac::StreamInfo};
use crate::{
    GaplessInfo,
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
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
    /// issues one batched `audio_file_read_packet_data` for that many
    /// packets. `None` for VBR (MP3, ALAC) — one packet per call so
    /// each `Frame` carries its own `AudioStreamPacketDescription`.
    cbr_batch_packets: Option<u32>,
    total_packets: Option<u64>,
    track_info: TrackInfo,
    read_buf: ByteBuffer,
    last_packet_desc_blob: [u8; size_of::<AudioStreamPacketDescription>()],
    frames_per_packet: u32,
    next_packet: u64,
    last_read_len: usize,
    /// Live source byte length (total), shared with the pipeline. Lets a
    /// size-less seek report an estimated `landed_byte` so the stream's byte
    /// cursor tracks where the decoder resumes: a size-less open is the one
    /// case where `AudioFile` refuses to map packet→byte itself
    /// (`kAudioFileInvalidPacketOffsetError`), and without an answer a
    /// size-less MP3 seek leaves the stream position stale and the reopen read
    /// mis-classifies as EOF. `None` / `0` when the total is unknown.
    byte_len: Option<Arc<AtomicU64>>,
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
    const fn file_type_id(codec: AudioCodec, container: ContainerFormat) -> Option<u32> {
        Some(match (codec, container) {
            (AudioCodec::Pcm, ContainerFormat::Wav) => Consts::FILE_WAVE_TYPE,
            (AudioCodec::Mp3, ContainerFormat::MpegAudio) => Consts::FILE_MP3_TYPE,
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

    fn open<S>(
        source: BoxedSource,
        hint: Option<u32>,
        codec: AudioCodec,
        open_mode: SourceOpenMode,
        duration_hint: Option<Duration>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        // MP3 and FLAC are VBR with no on-disk packet index, so a complete
        // open would query `packet_count()` — forcing `AudioFileServices` to
        // scan the WHOLE file to build a packet table before the first frame
        // (3–37 s on device for large lossless tracks, and a full download
        // wait on a streamed source). The streaming opens skip that scan;
        // duration and the read-buffer size come from cheap header metadata
        // instead (Xing for MP3, STREAMINFO for FLAC).
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

        // FLAC's magic cookie carries STREAMINFO: `total_samples` yields the
        // exact duration and `max_frame_size` bounds the VBR read buffer when
        // the streaming open leaves `max_packet_size()` at 0.
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

        let (cbr_batch_packets, buf_cap) = if asbd.bytes_per_packet == 0 {
            // VBR. A streaming open reports no max packet size, so fall back
            // to the FLAC STREAMINFO frame bound (FLAC frames reach ~16-19
            // KiB, far past the 4 KiB floor) when AudioFile can't supply one.
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
        duration_hint: Option<Duration>,
    ) -> DecodeResult<Self> {
        let pools = crate::test_pools::pools();
        Self::open_for_with_mode_and_pool(
            source,
            codec,
            container,
            open_mode,
            duration_hint,
            &pools,
        )
    }

    pub(crate) fn open_for_with_mode_and_pool<S>(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        open_mode: SourceOpenMode,
        duration_hint: Option<Duration>,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let hint = container
            .and_then(|c| Self::file_type_id(codec, c))
            .ok_or(DecodeError::UnsupportedCodec { codec })?;
        Self::open(source, Some(hint), codec, open_mode, duration_hint, pools)
    }

    /// Inject encoder priming/padding metadata probed by the factory
    /// layer (e.g. Xing/Info+LAME for MP3, `iTunSMPB`/`elst` for AAC).
    /// `AudioFileServices` does not expose Xing/LAME or MP4 edit lists,
    /// so the factory probes the source separately and pipes the
    /// captured trim counts through here.
    pub(crate) const fn set_gapless(&mut self, gapless: Option<GaplessInfo>) {
        self.track_info.gapless = gapless;
    }

    /// Attach the shared live byte-length handle so a size-less seek can
    /// report an estimated `landed_byte` (see the `byte_len` field).
    pub(crate) fn set_byte_len_handle(&mut self, handle: Option<Arc<AtomicU64>>) {
        self.file.set_byte_len_handle(handle.clone());
        self.byte_len = handle;
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

    fn audio_spec(&self) -> DecodeResult<AudioSpec> {
        checked_audio_spec(
            self.track_info.channels,
            self.track_info.sample_rate,
            "apple.audio_file",
        )
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

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
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

        // Prefer Apple's own packet→byte mapping so `landed_byte` matches the
        // offset its packet read seeks to; fall back to a linear estimate from
        // the live total when the property is unavailable.
        // Apple's own packet→byte mapping is exact and is the offset its packet
        // read seeks to, but a size-less open rejects it outright
        // (`kAudioFileInvalidPacketOffsetError`). That degraded mode — the
        // streamed MP3 path — falls back to the linear estimate; see
        // `estimate_landed_byte`.
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

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };

    use kithara_platform::sync::Arc;
    use kithara_stream::{
        AudioCodec, ContainerFormat, NotReadyCause, PendingReason, SourcePhase, StreamPending,
    };
    use kithara_test_fixtures::assets::{
        flac_unknown_length_saw_6s, signal_mp3_track_sine440_187s, signal_wav_sine440_1s,
    };
    use kithara_test_utils::kithara;

    use super::{AppleAudioFileDemuxer, Duration, SourceOpenMode};
    use crate::{
        codec::CodecPriming,
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    };

    #[kithara::test]
    fn open_wav_demuxer_track_info_and_first_frame() {
        let bytes = signal_wav_sine440_1s().bytes().to_vec();
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
    /// standalone `AudioFileServices` dispatch on.
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

    struct TailLoopGuard {
        inner: Cursor<Vec<u8>>,
        last_short_read: Option<(u64, usize, usize)>,
        tripped: Arc<AtomicBool>,
    }

    impl Read for TailLoopGuard {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let start = self.inner.position();
            let read = self.inner.read(buf)?;
            if read > 0 && read < buf.len() {
                let span = (start, buf.len(), read);
                if self.last_short_read == Some(span) {
                    self.tripped.store(true, Ordering::Release);
                    return Err(Error::other("repeated short tail read"));
                }
                self.last_short_read = Some(span);
            } else {
                self.last_short_read = None;
            }
            Ok(read)
        }
    }

    impl Seek for TailLoopGuard {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
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
    fn next_frame_surfaces_not_ready_as_pending_not_err() {
        let bytes = signal_wav_sine440_1s().bytes().to_vec();
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
        let bytes = signal_mp3_track_sine440_187s().bytes().to_vec();
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
            Some(Duration::from_secs(2)),
        )
        .expect("MP3 streaming open must not require tail bytes");
        assert!(
            !tail_read_attempted.load(Ordering::Acquire),
            "MP3 streaming open must not probe bytes beyond the startup prefix"
        );
        assert_eq!(
            dx.duration(),
            Some(Duration::from_secs(2)),
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

    /// Demuxes `bytes` to EOF in size-less streaming mode and returns how many
    /// packets came out, failing if `AudioFileServices` repeats a short tail
    /// read.
    fn packets_to_eof(bytes: Vec<u8>) -> u64 {
        let byte_len = u64::try_from(bytes.len()).expect("fixture length fits u64");
        let tripped = Arc::new(AtomicBool::new(false));
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(TailLoopGuard {
                inner: Cursor::new(bytes),
                last_short_read: None,
                tripped: Arc::clone(&tripped),
            }),
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
            SourceOpenMode::Streaming,
            Some(Duration::from_secs(188)),
        )
        .expect("streaming MP3 open");
        dx.set_byte_len_handle(Some(Arc::new(AtomicU64::new(byte_len))));

        let mut packets = 0_u64;
        loop {
            match dx.next_frame() {
                Ok(DemuxOutcome::Frame(_)) => {
                    packets += 1;
                    // A packet costs at least one byte, so this cannot be
                    // reached by a run that is making progress.
                    assert!(
                        packets <= byte_len,
                        "demuxer produced more packets than bytes"
                    );
                }
                Ok(DemuxOutcome::Eof) => break,
                Ok(DemuxOutcome::Pending(reason)) => {
                    panic!("complete fixture must not become pending: {reason:?}")
                }
                Err(error) => panic!("complete fixture must reach EOF: {error}"),
            }
        }

        assert!(
            !tripped.load(Ordering::Acquire),
            "AudioFileServices repeated the same short tail read"
        );
        packets
    }

    #[kithara::test(native, flash(false))]
    fn size_less_mp3_with_id3v1_tail_reaches_eof() {
        let plain = signal_mp3_track_sine440_187s().bytes().to_vec();
        let mut tagged = plain.clone();
        let mut id3v1 = [0_u8; 128];
        id3v1[..3].copy_from_slice(b"TAG");
        tagged.extend_from_slice(&id3v1);

        let plain_packets = packets_to_eof(plain);
        // The fixture is a 187 s 44.1 kHz clip and MPEG Layer III carries 1152
        // frames per packet, so a run that reached EOF cannot come back with
        // less than that budget.
        assert!(
            plain_packets >= 187 * 44_100 / 1_152,
            "a complete pass must cover the fixture's own frame budget, got {plain_packets}"
        );
        assert_eq!(
            packets_to_eof(tagged),
            plain_packets,
            "the ID3v1 tail must not cost the final MPEG packet"
        );
    }

    /// Seeks `dx` to three seconds and returns the reported `landed_byte`.
    fn landed_byte_at_3s(dx: &mut AppleAudioFileDemuxer) -> Option<u64> {
        match dx
            .seek(Duration::from_secs(3), CodecPriming::default())
            .expect("seek returns an outcome")
        {
            DemuxSeekOutcome::Landed { landed_byte, .. } => landed_byte,
            other => panic!("unexpected seek outcome: {other:?}"),
        }
    }

    /// Opens the generated full-length MP3 clip in `mode`, returning the
    /// demuxer and the fixture's total byte length.
    fn open_mp3(mode: SourceOpenMode) -> (AppleAudioFileDemuxer, u64) {
        let bytes = signal_mp3_track_sine440_187s().bytes().to_vec();
        let total = u64::try_from(bytes.len()).expect("fixture length fits in u64");
        let dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
            mode,
            Some(Duration::from_secs(180)),
        )
        .expect("MP3 open");
        (dx, total)
    }

    /// Regression (reopen false-EOF): a size-less MP3 seek must report a
    /// `landed_byte` so the pipeline realigns the stream's byte cursor with
    /// where the decoder resumes (`seek::emit` calls `stream.set_position`).
    /// Before the fix an Apple MP3 seek returned `landed_byte: None`, the
    /// stream position stayed at the pre-seek offset, and a reopened track
    /// mis-classified the post-seek read as EOF. Mirrors the symphonia MP3
    /// contract asserted in `composed.rs`.
    ///
    /// This is the degraded path: a size-less open makes Apple's own mapping
    /// answer `kAudioFileInvalidPacketOffsetError`, so the live byte-length
    /// handle carries the answer. Pinned separately from the exact path in
    /// [`sized_mp3_seek_reports_landed_byte_without_a_length_handle`].
    #[kithara::test]
    fn size_less_mp3_seek_reports_landed_byte_from_the_length_handle() {
        let (mut dx, total) = open_mp3(SourceOpenMode::Streaming);
        dx.set_byte_len_handle(Some(Arc::new(AtomicU64::new(total))));

        let byte =
            landed_byte_at_3s(&mut dx).expect("size-less MP3 seek must expose a landed_byte");

        assert!(byte > 0, "landed_byte must leave the start of the file");
    }

    /// The same size-less seek must land inside the file: a `landed_byte` past
    /// the end would move the stream cursor to a false EOF — the very failure
    /// the reported offset exists to prevent.
    #[kithara::test]
    fn size_less_mp3_landed_byte_stays_inside_the_file() {
        let (mut dx, total) = open_mp3(SourceOpenMode::Streaming);
        dx.set_byte_len_handle(Some(Arc::new(AtomicU64::new(total))));

        let byte =
            landed_byte_at_3s(&mut dx).expect("size-less MP3 seek must expose a landed_byte");

        assert!(
            byte < total,
            "landed_byte {byte} must precede EOF ({total})"
        );
    }

    /// The exact path: with a known size `AudioFile` answers
    /// `kAudioFilePropertyPacketToByte` itself, so the seek reports a
    /// `landed_byte` with NO byte-length handle attached — the estimate cannot
    /// contribute here, which is what makes this a pin on Apple's own mapping
    /// rather than on the degraded fallback.
    #[kithara::test]
    fn sized_mp3_seek_reports_landed_byte_without_a_length_handle() {
        let (mut dx, _total) = open_mp3(SourceOpenMode::Complete);

        let byte = landed_byte_at_3s(&mut dx)
            .expect("a sized open must map packet→byte through AudioFile itself");

        assert!(byte > 0, "landed_byte must leave the start of the file");
    }

    /// Regression (#device-flac-slow-load): a streamed FLAC must open
    /// without `AudioFileServices` scanning the whole file to build a packet
    /// table (the `packet_count()` query a complete open issues). The scan
    /// reads to EOF — 3–37 s of startup latency on device and a full
    /// download wait on a streamed source. Mirrors the MP3 contract above.
    #[kithara::test]
    fn open_flac_demuxer_does_not_require_tail_bytes() {
        let bytes = flac_unknown_length_saw_6s().bytes().to_vec();
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
        let bytes = signal_mp3_track_sine440_187s().bytes().to_vec();
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
        let bytes = flac_unknown_length_saw_6s().bytes().to_vec();
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

    /// The MP3 half of the pair the streaming open splits.
    ///
    /// `open_for_with_mode` hands `AudioFile` a real size for streaming FLAC
    /// and nothing for streaming MP3, so only FLAC can tell "no data yet"
    /// from "no data ever". A size-less MP3 reading past the download
    /// boundary must still park: ending the track there is the mid-stream
    /// stop the FLAC regression above was written for. The ready prefix
    /// carries the header and several frames; everything past it is the
    /// part that has not been downloaded.
    #[kithara::test]
    fn mp3_streaming_not_ready_surfaces_pending_not_eof() {
        let bytes = signal_mp3_track_sine440_187s().bytes().to_vec();
        let ready = 64_u64 * 1024;
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(NotReadySource::new(bytes, ready, None)),
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
            SourceOpenMode::Streaming,
            None,
        )
        .expect("streaming MP3 open");

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
        end_seeks: Arc<AtomicUsize>,
        inner: Cursor<Vec<u8>>,
    }

    impl Read for CountingSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for CountingSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            if matches!(pos, SeekFrom::End(_)) {
                self.end_seeks.fetch_add(1, Ordering::Release);
            }
            self.inner.seek(pos)
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
    fn flac_streaming_size_query_is_bounded() {
        let bytes = flac_unknown_length_saw_6s().bytes().to_vec();
        let end_seeks = Arc::new(AtomicUsize::new(0));
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(CountingSource {
                inner: Cursor::new(bytes),
                end_seeks: Arc::clone(&end_seeks),
            }),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
            None,
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
