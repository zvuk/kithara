#[cfg(feature = "symphonia")]
use std::io::{Read, Seek};
use std::{
    io::ErrorKind,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};
#[cfg(feature = "symphonia")]
use kithara_stream::ContainerFormat;
use kithara_stream::{
    AudioCodec, NotReadyCause, PendingReason, PrerollHint, StreamPending, StreamSeekPastEof,
};
use kithara_test_utils::kithara;
#[cfg(feature = "symphonia")]
use symphonia_core::formats::FormatOptions;
#[cfg(all(test, feature = "symphonia"))]
use symphonia_core::packet::Packet;
use symphonia_core::{
    codecs::{
        CodecParameters,
        audio::{
            AudioCodecId, AudioCodecParameters,
            well_known::{
                CODEC_ID_AAC, CODEC_ID_ADPCM_G722, CODEC_ID_ADPCM_G726, CODEC_ID_ADPCM_G726LE,
                CODEC_ID_ADPCM_IMA_QT, CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ADPCM_MS, CODEC_ID_ALAC,
                CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32BE,
                CODEC_ID_PCM_F32BE_PLANAR, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F32LE_PLANAR,
                CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64BE_PLANAR, CODEC_ID_PCM_F64LE,
                CODEC_ID_PCM_F64LE_PLANAR, CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S8,
                CODEC_ID_PCM_S8_PLANAR, CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16BE_PLANAR,
                CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S16LE_PLANAR, CODEC_ID_PCM_S24BE,
                CODEC_ID_PCM_S24BE_PLANAR, CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S24LE_PLANAR,
                CODEC_ID_PCM_S32BE, CODEC_ID_PCM_S32BE_PLANAR, CODEC_ID_PCM_S32LE,
                CODEC_ID_PCM_S32LE_PLANAR, CODEC_ID_PCM_U8, CODEC_ID_PCM_U8_PLANAR,
                CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U16BE_PLANAR, CODEC_ID_PCM_U16LE,
                CODEC_ID_PCM_U16LE_PLANAR, CODEC_ID_PCM_U24BE, CODEC_ID_PCM_U24BE_PLANAR,
                CODEC_ID_PCM_U24LE, CODEC_ID_PCM_U24LE_PLANAR, CODEC_ID_PCM_U32BE,
                CODEC_ID_PCM_U32BE_PLANAR, CODEC_ID_PCM_U32LE, CODEC_ID_PCM_U32LE_PLANAR,
                CODEC_ID_VORBIS,
            },
        },
    },
    errors::{Error as SymphoniaError, SeekErrorKind},
    formats::{FormatReader, SeekMode, SeekTo, Track, TrackType},
    units::{Duration as SymphoniaDuration, Time, TimeBase, Timestamp},
};

#[cfg(feature = "symphonia")]
use crate::symphonia::{
    config::SymphoniaConfig,
    probe::{ReaderBootstrap, new_direct, probe_with_seek},
};
use crate::{
    codec::CodecPriming,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PreparedPacket, TrackInfo},
    error::{DecodeError, DecodeResult},
    symphonia::packets::Packets,
};

/// Demuxer adapter over Symphonia's [`FormatReader`].
pub(crate) struct SymphoniaDemuxer {
    /// Native Symphonia codec parameters for the audio track. Carried so
    /// the matching [`SymphoniaCodec::open_native`] path can build a
    /// decoder for codecs whose generic [`AudioCodec`] enum representation
    /// loses information (PCM bit-depth/endianness, ADPCM dialect).
    #[cfg(feature = "symphonia")]
    pub(crate) native_params: AudioCodecParameters,
    format_reader: Packets,
    prepared: Option<PreparedPacket>,
    byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
    /// Live byte cursor of the underlying media source. Populated by
    /// the [`super::super::symphonia::adapter::ReadSeekAdapter`] when
    /// the demuxer is built through that path; absent for synthetic
    /// readers in unit tests.
    byte_pos_handle: Option<Arc<AtomicU64>>,
    /// Pending reason retained while recovering an interrupted packet read.
    /// Symphonia's `MediaSourceStream` may have consumed ring-buffered bytes
    /// into a packet that was then discarded (its read position advanced),
    /// stranding those bytes. The next `next_frame` re-seeks the reader back
    /// to `resume_ts`; if that seek also pends, the reason stays armed until
    /// recovery succeeds.
    resume_pending: Option<PendingReason>,
    /// Time base used to translate packet timestamps into wall-clock
    /// [`std::time::Duration`].
    time_base: Option<TimeBase>,
    track_info: TrackInfo,
    /// Native (timebase-unit) timestamp the *next* packet must start at.
    /// Authoritative across a `Pending`: set to the seek's `actual_ts` on
    /// seek and advanced to `pts + dur` after each successfully-returned
    /// frame. Kept in native units (not `Duration`) so the resume re-seek
    /// round-trips exactly to a packet boundary — a `Duration` conversion
    /// loses sub-frame precision and snaps one packet early. Used to undo a
    /// read-ahead strand (see `next_frame` / `reseek_to_resume`).
    resume_ts: i64,
    track_id: u32,
}

