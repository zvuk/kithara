use std::sync::atomic::{AtomicU64, Ordering};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{
    AudioCodec, BoxedEventSink, NotReadyCause, PendingReason, ReaderChunkSignal, ReaderSeekSignal,
};
use kithara_test_utils::kithara;

use crate::{
    BlenderProfile,
    codec::FrameCodec,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    error::{DecodeError, DecodeResult},
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::TrackMetadata,
};

/// Frames the decoder drops from the head of its decoded signal, counted as they are
/// dropped rather than declared up front.
///
/// `supplied` comes from the packet's own duration converted at the output
/// sample rate, so it is already in the domain the decoder emits in: an
/// SBR codec whose access unit is 1024 core frames but 2048 output frames
/// is measured against 2048, not against a core-rate constant.
#[derive(Clone, Copy, Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
struct HeadStrip {
    settled: bool,
    #[field(get, copy)]
    frames: u64,
}

impl HeadStrip {
    /// Stops once a packet comes back whole: from there on the decoder is
    /// through its delay, and a short packet is the stream's own tail
    /// rather than more strip.
    const fn record(&mut self, supplied: u64, emitted: u64) {
        if self.settled || supplied == 0 {
            return;
        }
        if emitted >= supplied {
            self.settled = true;
            return;
        }
        self.frames = self.frames.saturating_add(supplied - emitted);
    }
}

const ZERO_FRAME_BUDGET: u32 = 32;

/// Generic decoder built by composition: a [`Demuxer`] feeds raw frames
/// into a [`FrameCodec`] which produces PCM. One implementation, one
/// dispatch path — no per-backend duplication.
pub(crate) struct ComposedDecoder<D: Demuxer, C: FrameCodec, S> {
    spec: AudioSpec,
    codec: C,
    demuxer: D,
    head_strip: HeadStrip,
    byte_len_handle: Option<Arc<AtomicU64>>,
    duration: Option<Duration>,
    /// Reader-side event sink. Single-owner `Box<dyn ReaderEventSink>` —
    /// `None` skips emission entirely; `Some(_)` calls `on_chunk` /
    /// `on_seek` directly via `&mut` after the inner outcome resolves.
    /// No lock on the produce-core. Folded in from the former
    /// `HookedDecoder` decorator — every decoder is hookable now.
    hooks: Option<BoxedEventSink>,
    /// When `Some`, frames whose decode-time end is `<= target` are
    /// dropped before the next chunk is emitted. Cleared after the
    /// first frame past the target is consumed. Lets `seek(target)`
    /// land precisely at `target` instead of at the granule boundary.
    pending_seek_target: Option<Duration>,
    pools: PoolRegion<S>,
    output: Option<DecodeResult<SampleBuffer>>,
    prefill: Prefill,
    /// Set on every seek; the next emitted chunk may re-anchor the PCM cursor.
    resync_frame_offset_to_pts: bool,
    zero_frame_count: u32,
    epoch: u64,
    /// Cumulative frame counter. Anchored to `landed_at` on seek (so the
    /// next chunk's `frame_offset / sample_rate ≈ timestamp`) and
    /// incremented by `decoded.frames` per emitted chunk. Tracking it
    /// cumulatively avoids the precision loss that sneaks in if every
    /// chunk recomputes `floor(pts * sample_rate)` from a `Duration`
    /// nanosecond value.
    frame_offset: u64,
    timeline_gap_frames: u64,
}

/// Runtime wiring required to build a [`ComposedDecoder`].
///
/// The host must provide its configured pool region explicitly.
pub(crate) struct DecoderRuntime<S> {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) hooks: Option<BoxedEventSink>,
    pub(crate) pools: PoolRegion<S>,
    pub(crate) epoch: u64,
}

enum Prefill {
    Needed,
    Buffered(DecodeResult<DecoderChunkOutcome>),
    Complete,
}

