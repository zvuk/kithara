//! `SymphoniaDemuxer` — generic [`Demuxer`] over `symphonia`'s
//! [`FormatReader`].
//!
//! Wraps `Box<dyn FormatReader>` for any container Symphonia handles
//! (MP3, FLAC, OGG, WAV, AIFF, ADTS, MKV, MP4 / fMP4 file). The demuxer
//! pumps packets out of the audio track and exposes them as
//! [`DemuxOutcome::Frame`] values consumable by any [`FrameCodec`].

use std::{
    io::{ErrorKind, Read, Seek},
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use kithara_stream::{AudioCodec, ContainerFormat, PendingReason, StreamSeekPastEof};
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{
            AudioCodecId, AudioCodecParameters,
            well_known::{
                CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS,
                CODEC_ID_VORBIS,
            },
        },
    },
    errors::{Error as SymphoniaError, SeekErrorKind},
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType},
    units::{Time, TimeBase, Timestamp},
};

use crate::{
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::{DecodeError, DecodeResult},
    symphonia::{
        config::SymphoniaConfig,
        probe::{ReaderBootstrap, new_direct, probe_with_seek},
    },
};

/// Demuxer adapter over Symphonia's [`FormatReader`].
pub(crate) struct SymphoniaDemuxer {
    /// Native Symphonia codec parameters for the audio track. Carried so
    /// the matching [`SymphoniaCodec::open_native`] path can build a
    /// decoder for codecs whose generic [`AudioCodec`] enum representation
    /// loses information (PCM bit-depth/endianness, ADPCM dialect).
    native_params: AudioCodecParameters,
    format_reader: Box<dyn FormatReader>,
    /// Live byte cursor of the underlying media source. Populated by
    /// the [`super::super::symphonia::adapter::ReadSeekAdapter`] when
    /// the demuxer is built through that path; absent for synthetic
    /// readers in unit tests.
    byte_pos_handle: Option<Arc<AtomicU64>>,
    /// Latest packet handed to [`Demuxer::next_frame`]. Held so the
    /// returned `Frame<'_>` can borrow the packet's `Box<[u8]>` payload
    /// directly — Symphonia owns the allocation, we don't clone it.
    /// Replaced (and the previous packet dropped) on every successful
    /// `next_frame` call.
    current_packet: Option<symphonia::core::packet::Packet>,
    /// Time base used to translate packet timestamps into wall-clock
    /// [`std::time::Duration`].
    time_base: Option<TimeBase>,
    track_info: TrackInfo,
    track_id: u32,
}

impl SymphoniaDemuxer {
    fn current_byte(&self) -> Option<u64> {
        self.byte_pos_handle
            .as_ref()
            .map(|h| h.load(std::sync::atomic::Ordering::Acquire))
    }

    fn dur_to_duration(&self, dur: symphonia::core::units::Duration) -> Duration {
        let Some(tb) = self.time_base else {
            return Duration::ZERO;
        };
        let ts = Timestamp::new(i64::try_from(dur.get()).unwrap_or(i64::MAX));
        tb.calc_time(ts).map_or(Duration::ZERO, time_to_duration)
    }