/// Inputs to [`SymphoniaDemuxer::open_file`] besides the reader: the
/// format `hint` (file extension), an explicit `container` format that
/// skips probing when known, the bootstrap `byte_len_handle`, and an
/// optional `byte_map` over the underlying source.
#[cfg(feature = "symphonia")]
pub(crate) struct FileOpen {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) hint: Option<String>,
}

impl SymphoniaDemuxer {
    fn current_byte(&self) -> Option<u64> {
        self.byte_pos_handle
            .as_ref()
            .map(|h| h.load(Ordering::Acquire))
    }

    fn dur_to_duration(&self, dur: SymphoniaDuration) -> Duration {
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
    pub(crate) fn from_reader_with_layout(
        format_reader: Box<dyn FormatReader>,
        byte_pos_handle: Option<Arc<AtomicU64>>,
        byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
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
            format_reader: Packets::new(format_reader),
            track_id,
            track_info,
            #[cfg(feature = "symphonia")]
            native_params,
            time_base,
            byte_pos_handle,
            byte_map,
            resume_ts: 0,
            resume_pending: None,
            prepared: None,
        })
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
    #[cfg(feature = "symphonia")]
    pub(crate) fn open_file<R>(source: R, open: FileOpen) -> DecodeResult<(Self, Arc<AtomicU64>)>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let FileOpen {
            hint,
            container,
            byte_len_handle,
            byte_map,
        } = open;
        let config = SymphoniaConfig::builder()
            .maybe_byte_len_handle(byte_len_handle)
            .maybe_hint(hint)
            .build();
        let format_opts = FormatOptions::default();
        let bootstrap: ReaderBootstrap = if let Some(container) = container {
            new_direct(source, &config, container, format_opts)?
        } else {
            probe_with_seek(source, &config, format_opts, false)?
        };
        let len_handle = bootstrap.byte_len_handle.clone();
        let demuxer = Self::from_reader_with_layout(
            bootstrap.format_reader,
            Some(bootstrap.byte_pos_handle),
            byte_map,
        )?;
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
    fn current_segment_index(&self) -> Option<u32> {
        let byte = self.current_byte()?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte.saturating_sub(1))
            .map(|d| d.segment_index)
    }

    fn current_variant_index(&self) -> Option<usize> {
        let byte = self.current_byte()?;
        self.byte_map
            .as_ref()?
            .segment_at_byte(byte.saturating_sub(1))
            .map(|d| d.variant_index)
    }

    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn prepare_frame(&mut self) -> DecodeResult<()> {
        if self.prepared.is_none() {
            self.prepared = Some(self.read_frame()?.into());
        }
        Ok(())
    }

    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        Ok(match self.prepared.take() {
            Some(PreparedPacket::Frame { pts, duration }) => DemuxOutcome::Frame(Frame {
                data: self.format_reader.data(),
                packet_desc: &[],
                pts,
                duration,
            }),
            Some(PreparedPacket::Pending(reason)) => DemuxOutcome::Pending(reason),
            Some(PreparedPacket::Eof) => DemuxOutcome::Eof,
            None => DemuxOutcome::Pending(PendingReason::Retry),
        })
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.prepared = None;
        // WHY: park before target by max(priming warmup, one codec packet) so the trim
        // guard lands on a packet boundary.
        let sr = f64::from(self.track_info.sample_rate.max(1));
        let priming_secs = f64::from(u32::try_from(priming.frames).unwrap_or(u32::MAX)) / sr;
        let packet_secs = f64::from(mdct_packet_frames(self.track_info.codec)) / sr;
        let backup_duration = Duration::from_secs_f64(priming_secs.max(packet_secs));
        let effective_target = target.saturating_sub(backup_duration);
        let seek_to = SeekTo::Time {
            time: Time::try_new(
                effective_target.as_secs() as i64,
                effective_target.subsec_nanos(),
            )
            .unwrap_or(Time::ZERO),
            track_id: Some(self.track_id),
        };
        let seeked = self
            .format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| classify_seek_err(&e))?;

        let landed_at = self.ts_to_duration(seeked.actual_ts);

        // WHY: A fresh seek defines the authoritative resume point and clears any pending strand recovery left over from the prior read
        // position.
        self.resume_ts = seeked.actual_ts.get();
        self.resume_pending = None;

        if let Some(duration) = self.track_info.duration
            && landed_at >= duration
        {
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }

        let landed_byte = self.current_byte();
        let preroll = match landed_byte {
            Some(lb) if priming.byte_margin > 0 => {
                PrerollHint::Required(lb.saturating_sub(priming.byte_margin))
            }
            _ => PrerollHint::NotNeeded,
        };
        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte,
            preroll,
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

