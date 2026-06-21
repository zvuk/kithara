use std::{
    io::{ErrorKind, Read, Seek},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_platform::time::Duration;
use kithara_stream::{
    AudioCodec, ContainerFormat, NotReadyCause, PendingReason, PrerollHint, StreamPending,
    StreamSeekPastEof,
};
use kithara_test_utils::kithara;
use symphonia::core::{
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
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType},
    packet::Packet,
    units::{Time, TimeBase, Timestamp},
};

use crate::{
    codec::CodecPriming,
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
    byte_map: Option<Arc<dyn kithara_stream::ByteMap>>,
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
    current_packet: Option<Packet>,
    /// Time base used to translate packet timestamps into wall-clock
    /// [`std::time::Duration`].
    time_base: Option<TimeBase>,
    track_info: TrackInfo,
    /// Set when a `next_frame` read was interrupted at a not-ready segment
    /// boundary. Symphonia's `MediaSourceStream` may have consumed
    /// ring-buffered bytes into a packet that was then discarded (its read
    /// position advanced), stranding those bytes. The next `next_frame`
    /// re-seeks the reader back to `resume_ts` before reading so the
    /// interrupted packet is re-read from its start instead of skipped.
    needs_resume: bool,
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
            format_reader,
            track_id,
            track_info,
            native_params,
            time_base,
            byte_pos_handle,
            byte_map,
            current_packet: None,
            resume_ts: 0,
            needs_resume: false,
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
        let config = SymphoniaConfig {
            byte_len_handle,
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

    #[kithara::probe]
    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.current_packet = None;
        // A previous read stranded bytes inside `MediaSourceStream` at a
        // not-ready boundary (it consumed ring bytes into a packet that was
        // then discarded, advancing its read position). Re-seek to the last
        // authoritative timestamp so the stranded packet is re-read from its
        // start instead of being skipped (CONTEXT.md "Read-ahead strand").
        if self.needs_resume {
            self.needs_resume = false;
            self.reseek_to_resume()?;
        }
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(DemuxOutcome::Eof),
                Err(SymphoniaError::ResetRequired) => continue,
                Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Ok(DemuxOutcome::Eof);
                }
                Err(SymphoniaError::IoError(e))
                    if e.kind() == ErrorKind::Interrupted || e.kind() == ErrorKind::WouldBlock =>
                {
                    let reason = e
                        .get_ref()
                        .and_then(|src| src.downcast_ref::<StreamPending>())
                        .map(|p| p.reason)
                        .or_else(|| {
                            e.get_ref()
                                .and_then(|src| src.downcast_ref::<PendingReason>())
                                .copied()
                        })
                        .unwrap_or(PendingReason::NotReady(NotReadyCause::SourcePending));
                    // A `MediaSourceStream` read interrupted at a not-ready
                    // boundary can strand bytes it already consumed from its
                    // ring (read position advanced, no packet emitted). The
                    // adapter's byte cursor doesn't reveal this — the consumed
                    // bytes were buffered by an earlier read-ahead. Flag an
                    // unconditional resume: the next call re-seeks to
                    // `resume_ts` so the interrupted packet is re-read from
                    // its start. Idempotent when no strand occurred (the read
                    // position already sits at `resume_ts`).
                    self.needs_resume = true;
                    return Ok(DemuxOutcome::Pending(reason));
                }
                Err(e) => return Err(DecodeError::backend(e)),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            // This packet was emitted cleanly; the next one must start at
            // its end. Track the resume point in native timebase units so a
            // strand re-seek round-trips exactly to this boundary.
            self.resume_ts = packet
                .pts
                .get()
                .saturating_add(i64::try_from(packet.dur.get()).unwrap_or(i64::MAX));
            self.needs_resume = false;
            let pts = self.ts_to_duration(packet.pts);
            let duration = self.dur_to_duration(packet.dur);
            self.current_packet = Some(packet);
            let data: &[u8] = &self.current_packet.as_ref().expect("BUG: just stored").data;
            return Ok(DemuxOutcome::Frame(Frame {
                data,
                duration,
                pts,
                packet_desc: &[],
            }));
        }
    }

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        // WHY: park before target by max(priming warmup, one codec packet) so the trim guard lands on a packet boundary (CONTEXT.md "Seek pre-roll and trim").
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

        // A fresh seek defines the authoritative resume point and clears any
        // pending strand recovery left over from the prior read position.
        self.resume_ts = seeked.actual_ts.get();
        self.needs_resume = false;

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

impl SymphoniaDemuxer {
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
    fn reseek_to_resume(&mut self) -> DecodeResult<()> {
        let seek_to = SeekTo::Timestamp {
            ts: Timestamp::new(self.resume_ts),
            track_id: self.track_id,
        };
        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| classify_seek_err(&e))?;
        Ok(())
    }

    /// Override the `track.gapless` slot built by `from_reader` with a
    /// container-level probe result.
    ///
    /// `build_track_info` cannot probe the source itself — by the time it
    /// runs, the format reader has already consumed the relevant bytes.
    /// The factory layer probes the underlying source separately
    /// (e.g. `probe_mp4_gapless` for AAC fMP4) and pipes the result
    /// through this setter so the codec sees the captured trim counts.
    pub(crate) fn set_gapless(&mut self, gapless: Option<crate::GaplessInfo>) {
        self.track_info.gapless = gapless;
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
        _ => AudioCodec::Pcm,
    }
}

fn is_pcm_codec_id(id: AudioCodecId) -> bool {
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

fn mdct_packet_frames(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Mp3 => 1152,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => 1024,
        _ => 0,
    }
}