    /// Build from an already-constructed `FormatReader`.
    ///
    /// The factory layer is responsible for wiring up the
    /// `Source -> MediaSource` adapter and probing the right reader
    /// for the container; `SymphoniaDemuxer` only deals with the
    /// post-bootstrap object.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the reader exposes no
    /// audio track or the audio track's codec parameters are missing
    /// fields the demuxer needs (sample rate, channel count).
    pub(crate) fn from_reader(
        format_reader: Box<dyn FormatReader>,
        byte_pos_handle: Option<Arc<AtomicU64>>,
    ) -> DecodeResult<Self> {
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::ProbeFailed)?
            .clone();
        let track_id = track.id;
        let Some(CodecParameters::Audio(native_params)) = &track.codec_params else {
            return Err(DecodeError::ProbeFailed);
        };
        let native_params = native_params.clone();
        let track_info = build_track_info(&track, &native_params)?;
        let time_base = track.time_base;
        Ok(Self {
            format_reader,
            track_id,
            track_info,
            native_params,
            time_base,
            byte_pos_handle,
            current_packet: None,
        })
    }

    /// Native Symphonia codec parameters for the selected track.
    ///
    /// Exposed so the [`SymphoniaCodec`] wiring path can build a registry
    /// decoder for PCM/ADPCM tracks where the generic `AudioCodec` enum
    /// in [`TrackInfo`] does not carry enough information (bit-depth,
    /// endianness, dialect).
    ///
    /// [`SymphoniaCodec`]: crate::codec::SymphoniaCodec
    #[must_use]
    pub(crate) fn native_params(&self) -> &AudioCodecParameters {
        &self.native_params
    }

    /// Build a demuxer for a file-like source: probe the container if
    /// no [`ContainerFormat`] hint is provided, otherwise wire the
    /// matching reader directly. Returns a [`SymphoniaDemuxer`] plus the
    /// bootstrap byte-length handle (so the factory can keep updating it
    /// across the decoder's lifetime).
    ///
    /// # Errors
    ///
    /// Surfaces probe-side errors verbatim ([`DecodeError::Backend`])
    /// and missing-track errors ([`DecodeError::ProbeFailed`]).
    pub(crate) fn open_file<R>(
        source: R,
        hint: Option<String>,
        container: Option<ContainerFormat>,
        byte_len_handle: Option<Arc<AtomicU64>>,
    ) -> DecodeResult<(Self, Arc<AtomicU64>)>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let config = SymphoniaConfig {
            byte_len_handle,
            hint,
        };
        let format_opts = FormatOptions::default();
        let bootstrap: ReaderBootstrap = if let Some(container) = container {
            new_direct(source, &config, container, format_opts)?
        } else {
            probe_with_seek(source, &config, format_opts, false)?
        };
        let len_handle = bootstrap.byte_len_handle.clone();
        let demuxer = Self::from_reader(bootstrap.format_reader, Some(bootstrap.byte_pos_handle))?;
        Ok((demuxer, len_handle))
    }

    fn ts_to_duration(&self, ts: Timestamp) -> Duration {
        let Some(tb) = self.time_base else {
            return Duration::ZERO;
        };
        let Some(time) = tb.calc_time(ts) else {
            return Duration::ZERO;
        };
        time_to_duration(time)
    }
}

impl Demuxer for SymphoniaDemuxer {
    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        // Drop the previous packet first so the new one is the only
        // borrow source the returned `Frame<'_>` ties into.
        self.current_packet = None;
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(DemuxOutcome::Eof),
                Err(SymphoniaError::ResetRequired) => continue,
                Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Ok(DemuxOutcome::Eof);
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::Interrupted => {
                    return Ok(DemuxOutcome::Pending(PendingReason::SeekPending));
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let pts = self.ts_to_duration(packet.pts());
            let duration = self.dur_to_duration(packet.dur());
            self.current_packet = Some(packet);
            let data: &[u8] = &self.current_packet.as_ref().expect("just stored").data;
            return Ok(DemuxOutcome::Frame(Frame {
                data,
                duration,
                pts,
            }));
        }
    }

    fn seek(&mut self, target: Duration) -> DecodeResult<DemuxSeekOutcome> {
        let seek_to = SeekTo::Time {
            time: Time::try_new(target.as_secs() as i64, target.subsec_nanos())
                .unwrap_or(Time::ZERO),
            track_id: Some(self.track_id),
        };
        let seeked = self
            .format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| classify_seek_err(&e))?;

        let landed_at = self.ts_to_duration(seeked.actual_ts);

        if let Some(duration) = self.track_info.duration
            && landed_at >= duration
        {
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }

        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte: self.current_byte(),
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