fn packet_ends_at_or_before(pts: Timestamp, dur: SymphoniaDuration, timestamp: i64) -> bool {
    pts.get()
        .saturating_add(i64::try_from(dur.get()).unwrap_or(i64::MAX))
        <= timestamp
}

impl SymphoniaDemuxer {
    #[kithara::probe]
    #[kithara::measure(label = "decode.symphonia.demux")]
    fn read_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        // WHY: A previous read stranded bytes inside `MediaSourceStream` at a not-ready boundary (it consumed ring bytes into a packet that
        // was then discarded, advancing its read position).
        let resume_floor = if let Some(reason) = self.resume_pending {
            match self.reseek_to_resume() {
                Ok(()) => {
                    self.resume_pending = None;
                    Some(self.resume_ts)
                }
                Err(error) => {
                    if pending_reason(&error).is_some() {
                        return Ok(DemuxOutcome::Pending(reason));
                    }
                    let failure = classify_seek_err(&error);
                    if resume_point_is_past_the_end(&failure) {
                        return Ok(DemuxOutcome::Eof);
                    }
                    return Err(failure);
                }
            }
        } else {
            None
        };
        loop {
            let packet = match self.format_reader.read() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(DemuxOutcome::Eof),
                Err(SymphoniaError::ResetRequired) => continue,
                Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Ok(DemuxOutcome::Eof);
                }
                Err(error) => {
                    let Some(reason) = pending_reason(&error) else {
                        return Err(DecodeError::backend(error));
                    };
                    // WHY: A `MediaSourceStream` read interrupted at a not-ready boundary can strand bytes it already consumed from its ring (read
                    // position advanced, no packet emitted).
                    if !self.format_reader.restores_interrupted_packet() {
                        self.resume_pending = Some(reason);
                    }
                    return Ok(DemuxOutcome::Pending(reason));
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            if resume_floor
                .is_some_and(|floor| packet_ends_at_or_before(packet.pts, packet.dur, floor))
            {
                continue;
            }
            // WHY: This packet was emitted cleanly; the next one must start at its end. Track the resume point in native timebase units so a
            // strand re-seek round-trips exactly to this boundary.
            self.resume_ts = packet
                .pts
                .get()
                .saturating_add(i64::try_from(packet.dur.get()).unwrap_or(i64::MAX));
            self.resume_pending = None;
            let pts = self.ts_to_duration(packet.pts);
            let duration = self.dur_to_duration(packet.dur);
            let data = self.format_reader.data();
            return Ok(DemuxOutcome::Frame(Frame {
                data,
                duration,
                pts,
                packet_desc: &[],
            }));
        }
    }

    /// Re-seek the reader back to the last authoritative timestamp
    /// (`resume_ts`) after a read-ahead strand. Unlike [`Demuxer::seek`]
    /// this applies no pre-roll back-off and no codec flush — it is a
    /// position restore, not a user seek: the goal is to re-read the exact
    /// packet whose in-flight read was interrupted at a not-ready boundary
    /// so its bytes are re-delivered rather than skipped. `Accurate` mode
    /// lands at or before `resume_ts`; for the packet-quantised readers
    /// that strand (WAV/PCM) the landing is the same packet boundary, so no
    /// audio is re-emitted twice (the decoder's leading-trim guard already
    /// gates on the seek target).
    fn reseek_to_resume(&mut self) -> Result<(), SymphoniaError> {
        let seek_to = SeekTo::Timestamp {
            ts: Timestamp::new(self.resume_ts),
            track_id: self.track_id,
        };
        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map(|_| ())
    }

    /// Set encoder trim obtained from container metadata.
    pub(crate) const fn set_gapless(&mut self, gapless: Option<crate::GaplessInfo>) {
        self.track_info.gapless = gapless;
    }
}