impl<D, C, S> ComposedDecoder<D, C, S>
where
    D: Demuxer,
    C: FrameCodec,
    S: HasPool<f32>,
{
    /// Build a decoder from a `(demuxer, codec)` pair and its runtime wiring.
    pub(crate) fn new(demuxer: D, codec: C, runtime: DecoderRuntime<S>) -> Self {
        let spec = codec.spec();
        let duration = demuxer.duration();
        Self {
            demuxer,
            codec,
            spec,
            duration,
            pools: runtime.pools,
            output: None,
            prefill: Prefill::Needed,
            epoch: runtime.epoch,
            byte_len_handle: runtime.byte_len_handle,
            hooks: runtime.hooks,
            frame_offset: 0,
            pending_seek_target: None,
            resync_frame_offset_to_pts: true,
            timeline_gap_frames: 0,
            head_strip: HeadStrip::default(),
            zero_frame_count: 0,
        }
    }

    fn decode_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let output = self.output.take().ok_or(DecodeError::InvalidData {
            detail: "decoder output was not prepared",
        })??;
        self.output = Some(Ok(output));
        self.next_chunk_inner()
    }

    /// Build the output `AudioChunk` from a just-filled pool buffer plus
    /// the demuxed frame's metadata. Inlined fields (rather than taking
    /// `&Frame<'_>`) so the caller can release the demuxer borrow before
    /// invoking this — needed because `Frame<'_>` borrows into the
    /// demuxer state and would conflict with `&mut self` here.
    #[kithara::probe(timestamp, frames)]
    fn build_chunk(
        &mut self,
        buf: SampleBuffer,
        frames: u32,
        timestamp: Duration,
        source_bytes: u64,
    ) -> AudioChunk {
        let live_spec = self.codec.spec();
        self.spec = live_spec;

        let chunk_secs = f64::from(frames) / f64::from(live_spec.sample_rate.get());
        let frame_duration = Duration::from_secs_f64(chunk_secs);
        let end_timestamp = timestamp.saturating_add(frame_duration);

        let timestamp_frame = live_spec.frame_at(timestamp).unwrap_or(u64::MAX);
        if self.resync_frame_offset_to_pts {
            self.resync_frame_offset_to_pts = false;
            self.frame_offset = timestamp_frame;
        } else if timestamp_frame > self.frame_offset {
            self.timeline_gap_frames = self
                .timeline_gap_frames
                .saturating_add(timestamp_frame.saturating_sub(self.frame_offset));
            self.frame_offset = timestamp_frame;
        }
        let frame_offset = self.frame_offset;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(frames));

        let meta = AudioChunkInfo {
            end_timestamp,
            timestamp,
            frames,
            frame_offset,
            source_bytes,
            segment_index: self.demuxer.current_segment_index(),
            source_byte_offset: None,
            variant_index: self.demuxer.current_variant_index(),
            spec: live_spec,
            epoch: self.epoch,
            render_revision: 0,
        };
        AudioChunk::new(meta, buf)
    }

    fn drain_codec_eof(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if !self
            .codec
            .needs_eof_drain(self.demuxer.track_info().sample_rate)
        {
            return Ok(DecoderChunkOutcome::Eof);
        }

        let mut buf = self.output.take().ok_or(DecodeError::InvalidData {
            detail: "decoder output was not prepared",
        })??;
        let timestamp = self
            .spec
            .duration_for(self.frame_offset)
            .unwrap_or(Duration::from_nanos(u64::MAX));
        let frames = match self.codec.decode_frame(&[], timestamp, &[], &mut buf) {
            Ok(frames) => frames,
            Err(error) => {
                self.output = Some(Ok(buf));
                return Err(error);
            }
        };
        if frames == 0 {
            self.output = Some(Ok(buf));
            return Ok(DecoderChunkOutcome::Eof);
        }
        let chunk = self.build_chunk(buf, frames, timestamp, 0);
        Ok(DecoderChunkOutcome::Chunk(chunk))
    }

    fn emit_chunk_signal(&mut self, outcome: &DecoderChunkOutcome) {
        let signal = match outcome {
            DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
            DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
            DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
        };
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.on_chunk(signal);
        }
    }

    fn emit_seek_signal(&mut self, outcome: &DecoderSeekOutcome) {
        let signal = match outcome {
            DecoderSeekOutcome::Landed {
                landed_byte,
                preroll,
                ..
            } => ReaderSeekSignal::Landed {
                landed_byte: *landed_byte,
                preroll: *preroll,
            },
            DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
        };
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.on_seek(signal);
        }
    }

    #[kithara::measure(label = "decode.composed.next")]
    #[kithara::hang_watchdog]
    fn next_chunk_inner(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            hang_tick!();
            let frame = match self.demuxer.next_frame_prepared()? {
                DemuxOutcome::Frame(frame) => frame,
                DemuxOutcome::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                DemuxOutcome::Eof => return self.drain_codec_eof(),
            };
            hang_reset!();
            let frame_pts = frame.pts;
            let frame_duration = frame.duration;
            let frame_end = frame_pts.saturating_add(frame_duration);
            let source_bytes = u64::try_from(frame.data.len()).unwrap_or(u64::MAX);
            let mut buf = self.output.take().ok_or(DecodeError::InvalidData {
                detail: "decoder output was not prepared",
            })??;
            let mut frames =
                match self
                    .codec
                    .decode_frame(frame.data, frame_pts, frame.packet_desc, &mut buf)
                {
                    Ok(frames) => frames,
                    Err(error) => {
                        self.output = Some(Ok(buf));
                        return Err(error);
                    }
                };
            let prior_head_strip = self.head_strip.frames();
            self.head_strip.record(
                self.spec.frame_at(frame_duration).unwrap_or(u64::MAX),
                u64::from(frames),
            );
            let zero_frame_budget_reached = if frames == 0 {
                self.zero_frame_count = self.zero_frame_count.saturating_add(1);
                self.zero_frame_count >= ZERO_FRAME_BUDGET
            } else {
                self.zero_frame_count = 0;
                false
            };
            let mut chunk_pts = if frames == 0 {
                frame_pts
            } else {
                // A head-trimmed packet keeps its end time; the missing prefix precedes its PCM.
                let stripped = self.head_strip.frames().saturating_sub(prior_head_strip);
                self.codec.decoded_pts(frame_pts).saturating_add(
                    self.codec
                        .spec()
                        .duration_for(stripped)
                        .unwrap_or(Duration::MAX),
                )
            };
            if let Some(target) = self.pending_seek_target {
                let decoded_end = if chunk_pts == frame_pts {
                    frame_end
                } else {
                    chunk_pts.saturating_add(
                        self.codec
                            .spec()
                            .duration_for(u64::from(frames))
                            .unwrap_or(Duration::from_nanos(u64::MAX)),
                    )
                };
                if (frames == 0 && frame_end <= target) || (frames > 0 && decoded_end <= target) {
                    self.output = Some(Ok(buf));
                    if zero_frame_budget_reached {
                        return Ok(DecoderChunkOutcome::Pending(PendingReason::NotReady(
                            NotReadyCause::SourcePending,
                        )));
                    }
                    continue;
                }
                // WHY: frame straddles target — trim leading samples.
                if frames > 0 && chunk_pts < target {
                    let live_spec = self.codec.spec();
                    let trim_frames_u64 =
                        frames_to_trim(chunk_pts, target, live_spec.sample_rate.get())
                            .min(u64::from(frames));
                    let trim_frames = u32::try_from(trim_frames_u64).unwrap_or(frames);
                    // Duration rounding can leave `frame_end > target` even when
                    // this packet is fully pre-target in sample space.
                    if trim_frames >= frames {
                        self.output = Some(Ok(buf));
                        continue;
                    }
                    if trim_frames > 0 {
                        let channels = usize::from(live_spec.channels);
                        let trim_samples = usize::try_from(trim_frames)
                            .unwrap_or(0)
                            .saturating_mul(channels);
                        let total_samples = usize::try_from(frames)
                            .unwrap_or(0)
                            .saturating_mul(channels);
                        if trim_samples < total_samples && trim_samples <= buf.len() {
                            buf.copy_within(trim_samples..total_samples, 0);
                            buf.truncate(total_samples - trim_samples);
                            frames = frames.saturating_sub(trim_frames);
                            chunk_pts = target;
                        }
                    }
                }
                if frames > 0 {
                    self.pending_seek_target = None;
                }
            }
            if frames == 0 {
                self.output = Some(Ok(buf));
                if zero_frame_budget_reached {
                    return Ok(DecoderChunkOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::SourcePending,
                    )));
                }
                continue;
            }
            let chunk = self.build_chunk(buf, frames, chunk_pts, source_bytes);
            return Ok(DecoderChunkOutcome::Chunk(chunk));
        }
    }

    fn seek_inner(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let priming = self.codec.priming(self.demuxer.track_info().codec);
        match self.demuxer.seek(pos, priming)? {
            DemuxSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
            } => {
                self.codec.flush()?;
                self.head_strip = HeadStrip::default();
                self.zero_frame_count = 0;
                self.pending_seek_target = (landed_at < pos).then_some(pos);
                self.frame_offset = self.spec.frame_at(landed_at).unwrap_or(u64::MAX);
                self.resync_frame_offset_to_pts = true;
                self.timeline_gap_frames = 0;
                Ok(DecoderSeekOutcome::Landed {
                    landed_byte,
                    landed_at,
                    preroll,
                    landed_frame: self.frame_offset,
                })
            }
            DemuxSeekOutcome::PastEof { duration } => {
                self.codec.flush()?;
                self.head_strip = HeadStrip::default();
                self.zero_frame_count = 0;
                Ok(DecoderSeekOutcome::PastEof { duration })
            }
        }
    }
}

impl<D, C, S> Decoder for ComposedDecoder<D, C, S>
where
    D: Demuxer + 'static,
    C: FrameCodec,
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn blender_profile(&self) -> BlenderProfile {
        BlenderProfile::new(self.spec)
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn flush_reader_signals(&mut self) {
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.flush();
        }
    }

    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }

    fn prepare_next_chunk(&mut self) {
        if matches!(self.prefill, Prefill::Buffered(_)) {
            return;
        }
        if let Err(error) = self.demuxer.prepare_frame() {
            self.output = Some(Err(error));
            return;
        }
        if self.output.is_none() {
            let mut buffer = self.pools.get::<f32>();
            self.output = Some(self.codec.prepare_output(&mut buffer).map(|()| buffer));
        }
        if matches!(self.prefill, Prefill::Needed) {
            self.prefill = Prefill::Buffered(self.decode_prepared());
        }
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            self.prepare_next_chunk();
            match self.next_chunk_prepared()? {
                DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
                outcome => return Ok(outcome),
            }
        }
    }

    fn next_chunk_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if matches!(self.prefill, Prefill::Buffered(_)) {
            let Prefill::Buffered(result) = std::mem::replace(&mut self.prefill, Prefill::Complete)
            else {
                unreachable!();
            };
            let outcome = result?;
            if matches!(outcome, DecoderChunkOutcome::Pending(_)) {
                self.prefill = Prefill::Needed;
            }
            self.emit_chunk_signal(&outcome);
            return Ok(outcome);
        }
        let outcome = self.decode_prepared()?;
        self.emit_chunk_signal(&outcome);
        Ok(outcome)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.seek_inner(pos)?;
        self.prefill = Prefill::Needed;
        self.emit_seek_signal(&outcome);
        Ok(outcome)
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    /// The frames between a packet's timestamp and the PCM the decoder has
    /// actually produced for it.
    ///
    /// A codec's PTS bias and observed jumps may explain only part of its
    /// algorithmic strip. The directly observed strip remains authoritative
    /// when output timestamps already account for the removed prefix, including
    /// after a seek. Taking the maximum keeps variant-splice offsets consistent.
    fn timeline_gap_frames(&self) -> u64 {
        let modelled = self
            .codec
            .timestamp_bias_frames()
            .saturating_add(self.timeline_gap_frames);
        self.head_strip.frames().max(modelled)
    }

    fn update_byte_len(&self, len: u64) {
        if let Some(handle) = &self.byte_len_handle {
            handle.store(len, Ordering::Release);
        }
    }

    delegate::delegate! {
        to self.codec {
            #[expr(AudioCodec::encoder_priming_frames(codec).saturating_add($))]
            #[call(decoder_algo_delay)]
            fn default_priming_frames(&self, codec: AudioCodec) -> u64;
            fn track_info(&self) -> crate::DecoderTrackInfo;
        }
    }
}