fn build_track_info(track: &Track, codec_params: &AudioCodecParameters) -> DecodeResult<TrackInfo> {
    const DEFAULT_CHANNEL_COUNT: u16 = 2;

    let codec = map_codec_id(codec_params.codec);
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| DecodeError::InvalidData("missing sample rate".into()))?;
    let channels = codec_params
        .channels
        .as_ref()
        .map_or(DEFAULT_CHANNEL_COUNT, |c| {
            u16::try_from(c.count()).unwrap_or(DEFAULT_CHANNEL_COUNT)
        });
    let extra_data = codec_params
        .extra_data
        .as_ref()
        .map(|d| d.to_vec())
        .unwrap_or_default();
    let duration = calculate_track_duration(track);

    Ok(TrackInfo {
        codec,
        duration,
        extra_data,
        channels,
        sample_rate,
    })
}

fn calculate_track_duration(track: &Track) -> Option<Duration> {
    let num_frames = track.num_frames?;
    let time_base = track.time_base?;
    let time = time_base.calc_time(Timestamp::new(
        i64::try_from(num_frames).unwrap_or(i64::MAX),
    ))?;
    Some(time_to_duration(time))
}

fn time_to_duration(time: Time) -> Duration {
    let (seconds, nanos) = time.parts();
    Duration::new(seconds.cast_unsigned(), nanos)
}

/// Map a symphonia codec id to our [`AudioCodec`] enum. Unknown ids fall
/// back to [`AudioCodec::Pcm`] / [`AudioCodec::Adpcm`] when the id sits
/// inside the corresponding well-known range so PCM/ADPCM tracks still
/// surface a usable [`TrackInfo`]. The matching codec wiring uses
/// [`SymphoniaDemuxer::native_params`] for the actual decoder build.
fn map_codec_id(id: AudioCodecId) -> AudioCodec {
    match id {
        CODEC_ID_AAC => AudioCodec::AacLc,
        CODEC_ID_FLAC => AudioCodec::Flac,
        CODEC_ID_MP3 => AudioCodec::Mp3,
        CODEC_ID_ALAC => AudioCodec::Alac,
        CODEC_ID_OPUS => AudioCodec::Opus,
        CODEC_ID_VORBIS => AudioCodec::Vorbis,
        other if is_pcm_codec_id(other) => AudioCodec::Pcm,
        other if is_adpcm_codec_id(other) => AudioCodec::Adpcm,
        // Unknown codec id — surface as PCM so the demuxer still
        // produces a `TrackInfo`. The codec wiring path uses
        // `native_params()` and will reject unsupported ids during
        // `make_audio_decoder`, surfacing the real registry error.
        _ => AudioCodec::Pcm,
    }
}