fn build_track_info(track: &Track, codec_params: &AudioCodecParameters) -> DecodeResult<TrackInfo> {
    const DEFAULT_CHANNEL_COUNT: u16 = 2;

    let codec = map_codec_id(codec_params.codec);
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| DecodeError::InvalidData {
            detail: "missing sample rate",
        })?;
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
        gapless: None,
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
const fn map_codec_id(id: AudioCodecId) -> AudioCodec {
    match id {
        CODEC_ID_AAC => AudioCodec::AacLc,
        CODEC_ID_FLAC => AudioCodec::Flac,
        CODEC_ID_MP3 => AudioCodec::Mp3,
        CODEC_ID_ALAC => AudioCodec::Alac,
        CODEC_ID_OPUS => AudioCodec::Opus,
        CODEC_ID_VORBIS => AudioCodec::Vorbis,
        other if is_pcm_codec_id(other) => AudioCodec::Pcm,
        other if is_adpcm_codec_id(other) => AudioCodec::Adpcm,
        _ => AudioCodec::Pcm,
    }
}

const fn is_pcm_codec_id(id: AudioCodecId) -> bool {
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

const fn is_adpcm_codec_id(id: AudioCodecId) -> bool {
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
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => DecodeError::SeekOutOfRange {
            detail: "seek target past indexed sample range",
        },
        SymphoniaError::IoError(io_err)
            if io_err.get_ref().is_some_and(
                <dyn std::error::Error + Send + Sync + 'static>::is::<StreamSeekPastEof>,
            ) =>
        {
            DecodeError::SeekOutOfRange {
                detail: "seek past end of stream",
            }
        }
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::SeekOutOfRange {
                detail: "seek hit unexpected end of stream",
            }
        }
        SymphoniaError::IoError(io_err)
            if matches!(
                io_err.kind(),
                ErrorKind::Interrupted | ErrorKind::WouldBlock
            ) || io_err.get_ref().is_some_and(|src| {
                src.downcast_ref::<PendingReason>()
                    .is_some_and(|reason| matches!(reason, PendingReason::SeekPending))
            }) =>
        {
            // WHY: The typed payload (`StreamPending`: pos/phase/epoch/flushing) dies here - `Interrupted` is a unit variant, and the seek
            // recovery above can only log the name.
            tracing::debug!(error = ?io_err, "demuxer seek interrupted");
            DecodeError::Interrupted
        }
        _ => DecodeError::SeekFailed {
            detail: "symphonia seek failed",
        },
    }
}

/// Whether a failed resume re-seek means the source has nothing left to read.
///
/// `resume_ts` is the end of the last cleanly emitted packet, and a
/// packet-quantised reader reports a full packet duration even for a
/// truncated final packet — so once the last frame is out, the resume point
/// can sit past the end of the source. A reader publishes a length only once
/// every segment size is exact, so "past the published end" is a final
/// answer rather than a not-ready boundary: there is no stranded packet to
/// re-read and the stream ends, the way [`Demuxer::seek`] reports
/// `PastEof` instead of failing.
const fn resume_point_is_past_the_end(failure: &DecodeError) -> bool {
    matches!(failure, DecodeError::SeekOutOfRange { .. })
}

fn pending_reason(error: &SymphoniaError) -> Option<PendingReason> {
    let SymphoniaError::IoError(error) = error else {
        return None;
    };
    if !matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) {
        return None;
    }
    Some(
        error
            .get_ref()
            .and_then(|source| {
                source
                    .downcast_ref::<StreamPending>()
                    .map(StreamPending::reason)
                    .or_else(|| source.downcast_ref::<PendingReason>().copied())
            })
            .unwrap_or(PendingReason::NotReady(NotReadyCause::SourcePending)),
    )
}