#[cfg(test)]
impl DecoderRuntime<crate::test_pools::TestPools> {
    /// Test-only runtime with a crate-local typed pool region.
    pub(crate) fn for_test() -> Self {
        Self {
            pools: crate::test_pools::pools(),
            epoch: 0,
            byte_len_handle: None,
            hooks: None,
        }
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod default_priming_tests {
    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::fixtures::tone_mp3;
    use symphonia::{
        core::{
            formats::{FormatOptions, probe::Hint},
            io::{MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
        },
        default,
    };

    use super::*;
    use crate::symphonia::{SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer};

    fn build_mp3_decoder(
        tone_mp3: &[u8],
    ) -> ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec, crate::test_pools::TestPools> {
        let cursor = Cursor::new(tone_mp3.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        let demuxer = SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("BUG: MP3 demuxer should build");
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test())
    }

    fn prepared_chunk(decoder: &mut dyn Decoder) -> AudioChunk {
        for _ in 0..ZERO_FRAME_BUDGET {
            decoder.prepare_next_chunk();
            match decoder.next_chunk_prepared().expect("prepared decode") {
                DecoderChunkOutcome::Chunk(chunk) => return chunk,
                DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
                _ => panic!("expected PCM or another prepared packet during warmup"),
            }
        }
        panic!("prepared decode exhausted the codec warmup budget");
    }

    #[kithara::test]
    fn prefilled_mp3_preserves_first_pcm_and_seek(tone_mp3: &'static [u8]) {
        let mut decoder = build_mp3_decoder(tone_mp3);
        decoder.prepare_next_chunk();
        let mut reference = build_mp3_decoder(tone_mp3);
        for position in [None, Some(Duration::ZERO), Some(Duration::from_millis(250))] {
            if let Some(position) = position {
                decoder.seek(position).expect("seek");
                reference.seek(position).expect("reference seek");
            }
            for _ in 0..4 {
                decoder.prepare_next_chunk();
                decoder.prepare_next_chunk();
                let actual = decoder.next_chunk().expect("prefilled decode");
                let expected = reference.next_chunk().expect("reference decode");
                assert_same_mp3_output(&actual, &expected);
            }
        }
    }

    #[kithara::test]
    fn initialized_mp3_reuses_decoder_storage_after_seek(tone_mp3: &'static [u8]) {
        let mut decoder = build_mp3_decoder(tone_mp3);
        let mut reference = build_mp3_decoder(tone_mp3);
        let first = decoder.next_chunk().expect("initial decode");
        assert!(matches!(first, DecoderChunkOutcome::Chunk(_)));
        let expected_first = reference.next_chunk().expect("reference initial decode");
        assert_same_mp3_output(&first, &expected_first);
        for position in [None, Some(Duration::ZERO), Some(Duration::from_millis(250))] {
            if let Some(position) = position {
                decoder.seek(position).expect("seek");
                reference.seek(position).expect("reference seek");
            }
            let mut produced = 0;
            for _ in 0..4 {
                decoder.prepare_next_chunk();
                let chunk = checked_mp3_chunk(&mut decoder).expect("checked decode");
                reference.prepare_next_chunk();
                let expected = reference.next_chunk_prepared().expect("reference decode");
                assert_same_mp3_output(&chunk, &expected);
                produced += usize::from(matches!(chunk, DecoderChunkOutcome::Chunk(_)));
            }
            assert!(produced > 0, "checked calls must produce PCM");
        }
    }

    fn assert_same_mp3_output(actual: &DecoderChunkOutcome, expected: &DecoderChunkOutcome) {
        match (actual, expected) {
            (DecoderChunkOutcome::Chunk(actual), DecoderChunkOutcome::Chunk(expected)) => {
                assert_eq!(actual.meta.timestamp, expected.meta.timestamp);
                assert_eq!(actual.meta.frame_offset, expected.meta.frame_offset);
                assert_eq!(actual.meta.frames, expected.meta.frames);
                assert_eq!(&*actual.samples, &*expected.samples);
            }
            (
                DecoderChunkOutcome::Pending(PendingReason::Retry),
                DecoderChunkOutcome::Pending(PendingReason::Retry),
            ) => {}
            _ => panic!("expected matching PCM or packet progress"),
        }
    }

    #[kithara::rtsan_forbid_blocking]
    fn checked_mp3_chunk(decoder: &mut dyn Decoder) -> DecodeResult<DecoderChunkOutcome> {
        decoder.next_chunk_prepared()
    }

    #[kithara::test]
    fn prepared_mp3_decode_preserves_pcm_and_seek(tone_mp3: &'static [u8]) {
        let mut prepared = build_mp3_decoder(tone_mp3);
        let mut regular = build_mp3_decoder(tone_mp3);
        assert!(matches!(
            prepared.next_chunk_prepared(),
            Err(DecodeError::InvalidData { .. })
        ));
        prepared.output = Some(Err(DecodeError::InvalidData {
            detail: "output preparation failed",
        }));
        assert!(matches!(
            prepared.next_chunk_prepared(),
            Err(DecodeError::InvalidData {
                detail: "output preparation failed"
            })
        ));
        for seek in [None, Some(Duration::from_millis(250))] {
            if let Some(position) = seek {
                prepared.seek(position).expect("prepared seek");
                regular.seek(position).expect("regular seek");
            }
            for _ in 0..4 {
                let actual = prepared_chunk(&mut prepared);
                let DecoderChunkOutcome::Chunk(expected) =
                    regular.next_chunk().expect("regular decode")
                else {
                    panic!("expected PCM chunk");
                };
                assert_eq!(actual.meta.timestamp, expected.meta.timestamp);
                assert_eq!(actual.meta.frames, expected.meta.frames);
                assert_eq!(&*actual.samples, &*expected.samples);
            }
        }
    }

    #[kithara::test]
    fn composed_decoder_priming_combines_encoder_and_symphonia_mp3_algo_delay(
        tone_mp3: &'static [u8],
    ) {
        let decoder = build_mp3_decoder(tone_mp3);
        // WHY: 1105 = 576 libmp3lame priming + 529 LAME algo delay.
        assert_eq!(decoder.default_priming_frames(AudioCodec::Mp3), 1105);
        assert_eq!(decoder.default_priming_frames(AudioCodec::AacLc), 1024);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Opus), 312);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Flac), 0);
    }
}