fn is_pcm_codec_id(id: AudioCodecId) -> bool {
    use symphonia::core::codecs::audio::well_known::{
        CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32BE_PLANAR, CODEC_ID_PCM_F32LE,
        CODEC_ID_PCM_F32LE_PLANAR, CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64BE_PLANAR,
        CODEC_ID_PCM_F64LE, CODEC_ID_PCM_F64LE_PLANAR, CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S8,
        CODEC_ID_PCM_S8_PLANAR, CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16BE_PLANAR, CODEC_ID_PCM_S16LE,
        CODEC_ID_PCM_S16LE_PLANAR, CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S24BE_PLANAR,
        CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S24LE_PLANAR, CODEC_ID_PCM_S32BE,
        CODEC_ID_PCM_S32BE_PLANAR, CODEC_ID_PCM_S32LE, CODEC_ID_PCM_S32LE_PLANAR, CODEC_ID_PCM_U8,
        CODEC_ID_PCM_U8_PLANAR, CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U16BE_PLANAR, CODEC_ID_PCM_U16LE,
        CODEC_ID_PCM_U16LE_PLANAR, CODEC_ID_PCM_U24BE, CODEC_ID_PCM_U24BE_PLANAR,
        CODEC_ID_PCM_U24LE, CODEC_ID_PCM_U24LE_PLANAR, CODEC_ID_PCM_U32BE,
        CODEC_ID_PCM_U32BE_PLANAR, CODEC_ID_PCM_U32LE, CODEC_ID_PCM_U32LE_PLANAR,
    };
    matches!(
        id,
        CODEC_ID_PCM_S32LE
            | CODEC_ID_PCM_S32LE_PLANAR
            | CODEC_ID_PCM_S32BE
            | CODEC_ID_PCM_S32BE_PLANAR
            | CODEC_ID_PCM_S24LE
            | CODEC_ID_PCM_S24LE_PLANAR
            | CODEC_ID_PCM_S24BE
            | CODEC_ID_PCM_S24BE_PLANAR
            | CODEC_ID_PCM_S16LE
            | CODEC_ID_PCM_S16LE_PLANAR
            | CODEC_ID_PCM_S16BE
            | CODEC_ID_PCM_S16BE_PLANAR
            | CODEC_ID_PCM_S8
            | CODEC_ID_PCM_S8_PLANAR
            | CODEC_ID_PCM_U32LE
            | CODEC_ID_PCM_U32LE_PLANAR
            | CODEC_ID_PCM_U32BE
            | CODEC_ID_PCM_U32BE_PLANAR
            | CODEC_ID_PCM_U24LE
            | CODEC_ID_PCM_U24LE_PLANAR
            | CODEC_ID_PCM_U24BE
            | CODEC_ID_PCM_U24BE_PLANAR
            | CODEC_ID_PCM_U16LE
            | CODEC_ID_PCM_U16LE_PLANAR
            | CODEC_ID_PCM_U16BE
            | CODEC_ID_PCM_U16BE_PLANAR
            | CODEC_ID_PCM_U8
            | CODEC_ID_PCM_U8_PLANAR
            | CODEC_ID_PCM_F32LE
            | CODEC_ID_PCM_F32LE_PLANAR
            | CODEC_ID_PCM_F32BE
            | CODEC_ID_PCM_F32BE_PLANAR
            | CODEC_ID_PCM_F64LE
            | CODEC_ID_PCM_F64LE_PLANAR
            | CODEC_ID_PCM_F64BE
            | CODEC_ID_PCM_F64BE_PLANAR
            | CODEC_ID_PCM_ALAW
            | CODEC_ID_PCM_MULAW
    )
}

fn is_adpcm_codec_id(id: AudioCodecId) -> bool {
    use symphonia::core::codecs::audio::well_known::{
        CODEC_ID_ADPCM_G722, CODEC_ID_ADPCM_G726, CODEC_ID_ADPCM_G726LE, CODEC_ID_ADPCM_IMA_QT,
        CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ADPCM_MS,
    };
    matches!(
        id,
        CODEC_ID_ADPCM_G722
            | CODEC_ID_ADPCM_G726
            | CODEC_ID_ADPCM_G726LE
            | CODEC_ID_ADPCM_MS
            | CODEC_ID_ADPCM_IMA_WAV
            | CODEC_ID_ADPCM_IMA_QT
    )
}

fn classify_seek_err(err: &SymphoniaError) -> DecodeError {
    match err {
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => {
            DecodeError::SeekOutOfRange(err.to_string())
        }
        SymphoniaError::IoError(io_err)
            if io_err.get_ref().is_some_and(
                <dyn std::error::Error + Send + Sync + 'static>::is::<StreamSeekPastEof>,
            ) =>
        {
            DecodeError::SeekOutOfRange(io_err.to_string())
        }
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::SeekOutOfRange(err.to_string())
        }
        SymphoniaError::IoError(io_err)
            if io_err.kind() == ErrorKind::Interrupted
                || io_err.get_ref().is_some_and(|src| {
                    src.downcast_ref::<PendingReason>()
                        .is_some_and(|reason| matches!(reason, PendingReason::SeekPending))
                }) =>
        {
            DecodeError::Interrupted
        }
        _ => DecodeError::SeekFailed(err.to_string()),
    }
}
