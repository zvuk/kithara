use std::{
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_apple::audio_toolbox::{
    AUDIO_FORMAT_MPEG4_AAC_HE, AUDIO_FORMAT_MPEG4_AAC_HE_V2, AudioStreamPacketDescription,
    pod_to_vec, pod_write_to_slice,
};
use kithara_bufpool::{ByteBuffer, HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, FrameCount};
use kithara_stream::{AudioCodec, ByteMap, ContainerFormat, PendingReason, PrerollHint};
use num_traits::ToPrimitive;

use super::{consts, converter::derive_aac_asbd_from_esds, file::AppleAudioFile, flac::StreamInfo};
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

/// [`Demuxer`] over [`AppleAudioFile`] for standalone
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
    byte_map: Option<Arc<dyn ByteMap>>,
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
    Segmented(u64),
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
        let crate::DecoderDuration::RawLength(duration) = self.track_info.duration? else {
            return None;
        };
        let total = duration.as_nanos();
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
            (AudioCodec::Pcm, ContainerFormat::Wav) => consts::FILE_WAVE_TYPE,
            (AudioCodec::Pcm, ContainerFormat::Aiff) => consts::FILE_AIFF_TYPE,
            (AudioCodec::Pcm | AudioCodec::Alac, ContainerFormat::Caf) => consts::FILE_CAF_TYPE,
            (AudioCodec::Flac, ContainerFormat::Flac) => consts::FILE_FLAC_TYPE,
            (AudioCodec::Alac, ContainerFormat::Mp4 | ContainerFormat::Fmp4) => {
                consts::FILE_M4A_TYPE
            }
            (
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
                ContainerFormat::Mp4 | ContainerFormat::Fmp4,
            ) => consts::FILE_M4A_TYPE,
            (
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
                ContainerFormat::Adts,
            ) => consts::FILE_AAC_ADTS_TYPE,
            _ => return None,
        })
    }

    /// Streaming FLAC and segmented PCM skip the eager packet-count query so an unavailable
    /// tail cannot freeze a partial packet table. FLAC retains STREAMINFO duration and bounds.
    fn open<S>(
        mut source: BoxedSource,
        hint: Option<u32>,
        codec: AudioCodec,
        open_mode: SourceOpenMode,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Self>
    where
        S: HasPool<u8>,
    {
        let pcm_data_bytes = Self::pcm_data_bytes(&mut source, hint, codec, open_mode)?;
        let file = match (open_mode, codec) {
            (SourceOpenMode::Segmented(end), _) => {
                AppleAudioFile::open_segmented(source, hint, end, pools)?
            }
            (SourceOpenMode::Streaming, AudioCodec::Flac | AudioCodec::Pcm) => {
                AppleAudioFile::open_sized_streaming(source, hint, pools)?
            }
            _ => AppleAudioFile::open(source, hint, pools)?,
        };
        let mut asbd = file.data_format;
        Self::validate_codec(codec, asbd.format_id)?;
        let total_packets = Self::packet_count(&file, pcm_data_bytes)?;
        let extra_data = match codec {
            AudioCodec::Pcm => pod_to_vec(&asbd),
            _ => file.magic_cookie().unwrap_or_default(),
        };

        if matches!(
            codec,
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2
        ) && !extra_data.is_empty()
        {
            asbd = derive_aac_asbd_from_esds(&extra_data)?;
        }
        let frames_per_packet = if asbd.frames_per_packet > 0 {
            asbd.frames_per_packet
        } else {
            4096
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
        let duration = if codec == AudioCodec::Alac {
            Some(
                spec.duration_for(file.valid_frames()?)
                    .map_err(DecodeError::backend)?,
            )
        } else {
            total_packets
                .filter(|count| *count > 0)
                .map(|total_packets| {
                    let frames = total_packets.saturating_mul(u64::from(frames_per_packet));
                    spec.duration_for(frames)
                        .unwrap_or(Duration::from_nanos(u64::MAX))
                })
                .or(flac_duration)
        };

        let track_info = TrackInfo {
            codec,
            duration: duration.map(crate::DecoderDuration::RawLength),
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
            byte_map: None,
        })
    }

    fn validate_codec(codec: AudioCodec, format_id: u32) -> DecodeResult<()> {
        let matches_codec = match codec {
            AudioCodec::Pcm => format_id == consts::FORMAT_LINEAR_PCM,
            AudioCodec::Flac => format_id == consts::FORMAT_FLAC,
            AudioCodec::Alac => format_id == consts::FORMAT_APPLE_LOSSLESS,
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => matches!(
                format_id,
                consts::FORMAT_MPEG4_AAC | AUDIO_FORMAT_MPEG4_AAC_HE | AUDIO_FORMAT_MPEG4_AAC_HE_V2
            ),
            _ => false,
        };
        if !matches_codec {
            return Err(DecodeError::InvalidData {
                detail: "apple.audio_file: codec does not match supplied metadata",
            });
        }
        Ok(())
    }

    fn pcm_data_bytes(
        source: &mut BoxedSource,
        hint: Option<u32>,
        codec: AudioCodec,
        open_mode: SourceOpenMode,
    ) -> DecodeResult<Option<u64>> {
        if codec == AudioCodec::Pcm
            && hint == Some(consts::FILE_WAVE_TYPE)
            && open_mode != SourceOpenMode::Complete
        {
            let data = crate::factory::wav_data_range(source)?;
            if let SourceOpenMode::Segmented(end) = open_mode
                && data.end > end
            {
                return Err(DecodeError::InvalidData {
                    detail: "apple.audio_file: WAV data exceeds the declared source extent",
                });
            }
            Ok(Some(data.end - data.start))
        } else {
            Ok(None)
        }
    }

    fn packet_count(
        file: &AppleAudioFile,
        pcm_data_bytes: Option<u64>,
    ) -> DecodeResult<Option<u64>> {
        let asbd = file.data_format;
        Ok(match pcm_data_bytes {
            Some(bytes) => {
                let packet_bytes = u64::from(asbd.bytes_per_packet);
                let packet_frames = u64::from(asbd.frames_per_packet);
                let frame_bytes = u64::from(asbd.bytes_per_frame);
                if packet_bytes == 0
                    || packet_frames == 0
                    || frame_bytes == 0
                    || frame_bytes.checked_mul(packet_frames) != Some(packet_bytes)
                    || !bytes.is_multiple_of(packet_bytes)
                {
                    return Err(DecodeError::InvalidData {
                        detail: "apple.audio_file: WAV data does not match native PCM packet geometry",
                    });
                }
                Some(bytes / packet_bytes)
            }
            None => file.packet_count,
        })
    }

    /// [`Self::open_for_with_mode_and_pool`] on the test pools.
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

    /// Open a track for the given `(codec, container)` pair, picking the
    /// `AudioFileServices` file-type hint internally. The caller is
    /// expected to have checked [`Self::supports`] (the factory does);
    /// unsupported combinations return [`DecodeError::UnsupportedCodec`].
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

    pub(crate) fn set_byte_map(&mut self, byte_map: Option<Arc<dyn ByteMap>>) {
        self.byte_map = byte_map;
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
    fn current_segment_index(&self) -> Option<u32> {
        let byte = self
            .file
            .packet_to_byte(self.next_packet.saturating_sub(1))?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte)
            .map(|segment| segment.segment_index)
    }

    fn current_variant_index(&self) -> Option<usize> {
        let byte = self
            .file
            .packet_to_byte(self.next_packet.saturating_sub(1))?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte)
            .map(|segment| segment.variant_index)
    }

    fn duration(&self) -> Option<crate::DecoderDuration> {
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

    fn media_origin(&self) -> Option<Duration> {
        Some(Duration::ZERO)
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
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };

    use kithara_platform::sync::Arc;
    use kithara_stream::{
        AudioCodec, ContainerFormat, NotReadyCause, PendingReason, SourcePhase, StreamPending,
    };
    use kithara_test_fixtures::{
        assets::signal_profile_flac_flac_96000_2ch_24bit, fixtures::tone_wav,
        unit_fixtures::flac_saw,
    };
    use kithara_test_utils::kithara;

    use super::{AppleAudioFileDemuxer, Duration, SourceOpenMode};
    use crate::{
        codec::CodecPriming,
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    };

    #[kithara::test]
    fn open_wav_demuxer_track_info_and_first_frame(tone_wav: &'static [u8]) {
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(tone_wav.to_vec())),
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
        let duration = info.duration;

        match dx.next_frame().expect("next_frame ok") {
            DemuxOutcome::Frame(frame) => assert!(!frame.data.is_empty()),
            other => panic!("expected Frame, got {other:?}"),
        }

        let length = u64::try_from(tone_wav.len()).expect("fixture length");
        for mode in [SourceOpenMode::Streaming, SourceOpenMode::Segmented(length)] {
            let tail_read = Arc::new(AtomicBool::new(false));
            let mut streamed = AppleAudioFileDemuxer::open_for_with_mode(
                Box::new(NotReadySource::new(
                    tone_wav.to_vec(),
                    length / 2,
                    Some(Arc::clone(&tail_read)),
                )),
                AudioCodec::Pcm,
                Some(ContainerFormat::Wav),
                mode,
            )
            .expect("WAV header prefix supplies its complete frame count");
            assert_eq!(streamed.track_info().duration, duration, "{mode:?}");
            assert!(
                !tail_read.load(Ordering::Acquire),
                "{mode:?} must resolve PCM extent without reading the unavailable tail"
            );
            assert!(matches!(
                streamed.next_frame().expect("first streamed PCM packet"),
                DemuxOutcome::Frame(frame) if !frame.data.is_empty()
            ));
            assert!(!tail_read.load(Ordering::Acquire));
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
        ready: Arc<AtomicU64>,
    }

    impl NotReadySource {
        fn new(bytes: Vec<u8>, ready: u64, notify_not_ready: Option<Arc<AtomicBool>>) -> Self {
            let inner = Cursor::new(bytes);
            Self {
                inner,
                notify_not_ready,
                ready: Arc::new(AtomicU64::new(ready)),
            }
        }
    }

    impl Read for NotReadySource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.inner.position();
            let want = u64::try_from(buf.len())
                .map_err(|_| Error::other("NotReadySource request exceeds u64"))?;
            // `probe_read` waits on the whole requested range; a request crossing
            // the delivery boundary fails instead of returning a partial read.
            let ready = self.ready.load(Ordering::Acquire);
            if pos.saturating_add(want) > ready {
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

    #[kithara::test(native)]
    fn flac_seek_into_a_waiting_packet_resumes_when_bytes_arrive() {
        let bytes = signal_profile_flac_flac_96000_2ch_24bit().bytes().to_vec();
        let total = u64::try_from(bytes.len()).expect("fixture length fits in u64");
        let prefix = 96 * 1024;
        assert!(total > prefix, "the fixture must exceed its ready prefix");
        let boundary = Arc::new(AtomicBool::new(false));
        let source = NotReadySource::new(bytes, prefix, Some(Arc::clone(&boundary)));
        let ready = Arc::clone(&source.ready);
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(source),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
        )
        .expect("streaming FLAC open");
        assert_eq!(dx.track_info().sample_rate, 96_000);
        assert_eq!(dx.track_info().channels, 2);
        assert_eq!(
            dx.duration(),
            Some(crate::DecoderDuration::RawLength(Duration::from_secs(6)))
        );

        let target = Duration::from_secs(1);
        for index in 0..4 {
            let outcome = dx.next_frame().expect("ready startup frame");
            let DemuxOutcome::Frame(frame) = outcome else {
                panic!("startup frame {index} must be ready, got {outcome:?}");
            };
            assert!(frame.pts + frame.duration < target);
        }
        assert!(
            !boundary.load(Ordering::Acquire),
            "startup reads must stay inside the ready prefix"
        );

        let seek_pending = match dx.seek(target, CodecPriming::default()) {
            Ok(DemuxSeekOutcome::Landed { .. }) => {
                let outcome = dx.next_frame().expect("target frame status");
                assert!(
                    matches!(outcome, DemuxOutcome::Pending(PendingReason::NotReady(_))),
                    "the unavailable target must stay pending, got {outcome:?}"
                );
                false
            }
            Err(error) => {
                assert!(
                    matches!(error.pending_reason(), Some(PendingReason::NotReady(_))),
                    "the unavailable seek must preserve its pending reason, got {error:?}"
                );
                true
            }
            Ok(outcome) => panic!("seek inside the track must not end it, got {outcome:?}"),
        };
        assert!(
            boundary.load(Ordering::Acquire),
            "the target read must reach the unavailable bytes"
        );

        ready.store(total, Ordering::Release);
        if seek_pending {
            assert!(matches!(
                dx.seek(target, CodecPriming::default())
                    .expect("resumed seek"),
                DemuxSeekOutcome::Landed { .. }
            ));
        }
        let outcome = dx.next_frame().expect("released target frame");
        let DemuxOutcome::Frame(frame) = outcome else {
            panic!("the same decoder must resume after its bytes arrive, got {outcome:?}");
        };
        assert!(!frame.data.is_empty());
        assert!(frame.pts <= target && frame.pts + frame.duration > target);
    }

    #[kithara::test(native)]
    fn indexed_flac_packet_resumes_when_its_bytes_are_unavailable() {
        let bytes = signal_profile_flac_flac_96000_2ch_24bit().bytes().to_vec();
        let total = u64::try_from(bytes.len()).expect("fixture length fits in u64");
        let source = NotReadySource::new(bytes, total, None);
        let ready = Arc::clone(&source.ready);
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(source),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
        )
        .expect("streaming FLAC open");
        let indexed_through = Duration::from_secs(2);
        loop {
            let outcome = dx.next_frame().expect("available FLAC packet");
            let DemuxOutcome::Frame(frame) = outcome else {
                panic!("the fully available prefix must produce a packet, got {outcome:?}");
            };
            if frame.pts >= indexed_through {
                break;
            }
        }
        let target = Duration::from_secs(1);
        dx.seek(target, CodecPriming::default())
            .expect("seek to an indexed packet");
        ready.store(64 * 1024, Ordering::Release);
        let outcome = dx.next_frame().expect("unavailable indexed packet status");
        assert!(
            matches!(outcome, DemuxOutcome::Pending(PendingReason::NotReady(_))),
            "an indexed packet with unavailable bytes must stay pending, got {outcome:?}"
        );
        ready.store(total, Ordering::Release);
        let outcome = dx.next_frame().expect("released indexed packet");
        let DemuxOutcome::Frame(frame) = outcome else {
            panic!("the same native index must resume its packet, got {outcome:?}");
        };
        assert!(!frame.data.is_empty());
        assert!(frame.pts <= target && frame.pts + frame.duration > target);
    }

    #[kithara::test(native)]
    fn streaming_flac_walk_and_indexed_seeks_preserve_every_packet() {
        let bytes = signal_profile_flac_flac_96000_2ch_24bit().bytes().to_vec();
        let mut complete = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes.clone())),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Complete,
        )
        .expect("complete FLAC open");
        let mut dx = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Streaming,
        )
        .expect("streaming FLAC open");
        let mut packets = Vec::new();
        loop {
            match dx.next_frame().expect("fully available native packet") {
                DemuxOutcome::Frame(frame) => {
                    assert!(!frame.data.is_empty());
                    let expected = complete.next_frame().expect("complete reference packet");
                    let DemuxOutcome::Frame(expected) = expected else {
                        panic!("streaming read must match the complete file, got {expected:?}");
                    };
                    assert_eq!(frame.pts, expected.pts);
                    assert_eq!(frame.duration, expected.duration);
                    assert_eq!(
                        frame.data.len(),
                        expected.data.len(),
                        "native packet length at {:?}",
                        frame.pts
                    );
                    assert_eq!(frame.data, expected.data);
                    assert!(
                        packets.len() < 200,
                        "native walk must reach the fixture EOF"
                    );
                    packets.push((frame.pts, frame.duration, frame.data.to_vec()));
                }
                DemuxOutcome::Eof => break,
                outcome @ DemuxOutcome::Pending(_) => {
                    panic!("available data must produce a packet or EOF, got {outcome:?}")
                }
            }
        }
        assert!(matches!(
            complete.next_frame().expect("complete reference EOF"),
            DemuxOutcome::Eof
        ));
        let (last_pts, last_duration, _) = packets.last().expect("fixture packets");
        let duration = Duration::from_secs(6);
        assert_eq!(
            dx.duration(),
            Some(crate::DecoderDuration::RawLength(duration))
        );
        assert!(*last_pts < duration && *last_pts + *last_duration >= duration);
        assert!(matches!(
            dx.next_frame().expect("stable natural EOF"),
            DemuxOutcome::Eof
        ));
        for seconds in [1, 4, 0, 5, 2] {
            let target = Duration::from_secs(seconds);
            let expected = packets
                .iter()
                .find(|(pts, duration, _)| *pts <= target && *pts + *duration > target)
                .expect("indexed target packet");
            dx.seek(target, CodecPriming::default())
                .expect("seek inside the complete index");
            let outcome = dx.next_frame().expect("read indexed packet");
            let DemuxOutcome::Frame(actual) = outcome else {
                panic!("indexed target must produce its packet, got {outcome:?}");
            };
            assert_eq!(actual.pts, expected.0);
            assert_eq!(actual.duration, expected.1);
            assert_eq!(actual.data, expected.2);
        }
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