const fn mdct_packet_frames(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Mp3 => 1152,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => 1024,
        _ => 0,
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod tests {
    use std::io::{self, ErrorKind, Read, Seek, SeekFrom};

    use kithara_stream::{NotReadyCause, PendingReason, StreamSeekPastEof};
    use kithara_test_fixtures::fixtures::tone_wav;
    use kithara_test_utils::kithara;
    use symphonia::{
        core::{
            errors::Error as SymphoniaError,
            formats::{FormatOptions, probe::Hint},
            io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
            units::{Duration as SymphoniaDuration, Timestamp},
        },
        default,
    };

    use super::{
        DemuxOutcome, Demuxer, Packet, SymphoniaDemuxer, packet_ends_at_or_before, pending_reason,
    };

    #[kithara::test(native, flash(false))]
    fn resume_floor_rejects_the_packet_before_the_authoritative_timestamp() {
        let packet = Packet::new(
            0,
            Timestamp::new(1_000),
            SymphoniaDuration::new(1_152),
            Vec::new(),
        );

        assert!(packet_ends_at_or_before(packet.pts, packet.dur, 2_152));
        assert!(!packet_ends_at_or_before(packet.pts, packet.dur, 2_151));
    }

    /// Tail withheld from the WAV fixture so the source publishes a length
    /// shorter than the header's frame count, the way a variant publishes
    /// only the segments whose sizes are already exact.
    const WITHHELD_TAIL_BYTES: usize = 4 * 1024;

    /// Source modelling a reader that publishes an exact length and refuses
    /// any seek beyond it, the way the HLS session reader does once every
    /// segment size is known.
    struct PublishedEndSource {
        bytes: Vec<u8>,
        pos: u64,
    }

    impl PublishedEndSource {
        fn new(bytes: Vec<u8>) -> Self {
            Self { bytes, pos: 0 }
        }

        fn len(&self) -> u64 {
            u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
        }
    }

    impl Read for PublishedEndSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let start = usize::try_from(self.pos)
                .unwrap_or(usize::MAX)
                .min(self.bytes.len());
            let end = start.saturating_add(buf.len()).min(self.bytes.len());
            buf[..end - start].copy_from_slice(&self.bytes[start..end]);
            self.pos = u64::try_from(end).unwrap_or(u64::MAX);
            Ok(end - start)
        }
    }

    impl Seek for PublishedEndSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let len = self.len();
            let target = match pos {
                SeekFrom::Start(offset) => i128::from(offset),
                SeekFrom::Current(delta) => i128::from(self.pos).saturating_add(i128::from(delta)),
                SeekFrom::End(delta) => i128::from(len).saturating_add(i128::from(delta)),
            };
            let target = u64::try_from(target).unwrap_or(u64::MAX);
            if target > len {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    StreamSeekPastEof::new(self.pos, len, target),
                ));
            }
            self.pos = target;
            Ok(target)
        }
    }

    impl MediaSource for PublishedEndSource {
        fn byte_len(&self) -> Option<u64> {
            Some(self.len())
        }

        fn is_seekable(&self) -> bool {
            true
        }
    }

    fn wav_demuxer(tone_wav: &[u8]) -> SymphoniaDemuxer {
        let mut bytes = tone_wav.to_vec();
        bytes.truncate(bytes.len() - WITHHELD_TAIL_BYTES);
        let source = PublishedEndSource::new(bytes);
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("WAV fixture must probe");
        SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("WAV demuxer must build")
    }

    fn track_frames(demuxer: &SymphoniaDemuxer) -> i64 {
        let info = demuxer.track_info();
        let millis = i64::try_from(
            info.duration
                .expect("WAV fixture must publish a duration")
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        millis * i64::from(info.sample_rate) / 1000
    }

    #[kithara::test]
    fn a_resume_point_past_the_published_end_ends_the_stream(tone_wav: &'static [u8]) {
        let mut demuxer = wav_demuxer(tone_wav);
        demuxer.resume_ts = track_frames(&demuxer).saturating_sub(1);
        demuxer.resume_pending = Some(PendingReason::NotReady(NotReadyCause::SourcePending));

        match demuxer.next_frame() {
            Ok(DemuxOutcome::Eof) => {}
            Ok(_) => panic!("a resume point past the published end must end the stream"),
            Err(error) => panic!("a resume point past the published end must not fail: {error}"),
        }
    }

    #[kithara::test(native, flash(false))]
    fn would_block_uses_source_pending_reason() {
        let error = SymphoniaError::IoError(io::Error::from(ErrorKind::WouldBlock));

        assert_eq!(
            pending_reason(&error),
            Some(PendingReason::NotReady(NotReadyCause::SourcePending))
        );
    }
}