/// Per-channel sample count to drop from the head of the target packet
/// so the emitted chunk starts at the user's seek target rather than
/// at the packet boundary. Returns 0 when `frame_pts >= target` or
/// `sample_rate == 0`. Rounds to nearest sample (half-up) so the trim
/// matches `round(target_secs * sample_rate)` callers use to index
/// PCM by absolute sample frame.
fn frames_to_trim(frame_pts: Duration, target: Duration, sample_rate: u32) -> u64 {
    if sample_rate == 0 || frame_pts >= target {
        return 0;
    }
    let delta_nanos = target.saturating_sub(frame_pts).as_nanos();
    let sr_u128 = u128::from(sample_rate);
    // WHY: round-to-nearest sample, half-up via +5e8 before the /1e9 divide.
    let frames_u128 = delta_nanos
        .saturating_mul(sr_u128)
        .saturating_add(500_000_000)
        / 1_000_000_000;
    u64::try_from(frames_u128).unwrap_or(u64::MAX)
}

#[cfg(all(test, feature = "symphonia"))]
mod smoke_tests {

    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::fixtures::tone_mp3;
    use kithara_test_utils::kithara;
    use symphonia::{
        core::{
            formats::{FormatOptions, probe::Hint},
            io::{MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
        },
        default,
    };

    use super::*;
    use crate::{
        symphonia::{FileOpen, SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
        traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    };

    fn build_mp3_demuxer(tone_mp3: &[u8]) -> SymphoniaDemuxer {
        let cursor = Cursor::new(tone_mp3.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("BUG: MP3 demuxer should build")
    }

    #[kithara::test]
    fn mp3_track_info_carries_codec_and_rate(tone_mp3: &'static [u8]) {
        let demuxer = build_mp3_demuxer(tone_mp3);
        let info = demuxer.track_info();
        assert_eq!(info.codec, AudioCodec::Mp3);
        assert!(info.sample_rate > 0, "sample rate must be populated");
        assert!(info.channels > 0, "channels must be populated");
    }

    #[kithara::test]
    fn mp3_universal_decoder_emits_non_empty_chunks(tone_mp3: &'static [u8]) {
        let demuxer = build_mp3_demuxer(tone_mp3);
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let mut got_chunk = false;
        for _ in 0..16 {
            match decoder
                .next_chunk()
                .expect("BUG: next_chunk should not error")
            {
                DecoderChunkOutcome::Chunk(chunk) => {
                    assert!(chunk.frames() > 0, "Chunk frames must be > 0");
                    assert!(chunk.spec().sample_rate.get() > 0);
                    assert!(chunk.spec().channels > 0);
                    got_chunk = true;
                    break;
                }
                DecoderChunkOutcome::Pending(_) => continue,
                DecoderChunkOutcome::Eof => panic!("MP3 fixture must not EOF in 16 packets"),
            }
        }
        assert!(got_chunk, "ComposedDecoder must emit at least one chunk");
    }

    #[kithara::test]
    fn mp3_universal_decoder_seeks_back_to_start_after_pulling_chunks(tone_mp3: &'static [u8]) {
        let demuxer = build_mp3_demuxer(tone_mp3);
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        for _ in 0..4 {
            let _ = decoder
                .next_chunk()
                .expect("BUG: priming chunks should not error");
        }

        let outcome = decoder
            .seek(Duration::ZERO)
            .expect("BUG: seek to start must not error");
        match outcome {
            DecoderSeekOutcome::Landed { landed_at, .. } => {
                assert!(
                    landed_at < Duration::from_millis(50),
                    "seek to ZERO should land near 0, got {landed_at:?}"
                );
            }
            DecoderSeekOutcome::PastEof { .. } => {
                panic!("seek(0) on a real MP3 must not be PastEof")
            }
        }
    }

    #[kithara::test]
    fn symphonia_mp3_demuxer_emits_notneeded_preroll_after_seek(tone_mp3: &'static [u8]) {
        let (demuxer, _byte_len_handle) = SymphoniaDemuxer::open_file(
            Cursor::new(tone_mp3),
            FileOpen {
                hint: Some("mp3".into()),
                container: None,
                byte_len_handle: None,
                byte_map: None,
            },
        )
        .expect("BUG: open_file must succeed");
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let outcome = decoder.seek(Duration::from_secs(1)).expect("BUG: seek");
        let DecoderSeekOutcome::Landed {
            landed_byte,
            preroll,
            ..
        } = outcome
        else {
            panic!("expected Landed, got {outcome:?}");
        };
        assert!(
            landed_byte.is_some(),
            "BUG: symphonia MP3 must expose landed_byte"
        );
        assert_eq!(
            preroll,
            kithara_stream::PrerollHint::NotNeeded,
            "Symphonia handles MDCT priming internally; preroll must be NotNeeded"
        );
    }
}

#[cfg(test)]
fn write_silent_test_frame(
    pcm: &[f32],
    spec: AudioSpec,
    frames_per_call: u32,
    out: &mut SampleBuffer,
) -> DecodeResult<u32> {
    let frames = usize::try_from(frames_per_call)?;
    let samples = frames
        .checked_mul(usize::from(spec.channels))
        .ok_or_else(|| DecodeError::InvalidData {
            detail: "test frame sample count overflow",
        })?;
    out.ensure_len(samples)?;
    out[..samples].copy_from_slice(&pcm[..samples]);
    out.truncate(samples);
    Ok(frames_per_call)
}

#[cfg(test)]
macro_rules! reset_demuxer_seek {
    ($field:ident = $value:expr) => {
        fn seek(
            &mut self,
            _pos: kithara_platform::time::Duration,
            _priming: crate::codec::CodecPriming,
        ) -> crate::error::DecodeResult<crate::demuxer::DemuxSeekOutcome> {
            self.$field = $value;
            Ok(crate::demuxer::DemuxSeekOutcome::Landed {
                landed_at: kithara_platform::time::Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }
    };
}

#[cfg(test)]
mod test_stub_codec {

    use kithara_bufpool::SampleBuffer;
    use kithara_platform::time::Duration;
    use kithara_signal::AudioSpec;

    use crate::{codec::FrameCodec, error::DecodeResult};

    pub(super) struct ConstFrameCodec {
        pcm: Vec<f32>,
        spec: AudioSpec,
        frames_per_call: u32,
    }

    pub(super) struct LaggedQueueCodec {
        pcm: Vec<f32>,
        spec: AudioSpec,
        decoded_pts: Duration,
        pending_pts: Option<Duration>,
        frames_per_call: u32,
    }

    impl ConstFrameCodec {
        pub(super) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
            Self {
                pcm,
                spec,
                frames_per_call,
            }
        }
    }

    impl LaggedQueueCodec {
        pub(super) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
            Self {
                pcm,
                spec,
                frames_per_call,
                pending_pts: None,
                decoded_pts: Duration::ZERO,
            }
        }
    }

    impl FrameCodec for ConstFrameCodec {
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            super::write_silent_test_frame(&self.pcm, self.spec, self.frames_per_call, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            Ok(())
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }

    impl FrameCodec for LaggedQueueCodec {
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            let Some(decoded_pts) = self.pending_pts.replace(pts) else {
                out.clear();
                return Ok(0);
            };
            self.decoded_pts = decoded_pts;
            super::write_silent_test_frame(&self.pcm, self.spec, self.frames_per_call, out)
        }

        fn decoded_pts(&self, _input_pts: Duration) -> Duration {
            self.decoded_pts
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.pending_pts = None;
            Ok(())
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod test_counting_codec {

    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicU32, Ordering},
    };

    use kithara_bufpool::SampleBuffer;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::AudioSpec;

    use crate::{codec::FrameCodec, error::DecodeResult};

    pub(super) struct CountingCodec {
        pcm: Vec<f32>,
        pub(super) decode_calls: Arc<AtomicU32>,
        pub(super) flush_calls: Arc<AtomicU32>,
        pub(super) spec: AudioSpec,
        pub(super) frames_per_call: u32,
        frames: VecDeque<u32>,
    }

    impl CountingCodec {
        pub(super) fn new(pcm: Vec<f32>, spec: AudioSpec, frames_per_call: u32) -> Self {
            Self {
                pcm,
                spec,
                frames_per_call,
                frames: VecDeque::new(),
                decode_calls: Arc::new(AtomicU32::new(0)),
                flush_calls: Arc::new(AtomicU32::new(0)),
            }
        }

        pub(super) fn with_frames(mut self, frames: impl IntoIterator<Item = u32>) -> Self {
            self.frames.extend(frames);
            self
        }
    }

    impl FrameCodec for CountingCodec {
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            self.decode_calls.fetch_add(1, Ordering::SeqCst);
            let frames = self.frames.pop_front().unwrap_or(self.frames_per_call);
            super::write_silent_test_frame(&self.pcm, self.spec, frames, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.flush_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod test_eof_drain_codec {

    use std::sync::atomic::{AtomicU32, Ordering};

    use kithara_bufpool::SampleBuffer;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::AudioSpec;

    use crate::{codec::FrameCodec, error::DecodeResult};

    pub(super) struct EofDrainCodec {
        pcm: Vec<f32>,
        pub(super) empty_decode_calls: Arc<AtomicU32>,
        spec: AudioSpec,
        tail_pending: bool,
        frames_per_call: u32,
        tail_frames: u32,
    }

    impl EofDrainCodec {
        pub(super) fn new(
            pcm: Vec<f32>,
            spec: AudioSpec,
            frames_per_call: u32,
            tail_frames: u32,
        ) -> Self {
            Self {
                pcm,
                empty_decode_calls: Arc::new(AtomicU32::new(0)),
                spec,
                frames_per_call,
                tail_frames,
                tail_pending: tail_frames > 0,
            }
        }
    }

    impl FrameCodec for EofDrainCodec {
        fn decode_frame(
            &mut self,
            bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            let frames = if bytes.is_empty() {
                self.empty_decode_calls.fetch_add(1, Ordering::SeqCst);
                self.tail_pending
                    .then(|| {
                        self.tail_pending = false;
                        self.tail_frames
                    })
                    .unwrap_or(0)
            } else {
                self.frames_per_call
            };
            super::write_silent_test_frame(&self.pcm, self.spec, frames, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.tail_pending = self.tail_frames > 0;
            Ok(())
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }

    pub(super) struct QueueCodec {
        pcm: Vec<f32>,
        pub(super) empty_decode_calls: Arc<AtomicU32>,
        spec: AudioSpec,
        tail_pending: bool,
        tail_frames: u32,
    }

    impl QueueCodec {
        pub(super) fn new(pcm: Vec<f32>, spec: AudioSpec, tail_frames: u32) -> Self {
            Self {
                pcm,
                spec,
                tail_frames,
                empty_decode_calls: Arc::new(AtomicU32::new(0)),
                tail_pending: false,
            }
        }
    }

    impl FrameCodec for QueueCodec {
        fn decode_frame(
            &mut self,
            bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            if !bytes.is_empty() {
                self.tail_pending = true;
                out.clear();
                return Ok(0);
            }

            self.empty_decode_calls.fetch_add(1, Ordering::SeqCst);
            if !self.tail_pending {
                out.clear();
                return Ok(0);
            }
            self.tail_pending = false;
            super::write_silent_test_frame(&self.pcm, self.spec, self.tail_frames, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.tail_pending = false;
            Ok(())
        }

        fn needs_eof_drain(&self, _source_sample_rate: u32) -> bool {
            true
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod seek_trim_tests {
    use std::{num::NonZeroU32, sync::atomic::Ordering};

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::AudioSpec;
    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::{mock_fixtures::zero_packet, unit_fixtures::trim_silence};
    use kithara_test_utils::kithara;

    use super::{test_counting_codec::CountingCodec, test_stub_codec::LaggedQueueCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    struct Consts;

    impl Consts {
        const CHANNELS: u16 = 2;
        const OUTPUT_SAMPLE_RATE: u32 = 48_000;
        const PACKET_COUNT: u64 = 6;
        const PACKET_FRAMES: u32 = 1024;
        const SAMPLE_RATE: u32 = 44_100;
    }

    fn test_spec(sample_rate: u32) -> AudioSpec {
        AudioSpec::new(
            1,
            NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
        )
    }

    fn test_duration(sample_rate: u32, frames: u64) -> Duration {
        test_spec(sample_rate)
            .duration_for(frames)
            .expect("test duration is representable")
    }

    fn test_frames(sample_rate: u32, duration: Duration) -> usize {
        test_spec(sample_rate)
            .frames_for(duration)
            .expect("test frame count is representable")
            .get()
    }

    fn test_frame(sample_rate: u32, timestamp: Duration) -> u64 {
        test_spec(sample_rate)
            .frame_at(timestamp)
            .expect("test frame is representable")
    }

    struct ThreeFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        idx: usize,
    }

    #[derive(Clone, Copy)]
    struct BoundaryFrame {
        duration: Duration,
        pts: Duration,
    }

    struct BoundaryFrameDemuxer {
        track: TrackInfo,
        frames: Vec<BoundaryFrame>,
        held: Vec<u8>,
        idx: usize,
    }

    #[derive(Clone, Copy)]
    enum SeekTrimLayout {
        RegularPackets,
        RoundedPastTarget,
    }

    impl Demuxer for ThreeFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_millis(60))
        }
        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            let pts_ms = match self.idx {
                0 => 0,
                1 => 20,
                2 => 40,
                _ => return Ok(DemuxOutcome::Eof),
            };
            self.idx += 1;
            Ok(DemuxOutcome::Frame(Frame {
                pts: Duration::from_millis(pts_ms),
                duration: Duration::from_millis(20),
                data: &self.held,
                packet_desc: &[],
            }))
        }
        reset_demuxer_seek!(idx = 0);
        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    impl Demuxer for BoundaryFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            let Some(frame) = self.frames.get(self.idx).copied() else {
                return Ok(DemuxOutcome::Eof);
            };
            self.idx += 1;
            Ok(DemuxOutcome::Frame(Frame {
                pts: frame.pts,
                duration: frame.duration,
                data: &self.held,
                packet_desc: &[],
            }))
        }

        reset_demuxer_seek!(idx = 0);

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        track_with_rate(Consts::SAMPLE_RATE)
    }

    fn track_with_rate(sample_rate: u32) -> TrackInfo {
        TrackInfo {
            sample_rate,
            codec: AudioCodec::AacLc,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    fn packet_duration() -> Duration {
        test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES))
    }

    fn regular_seek_frames() -> Vec<BoundaryFrame> {
        (0..Consts::PACKET_COUNT)
            .map(|packet_idx| BoundaryFrame {
                pts: test_duration(
                    Consts::SAMPLE_RATE,
                    packet_idx.saturating_mul(u64::from(Consts::PACKET_FRAMES)),
                ),
                duration: packet_duration(),
            })
            .collect()
    }

    fn rounded_past_target_frames(target: Duration) -> Vec<BoundaryFrame> {
        vec![
            BoundaryFrame {
                pts: Duration::ZERO,
                duration: target.saturating_add(Duration::from_nanos(1)),
            },
            BoundaryFrame {
                pts: target,
                duration: packet_duration(),
            },
        ]
    }

    fn seek_frames_for(target: Duration, layout: SeekTrimLayout) -> Vec<BoundaryFrame> {
        match layout {
            SeekTrimLayout::RegularPackets => regular_seek_frames(),
            SeekTrimLayout::RoundedPastTarget => rounded_past_target_frames(target),
        }
    }

    #[kithara::test]
    fn pre_target_frames_are_decoded_and_dropped_by_pending_seek_target(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        const FRAME_FRAMES: u32 = 882;

        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            FRAME_FRAMES,
        );
        let calls = Arc::clone(&codec.decode_calls);
        let demuxer = ThreeFrameDemuxer {
            track: empty_track(),
            idx: 0,
            held: zero_packet.to_vec(),
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(Duration::from_millis(30)).expect("BUG: seek");

        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        assert!(
            matches!(outcome, DecoderChunkOutcome::Chunk(_)),
            "expected Chunk, got {outcome:?}"
        );

        // WHY: 2 calls — frame[0] (end=20ms ≤ 30ms target) dropped by the guard; frame[1] (end=40ms) emitted with leading 10ms trimmed.
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "decode_frame must be called for pre-target frames so MDCT advances"
        );

        let packet = test_duration(Consts::SAMPLE_RATE, 1_024);
        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("test rate"),
            ),
            1_024,
        )
        .with_frames([363]);
        let demuxer = BoundaryFrameDemuxer {
            track: empty_track(),
            held: zero_packet.to_vec(),
            idx: 0,
            frames: vec![
                BoundaryFrame {
                    pts: packet,
                    duration: packet,
                },
                BoundaryFrame {
                    pts: packet.saturating_mul(2),
                    duration: packet,
                },
            ],
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
        let first = decoder.next_chunk().expect("first head-trimmed chunk");
        assert!(matches!(first, DecoderChunkOutcome::Chunk(_)));
        let second = decoder.next_chunk().expect("second full packet");
        let DecoderChunkOutcome::Chunk(second) = second else {
            panic!("expected second PCM chunk, got {second:?}");
        };
        assert_eq!(
            second.meta.frame_offset, 2_048,
            "a decoder head trim must not compress every later packet's frame offset"
        );
        assert_eq!(
            decoder.timeline_gap_frames(),
            661,
            "the decoder must publish forward timestamp gaps without a second frame cursor"
        );
    }

    #[kithara::test]
    fn composed_decoder_observes_head_strip_from_packet_duration(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        const SOURCE_SAMPLE_RATE: u32 = 24_000;
        const OUTPUT_SAMPLE_RATE: u32 = 48_000;
        const PACKET_DURATION: Duration = Duration::from_millis(20);
        const SUPPLIED_FRAMES: u32 = 960;

        assert_eq!(
            test_frames(OUTPUT_SAMPLE_RATE, PACKET_DURATION),
            usize::try_from(SUPPLIED_FRAMES).expect("supplied frames fit in usize")
        );
        assert_eq!(
            test_frames(SOURCE_SAMPLE_RATE, PACKET_DURATION),
            480,
            "the fixture must distinguish source-rate from output-rate frames"
        );

        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(OUTPUT_SAMPLE_RATE).expect("test rate"),
            ),
            SUPPLIED_FRAMES,
        )
        .with_frames([480, 720]);
        let demuxer = BoundaryFrameDemuxer {
            track: track_with_rate(SOURCE_SAMPLE_RATE),
            held: zero_packet.to_vec(),
            idx: 0,
            frames: vec![
                BoundaryFrame {
                    pts: Duration::ZERO,
                    duration: PACKET_DURATION,
                };
                3
            ],
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        for packet_idx in 0..3 {
            let outcome = decoder.next_chunk().expect("packet must decode");
            assert!(
                matches!(outcome, DecoderChunkOutcome::Chunk(_)),
                "packet {packet_idx} must emit a chunk, got {outcome:?}"
            );
        }

        assert_eq!(
            decoder.timeline_gap_frames, 0,
            "constant packet PTS must keep the modelled timeline gap out of the result"
        );
        assert_eq!(
            decoder.timeline_gap_frames(),
            720,
            "the composed decoder must publish the head strip measured in output frames"
        );
    }

    #[kithara::test]
    fn fully_trimmed_seek_packet_is_dropped_before_target_chunk(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        const SAMPLE_RATE: u32 = 44_100;
        const PACKET_FRAMES: u32 = 1024;

        let target = test_duration(SAMPLE_RATE, u64::from(PACKET_FRAMES));
        let rounded_past_target = target.saturating_add(Duration::from_nanos(1));
        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            PACKET_FRAMES,
        );
        let calls = Arc::clone(&codec.decode_calls);
        let demuxer = BoundaryFrameDemuxer {
            track: empty_track(),
            held: zero_packet.to_vec(),
            idx: 0,
            frames: vec![
                BoundaryFrame {
                    pts: Duration::ZERO,
                    duration: rounded_past_target,
                },
                BoundaryFrame {
                    pts: target,
                    duration: rounded_past_target,
                },
            ],
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the fully pre-target packet must be decode-discarded before emitting"
        );
        assert_eq!(chunk.meta.frame_offset, u64::from(PACKET_FRAMES));
        assert_eq!(chunk.meta.timestamp, target);
    }

    #[kithara::test]
    fn queue_codec_keeps_seek_target_until_straddling_pcm_arrives(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        const FRAME_FRAMES: u32 = 882;

        let target = Duration::from_millis(10);
        let codec = LaggedQueueCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            FRAME_FRAMES,
        );
        let demuxer = ThreeFrameDemuxer {
            track: empty_track(),
            idx: 0,
            held: zero_packet.to_vec(),
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };

        assert_eq!(chunk.meta.timestamp, target);
        assert_eq!(chunk.meta.frames, FRAME_FRAMES / 2);
    }

    #[kithara::test]
    #[case::packet_boundary(
        test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES)),
        SeekTrimLayout::RegularPackets
    )]
    #[case::mid_packet(
        test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES) + 512),
        SeekTrimLayout::RegularPackets
    )]
    #[case::rounding_hair_past_boundary(
        test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES))
            .saturating_add(Duration::from_nanos(1)),
        SeekTrimLayout::RoundedPastTarget
    )]
    #[case::multiple_fully_pre_target_packets(
        test_duration(
            Consts::SAMPLE_RATE,
            u64::from(Consts::PACKET_FRAMES) * 3 + 512,
        ),
        SeekTrimLayout::RegularPackets
    )]
    fn seek_trim_lands_first_chunk_on_exact_target_frame(
        #[case] target: Duration,
        #[case] layout: SeekTrimLayout,
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("test rate"),
            ),
            Consts::PACKET_FRAMES,
        );
        let demuxer = BoundaryFrameDemuxer {
            track: empty_track(),
            held: zero_packet.to_vec(),
            frames: seek_frames_for(target, layout),
            idx: 0,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };
        let expected_frame = test_frame(Consts::SAMPLE_RATE, target);
        let packet_offset = expected_frame % u64::from(Consts::PACKET_FRAMES);
        let expected_frames = if packet_offset == 0 {
            Consts::PACKET_FRAMES
        } else {
            Consts::PACKET_FRAMES - u32::try_from(packet_offset).expect("packet offset fits in u32")
        };

        assert_eq!(chunk.meta.frame_offset, expected_frame);
        assert_eq!(chunk.meta.timestamp, target);
        assert_eq!(
            test_frame(Consts::SAMPLE_RATE, chunk.meta.timestamp),
            expected_frame
        );
        assert!(
            chunk.meta.timestamp >= target,
            "first chunk timestamp {:?} before seek target {:?}",
            chunk.meta.timestamp,
            target
        );
        assert_eq!(chunk.meta.frames, expected_frames);
    }

    #[kithara::test]
    fn seek_trim_uses_codec_output_rate_for_resampled_chunks(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        let source_packet_start =
            test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES));
        let target = test_duration(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES) + 512);
        let output_packet_frames = u32::try_from(frames_to_trim(
            Duration::ZERO,
            packet_duration(),
            Consts::OUTPUT_SAMPLE_RATE,
        ))
        .expect("output packet frame count fits in u32");
        let codec = CountingCodec::new(
            trim_silence.clone(),
            AudioSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(Consts::OUTPUT_SAMPLE_RATE).expect("test rate"),
            ),
            output_packet_frames,
        );
        let demuxer = BoundaryFrameDemuxer {
            track: track_with_rate(Consts::SAMPLE_RATE),
            held: zero_packet.to_vec(),
            frames: regular_seek_frames(),
            idx: 0,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };
        let expected_trim = frames_to_trim(source_packet_start, target, Consts::OUTPUT_SAMPLE_RATE);
        let expected_frames =
            output_packet_frames - u32::try_from(expected_trim).expect("trim fits in u32");
        let expected_frame = test_frame(Consts::OUTPUT_SAMPLE_RATE, target);

        assert_eq!(
            chunk.meta.spec.sample_rate.get(),
            Consts::OUTPUT_SAMPLE_RATE
        );
        assert_eq!(chunk.meta.frame_offset, expected_frame);
        assert_eq!(chunk.meta.timestamp, target);
        assert_eq!(chunk.meta.frames, expected_frames);
    }
}

