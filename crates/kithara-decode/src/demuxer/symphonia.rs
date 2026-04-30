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

use kithara_stream::{AudioCodec, ContainerFormat, PendingReason};
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{
            AudioCodecId,
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
pub struct SymphoniaDemuxer {
    format_reader: Box<dyn FormatReader>,
    track_id: u32,
    track_info: TrackInfo,
    /// Time base used to translate packet timestamps into wall-clock
    /// [`std::time::Duration`].
    time_base: Option<TimeBase>,
    /// Live byte cursor of the underlying media source. Populated by
    /// the [`super::super::symphonia::adapter::ReadSeekAdapter`] when
    /// the demuxer is built through that path; absent for synthetic
    /// readers in unit tests.
    byte_pos_handle: Option<Arc<AtomicU64>>,
}

impl SymphoniaDemuxer {
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
    pub fn open_file<R>(
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
            container,
            hint,
            ..Default::default()
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
    pub fn from_reader(
        format_reader: Box<dyn FormatReader>,
        byte_pos_handle: Option<Arc<AtomicU64>>,
    ) -> DecodeResult<Self> {
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::ProbeFailed)?
            .clone();
        let track_id = track.id;
        let track_info = build_track_info(&track)?;
        let time_base = track.time_base;
        Ok(Self {
            format_reader,
            track_id,
            track_info,
            time_base,
            byte_pos_handle,
        })
    }

    fn current_byte(&self) -> Option<u64> {
        self.byte_pos_handle
            .as_ref()
            .map(|h| h.load(std::sync::atomic::Ordering::Acquire))
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

    fn dur_to_duration(&self, dur: symphonia::core::units::Duration) -> Duration {
        let Some(tb) = self.time_base else {
            return Duration::ZERO;
        };
        let ts = Timestamp::new(i64::try_from(dur.get()).unwrap_or(i64::MAX));
        tb.calc_time(ts).map_or(Duration::ZERO, time_to_duration)
    }
}

impl Demuxer for SymphoniaDemuxer {
    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }

    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome> {
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
            return Ok(DemuxOutcome::Frame(Frame {
                data: packet.data.to_vec(),
                pts,
                duration,
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
}

fn build_track_info(track: &Track) -> DecodeResult<TrackInfo> {
    const DEFAULT_CHANNEL_COUNT: u16 = 2;

    let Some(CodecParameters::Audio(codec_params)) = &track.codec_params else {
        return Err(DecodeError::ProbeFailed);
    };

    let codec = map_codec_id(codec_params.codec)?;
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
    let timescale = track.time_base.map_or(sample_rate, |tb| tb.denom.get());

    Ok(TrackInfo {
        codec,
        sample_rate,
        channels,
        timescale,
        extra_data,
        duration,
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

fn map_codec_id(id: AudioCodecId) -> DecodeResult<AudioCodec> {
    Ok(match id {
        CODEC_ID_AAC => AudioCodec::AacLc,
        CODEC_ID_FLAC => AudioCodec::Flac,
        CODEC_ID_MP3 => AudioCodec::Mp3,
        CODEC_ID_ALAC => AudioCodec::Alac,
        CODEC_ID_OPUS => AudioCodec::Opus,
        CODEC_ID_VORBIS => AudioCodec::Vorbis,
        other => {
            return Err(DecodeError::InvalidData(format!(
                "unsupported symphonia codec id: {other:?}"
            )));
        }
    })
}

fn classify_seek_err(err: &SymphoniaError) -> DecodeError {
    match err {
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => {
            DecodeError::SeekOutOfRange(err.to_string())
        }
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::SeekOutOfRange(err.to_string())
        }
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::Interrupted => {
            DecodeError::Interrupted
        }
        _ => DecodeError::SeekFailed(err.to_string()),
    }
}