#[cfg(test)]
mod eof_drain_tests {
    use std::{num::NonZeroU32, sync::atomic::Ordering};

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::{mock_fixtures::one_packet, unit_fixtures::trim_silence};
    use kithara_test_utils::kithara;

    use super::{
        test_eof_drain_codec::{EofDrainCodec, QueueCodec},
        *,
    };
    use crate::{
        demuxer::{DemuxOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    struct OneFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        emitted: bool,
    }

    impl Demuxer for OneFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            if self.emitted {
                return Ok(DemuxOutcome::Eof);
            }
            self.emitted = true;
            Ok(DemuxOutcome::Frame(Frame {
                pts: Duration::ZERO,
                duration: Duration::from_millis(20),
                data: &self.held,
                packet_desc: &[],
            }))
        }

        reset_demuxer_seek!(emitted = false);

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::AacLc,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    #[kithara::test]
    fn rate_mismatch_default_codec_drains_tail_before_eof(
        trim_silence: Vec<f32>,
        one_packet: &'static [u8],
    ) {
        const FRAMES: u32 = 960;
        const TAIL_FRAMES: u32 = 17;
        const SAMPLE_RATE: u32 = 48_000;

        let codec = EofDrainCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            FRAMES,
            TAIL_FRAMES,
        );
        let empty_calls = Arc::clone(&codec.empty_decode_calls);
        let demuxer = OneFrameDemuxer {
            track: empty_track(),
            held: one_packet.to_vec(),
            emitted: false,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let first = decoder.next_chunk().expect("BUG: first chunk");
        let DecoderChunkOutcome::Chunk(first_chunk) = first else {
            panic!("expected first Chunk, got {first:?}");
        };
        let drained = decoder.next_chunk().expect("BUG: EOF drain chunk");
        let DecoderChunkOutcome::Chunk(drained_chunk) = drained else {
            panic!("expected drained Chunk, got {drained:?}");
        };
        let eof = decoder.next_chunk().expect("BUG: EOF after drain");

        assert!(matches!(eof, DecoderChunkOutcome::Eof));
        assert_eq!(first_chunk.meta.frames, FRAMES);
        assert_eq!(drained_chunk.meta.frames, TAIL_FRAMES);
        assert_eq!(drained_chunk.meta.frame_offset, u64::from(FRAMES));
        assert_eq!(
            drained_chunk.meta.timestamp,
            AudioSpec::new(
                1,
                NonZeroU32::new(SAMPLE_RATE).expect("test sample rate is non-zero"),
            )
            .duration_for(u64::from(FRAMES))
            .expect("test duration is representable")
        );
        assert_eq!(empty_calls.load(Ordering::SeqCst), 2);
        assert!(matches!(
            decoder
                .next_chunk_prepared()
                .expect("EOF retains prepared output"),
            DecoderChunkOutcome::Eof
        ));
    }

    #[kithara::test]
    fn equal_rate_queue_codec_emits_tail_before_eof(
        trim_silence: Vec<f32>,
        one_packet: &'static [u8],
    ) {
        const TAIL_FRAMES: u32 = 17;
        const SAMPLE_RATE: u32 = 44_100;

        let codec = QueueCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            TAIL_FRAMES,
        );
        let empty_calls = Arc::clone(&codec.empty_decode_calls);
        let demuxer = OneFrameDemuxer {
            track: empty_track(),
            held: one_packet.to_vec(),
            emitted: false,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let drained = decoder.next_chunk().expect("BUG: EOF drain chunk");
        let DecoderChunkOutcome::Chunk(drained_chunk) = drained else {
            panic!("expected drained Chunk, got {drained:?}");
        };
        let eof = decoder.next_chunk().expect("BUG: EOF after drain");

        assert_eq!(drained_chunk.meta.frames, TAIL_FRAMES);
        assert!(matches!(eof, DecoderChunkOutcome::Eof));
        assert_eq!(empty_calls.load(Ordering::SeqCst), 2);
    }

    #[kithara::test]
    fn equal_rate_default_codec_skips_eof_drain(trim_silence: Vec<f32>, one_packet: &'static [u8]) {
        const FRAMES: u32 = 960;
        const TAIL_FRAMES: u32 = 17;
        const SAMPLE_RATE: u32 = 44_100;

        let codec = EofDrainCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            FRAMES,
            TAIL_FRAMES,
        );
        let empty_calls = Arc::clone(&codec.empty_decode_calls);
        let demuxer = OneFrameDemuxer {
            track: empty_track(),
            held: one_packet.to_vec(),
            emitted: false,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let first = decoder.next_chunk().expect("BUG: first chunk");
        let eof = decoder.next_chunk().expect("BUG: EOF without drain");

        assert!(matches!(first, DecoderChunkOutcome::Chunk(_)));
        assert!(matches!(eof, DecoderChunkOutcome::Eof));
        assert_eq!(empty_calls.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod hook_tests {
    use std::{num::NonZeroU32, sync::Mutex};

    use kithara_platform::sync::Arc;
    use kithara_stream::{
        BoxedEventSink, PendingReason, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal,
    };
    use kithara_test_fixtures::{mock_fixtures::zero_packet, unit_fixtures::trim_silence};
    use kithara_test_utils::kithara;

    use super::{test_stub_codec::ConstFrameCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    #[derive(Default)]
    struct CallLog {
        chunks: Vec<&'static str>,
        seeks: Vec<&'static str>,
    }

    struct LoggingHooks {
        log: Arc<Mutex<CallLog>>,
    }

    impl ReaderEventSink for LoggingHooks {
        fn on_chunk(&mut self, signal: ReaderChunkSignal) {
            let tag = match signal {
                ReaderChunkSignal::Chunk => "chunk",
                ReaderChunkSignal::Pending(_) => "pending",
                ReaderChunkSignal::Eof => "eof",
                _ => "other",
            };
            self.log.lock().unwrap().chunks.push(tag);
        }

        fn on_seek(&mut self, signal: ReaderSeekSignal) {
            let tag = match signal {
                ReaderSeekSignal::Landed { .. } => "landed",
                ReaderSeekSignal::PastEof => "past_eof",
                _ => "other",
            };
            self.log.lock().unwrap().seeks.push(tag);
        }
    }

    /// Owned variant of `DemuxOutcome` used to seed the stub. Kept
    /// separate so the borrowed `DemuxOutcome<'_>` returned by
    /// `next_frame` can be backed by the stub's own buffer.
    enum StubOutcome {
        Frame { pts: Duration, duration: Duration },
        Pending(PendingReason),
    }

    /// Stub demuxer + codec pair driven by canned outcomes. Exists only so
    /// hook tests can construct a `ComposedDecoder` without needing a real
    /// container/codec.
    struct StubDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        next: Vec<StubOutcome>,
        seek: Vec<DemuxSeekOutcome>,
    }

    impl StubDemuxer {
        fn with_outcomes(
            zero_packet: &[u8],
            next: Vec<StubOutcome>,
            seek: Vec<DemuxSeekOutcome>,
        ) -> Self {
            Self {
                next,
                seek,
                track: empty_track(),
                held: zero_packet.to_vec(),
            }
        }
    }

    impl Demuxer for StubDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            match self.next.pop() {
                Some(StubOutcome::Frame { pts, duration }) => Ok(DemuxOutcome::Frame(Frame {
                    pts,
                    duration,
                    data: &self.held,
                    packet_desc: &[],
                })),
                Some(StubOutcome::Pending(reason)) => Ok(DemuxOutcome::Pending(reason)),
                None => Ok(DemuxOutcome::Eof),
            }
        }
        fn seek(
            &mut self,
            _pos: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            Ok(self.seek.pop().unwrap_or(DemuxSeekOutcome::PastEof {
                duration: Duration::ZERO,
            }))
        }
        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::Flac,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    fn build(
        trim_silence: Vec<f32>,
        demuxer: StubDemuxer,
        log: Arc<Mutex<CallLog>>,
    ) -> ComposedDecoder<StubDemuxer, ConstFrameCodec, crate::test_pools::TestPools> {
        let codec = ConstFrameCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            1,
        );
        let hooks: BoxedEventSink = Box::new(LoggingHooks { log });
        ComposedDecoder::new(
            demuxer,
            codec,
            DecoderRuntime {
                hooks: Some(hooks),
                ..DecoderRuntime::for_test()
            },
        )
    }

    #[kithara::test]
    #[case::chunk_signal(
        StubOutcome::Frame {
            pts: Duration::ZERO,
            duration: Duration::from_millis(20),
        },
        "chunk"
    )]
    #[case::pending_signal(StubOutcome::Pending(PendingReason::SeekPending), "pending")]
    fn next_chunk_emits_signal(
        #[case] outcome: StubOutcome,
        #[case] expected_signal: &str,
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(zero_packet, vec![outcome], Vec::new());
        let mut decoder = build(trim_silence.clone(), demuxer, Arc::clone(&log));
        let _ = decoder.next_chunk().unwrap();
        assert_eq!(log.lock().unwrap().chunks, vec![expected_signal]);
    }

    #[kithara::test]
    fn zero_frame_codec_yields_pending_before_demux_eof(
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        let outcomes = (0..=ZERO_FRAME_BUDGET)
            .map(|index| StubOutcome::Frame {
                pts: Duration::from_millis(u64::from(index)),
                duration: Duration::from_millis(1),
            })
            .collect();
        let demuxer = StubDemuxer::with_outcomes(zero_packet, outcomes, Vec::new());
        let codec = ConstFrameCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            0,
        );
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let outcome = decoder.next_chunk().expect("zero-frame decode");

        assert!(matches!(
            outcome,
            DecoderChunkOutcome::Pending(PendingReason::NotReady(NotReadyCause::SourcePending))
        ));
        assert_eq!(decoder.demuxer.next.len(), 1);
    }

    #[kithara::test]
    #[case::landed(
        DemuxSeekOutcome::Landed {
            landed_at: Duration::from_secs(1),
            landed_byte: Some(123),
            preroll: crate::demuxer::PrerollHint::NotNeeded,
        },
        Duration::from_secs(1),
        "landed"
    )]
    #[case::past_eof(
        DemuxSeekOutcome::PastEof {
            duration: Duration::from_secs(10),
        },
        Duration::from_secs(15),
        "past_eof"
    )]
    fn seek_emits_signal(
        #[case] outcome: DemuxSeekOutcome,
        #[case] target: Duration,
        #[case] expected_signal: &str,
        trim_silence: Vec<f32>,
        zero_packet: &'static [u8],
    ) {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(zero_packet, Vec::new(), vec![outcome]);
        let mut decoder = build(trim_silence.clone(), demuxer, Arc::clone(&log));
        let _ = decoder.seek(target).unwrap();
        assert_eq!(log.lock().unwrap().seeks, vec![expected_signal]);
    }

    #[kithara::test]
    fn owned_hooks_fire_exactly_once_per_chunk(trim_silence: Vec<f32>, zero_packet: &'static [u8]) {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(
            zero_packet,
            vec![
                StubOutcome::Frame {
                    pts: Duration::from_millis(40),
                    duration: Duration::from_millis(20),
                },
                StubOutcome::Frame {
                    pts: Duration::from_millis(20),
                    duration: Duration::from_millis(20),
                },
                StubOutcome::Frame {
                    pts: Duration::ZERO,
                    duration: Duration::from_millis(20),
                },
            ],
            Vec::new(),
        );
        let mut decoder = build(trim_silence.clone(), demuxer, Arc::clone(&log));

        for _ in 0..3 {
            let _ = decoder.next_chunk().unwrap();
        }
        let _ = decoder.next_chunk().unwrap();

        // WHY: single-owner Box hooks must fire once per next_chunk — three
        // PCM chunks then one Eof, never doubled, never dropped.
        assert_eq!(
            log.lock().unwrap().chunks,
            vec!["chunk", "chunk", "chunk", "eof"],
        );
    }
}

#[cfg(test)]
mod pool_budget_tests {
    use std::num::NonZeroU32;

    use kithara_platform::time::Duration;
    use kithara_signal::AudioSpec;
    use kithara_test_fixtures::unit_fixtures::trim_silence;
    use kithara_test_utils::kithara;

    use super::test_stub_codec::ConstFrameCodec;
    use crate::{codec::FrameCodec, test_pools::pools};

    #[kithara::test]
    fn codec_warm_pool_keeps_allocated_bytes_stable(trim_silence: Vec<f32>) {
        let pools = pools();
        for _ in 0..4 {
            let mut buf = pools.get::<f32>();
            buf.ensure_len(2048).unwrap();
        }
        let warmup_bytes = pools.stats().allocated_bytes;

        let mut codec = ConstFrameCodec::new(
            trim_silence.clone(),
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            1024,
        );
        for _ in 0..200 {
            let mut buf = pools.get::<f32>();
            let frames = codec
                .decode_frame(&[], Duration::ZERO, &[], &mut buf)
                .expect("BUG: decode_frame");
            assert_eq!(frames, 1024);
        }

        assert_eq!(pools.stats().allocated_bytes, warmup_bytes);
    }
}
