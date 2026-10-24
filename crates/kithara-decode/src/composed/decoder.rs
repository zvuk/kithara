use std::sync::atomic::Ordering;

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{
    AudioCodec, NotReadyCause, PendingReason, ReaderChunkSignal, ReaderSeekSignal,
};
use kithara_test_utils::kithara;

use super::{ComposedDecoder, DecoderRuntime, HeadStrip, PcmOriginState, Prefill, state::SeekTrim};
use crate::{
    InputState, PcmOrigin, PrepareWait,
    codec::FrameCodec,
    consts,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    error::{DecodeError, DecodeResult},
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
};

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
            origin: PcmOriginState::Observing(None),
            pools: runtime.pools,
            output: None,
            prefill: Prefill::Needed,
            byte_len_handle: runtime.byte_len_handle,
            hooks: runtime.hooks,
            frame_offset: 0,
            pending_seek_target: None,
            timeline: None,
            timeline_gap_frames: 0,
            head_strip: HeadStrip::default(),
            zero_frame_count: 0,
        }
    }

    fn source_media_origin(&self) -> Option<Duration> {
        self.demuxer
            .media_origin()?
            .checked_sub(self.codec.media_time_rebase()?)
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
        packet: crate::demuxer::PacketMetadata,
    ) -> AudioChunk {
        if let PcmOriginState::Observing(packet) = self.origin {
            self.origin = packet.map_or(PcmOriginState::Sealed, |packet| {
                PcmOriginState::Captured(PcmOrigin::new(timestamp, timestamp.min(packet)))
            });
        }
        let live_spec = self.codec.spec();
        self.spec = live_spec;

        let timestamp_frame = live_spec.frame_at(timestamp).unwrap_or(u64::MAX);
        let (anchor, anchor_frame) = match self.timeline {
            Some(anchor) if timestamp_frame <= self.frame_offset => anchor,
            Some(_) => {
                self.timeline_gap_frames = self
                    .timeline_gap_frames
                    .saturating_add(timestamp_frame.saturating_sub(self.frame_offset));
                self.frame_offset = timestamp_frame;
                (timestamp, timestamp_frame)
            }
            None => {
                self.frame_offset = timestamp_frame;
                (timestamp, timestamp_frame)
            }
        };
        self.timeline = Some((anchor, anchor_frame));
        let frame_offset = self.frame_offset;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(frames));
        let elapsed = live_spec
            .duration_for(frame_offset)
            .unwrap_or(Duration::MAX)
            .saturating_sub(
                live_spec
                    .duration_for(anchor_frame)
                    .unwrap_or(Duration::MAX),
            );
        let timestamp = anchor.saturating_add(elapsed);

        let chunk_secs = f64::from(frames) / f64::from(live_spec.sample_rate.get());
        let frame_duration = Duration::from_secs_f64(chunk_secs);
        let end_timestamp = timestamp.saturating_add(frame_duration);

        let meta = AudioChunkInfo {
            end_timestamp,
            timestamp,
            media_origin: self.source_media_origin(),
            frames,
            frame_offset,
            source_bytes: packet.source_bytes,
            segment_index: packet.segment_index,
            source_byte_offset: None,
            variant_index: packet.variant_index,
            spec: live_spec,
            render_revision: 0,
            mapping_revision: None,
            ..AudioChunkInfo::default()
        };
        AudioChunk::new(meta, buf)
    }

    fn decode_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let output = self.output.take().ok_or(DecodeError::InvalidData {
            detail: "decoder output was not prepared",
        })??;
        self.output = Some(Ok(output));
        self.next_chunk_inner()
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
            DecoderSeekOutcome::Pending(_) => return,
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

    /// A head-trimmed packet keeps its end time, so the missing prefix precedes its PCM. Duration
    /// rounding can leave the frame's end past the target even when the packet is fully pre-target
    /// in sample space.
    #[kithara::measure(label = "decode.composed.next")]
    #[kithara::hang_watchdog]
    fn next_chunk_inner(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            hang_tick!();
            let mut at_eof = false;
            let needs_eof_drain = self
                .codec
                .needs_eof_drain(self.demuxer.track_info().sample_rate);
            let mut packet = crate::demuxer::PacketMetadata {
                segment_index: self.demuxer.current_segment_index(),
                variant_index: self.demuxer.current_variant_index(),
                source_bytes: 0,
            };
            let frame = if self.codec.has_output() {
                None
            } else {
                Some(match self.demuxer.next_frame_prepared()? {
                    DemuxOutcome::Frame(frame) => frame,
                    DemuxOutcome::Pending(reason) => {
                        return Ok(DecoderChunkOutcome::Pending(reason));
                    }
                    DemuxOutcome::Eof => {
                        if !needs_eof_drain {
                            return Ok(DecoderChunkOutcome::Eof);
                        }
                        at_eof = true;
                        crate::demuxer::Frame {
                            data: &[],
                            packet_desc: &[],
                            duration: Duration::ZERO,
                            pts: self.spec.duration_for(self.frame_offset)?,
                        }
                    }
                })
            };
            hang_reset!();
            self.origin.observe_packet(frame, at_eof);
            let mut buf = self.output.take().ok_or(DecodeError::InvalidData {
                detail: "decoder output was not prepared",
            })??;
            if let Some(frame) = frame {
                packet.source_bytes = u64::try_from(frame.data.len()).unwrap_or(u64::MAX);
            }
            let result = match frame {
                Some(frame) => self.codec.decode_frame(frame, packet, &mut buf),
                None => self.codec.take_output(&mut buf),
            };
            let decoded = match result {
                Ok(decoded) => decoded,
                Err(error) => {
                    self.output = Some(Ok(buf));
                    return Err(error);
                }
            };
            let mut frames = decoded.frames;
            if at_eof && frames == 0 {
                self.output = Some(Ok(buf));
                return Ok(DecoderChunkOutcome::Eof);
            }
            let frame_pts = match frame {
                Some(frame) => frame.pts,
                None => self.codec.decoded_pts().ok_or(DecodeError::InvalidData {
                    detail: "buffered codec output has no presentation timestamp",
                })?,
            };
            let frame_duration = frame.map_or_else(
                || self.codec.spec().duration_for(u64::from(frames)),
                |frame| Ok(frame.duration),
            )?;
            let frame_end = frame_pts.saturating_add(frame_duration);
            let prior_head_strip = self.head_strip.frames();
            let decoded_pts = self.codec.decoded_pts();
            if decoded_pts.is_none() {
                self.head_strip.record(
                    self.spec.frame_at(frame_duration).unwrap_or(u64::MAX),
                    u64::from(frames),
                );
            }
            let zero_frame_budget_reached = if frames == 0 {
                self.zero_frame_count = self.zero_frame_count.saturating_add(1);
                self.zero_frame_count >= consts::ZERO_FRAME_BUDGET
            } else {
                self.zero_frame_count = 0;
                false
            };
            let mut chunk_pts = self.chunk_pts(frame_pts, frames, decoded_pts, prior_head_strip);
            if self.trim_seek_chunk(&mut buf, &mut frames, &mut chunk_pts, frame_end)? {
                self.output = Some(Ok(buf));
                if zero_frame_budget_reached {
                    return Ok(DecoderChunkOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::SourcePending,
                    )));
                }
                continue;
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
            let chunk = self.build_chunk(buf, frames, chunk_pts, decoded.metadata);
            return Ok(DecoderChunkOutcome::Chunk(Box::new(chunk)));
        }
    }

    fn chunk_pts(
        &self,
        frame_pts: Duration,
        frames: u32,
        decoded_pts: Option<Duration>,
        prior_head_strip: u64,
    ) -> Duration {
        if frames == 0 {
            frame_pts
        } else if let Some(decoded_pts) = decoded_pts {
            decoded_pts
        } else {
            let stripped = self.head_strip.frames().saturating_sub(prior_head_strip);
            frame_pts.saturating_add(
                self.codec
                    .spec()
                    .duration_for(stripped)
                    .unwrap_or(Duration::MAX),
            )
        }
    }

    fn seek_selected(
        &mut self,
        pos: Duration,
        prefix: bool,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        if !pos.is_zero() && matches!(self.origin, PcmOriginState::Observing(_)) {
            self.origin = PcmOriginState::Sealed;
        }
        let priming = self.codec.priming(self.demuxer.track_info().codec);
        let outcome = self.demuxer.seek_with_wait(pos, priming, wait)?;
        self.codec.flush()?;
        self.head_strip = HeadStrip::default();
        self.zero_frame_count = 0;
        self.pending_seek_target = Some(if prefix {
            SeekTrim::Prefix(pos)
        } else {
            SeekTrim::Nearest(pos)
        });
        if matches!(self.output, Some(Err(_))) {
            self.output = None;
        }
        Ok(self.apply_seek_outcome(outcome))
    }

    fn apply_seek_outcome(&mut self, outcome: DemuxSeekOutcome) -> DecoderSeekOutcome {
        self.prefill = match outcome {
            DemuxSeekOutcome::Pending(reason) => Prefill::Seeking(reason),
            DemuxSeekOutcome::Landed { .. } | DemuxSeekOutcome::PastEof { .. } => Prefill::Needed,
        };
        let outcome = match outcome {
            DemuxSeekOutcome::Pending(reason) => DecoderSeekOutcome::Pending(reason),
            DemuxSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
            } => {
                self.frame_offset = self.spec.frame_at(landed_at).unwrap_or(u64::MAX);
                self.timeline = None;
                self.timeline_gap_frames = 0;
                DecoderSeekOutcome::Landed {
                    landed_byte,
                    landed_at,
                    preroll,
                    landed_frame: self.frame_offset,
                    clock: crate::LandingClock::Kept,
                }
            }
            DemuxSeekOutcome::PastEof { duration } => {
                self.pending_seek_target = None;
                DecoderSeekOutcome::PastEof { duration }
            }
        };
        self.emit_seek_signal(&outcome);
        outcome
    }
}

impl<D, C, S> Decoder for ComposedDecoder<D, C, S>
where
    D: Demuxer + 'static,
    C: FrameCodec,
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn duration(&self) -> Option<crate::DecoderDuration> {
        self.duration
    }

    fn media_origin(&self) -> Option<Duration> {
        self.source_media_origin()
    }

    fn initial_pcm_origin(&self) -> Option<PcmOrigin> {
        match self.origin {
            PcmOriginState::Captured(origin) => Some(origin),
            PcmOriginState::Observing(_) | PcmOriginState::Sealed => None,
        }
    }

    fn flush_reader_signals(&mut self) {
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.flush();
        }
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if matches!(self.prefill, Prefill::Seeking(_)) {
            return self.next_chunk_prepared();
        }
        loop {
            self.prepare_next_chunk();
            match self.next_chunk_prepared()? {
                DecoderChunkOutcome::Pending(PendingReason::Retry) => {}
                outcome => return Ok(outcome),
            }
        }
    }

    fn next_chunk_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if let Prefill::Seeking(reason) = self.prefill {
            let outcome = DecoderChunkOutcome::Pending(reason);
            self.emit_chunk_signal(&outcome);
            return Ok(outcome);
        }
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
        if matches!(outcome, DecoderChunkOutcome::Pending(_)) {
            self.prefill = Prefill::Needed;
        }
        self.emit_chunk_signal(&outcome);
        Ok(outcome)
    }

    fn has_prepared_result(&self) -> bool {
        matches!(
            self.prefill,
            Prefill::Buffered(
                Ok(DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Eof) | Err(_)
            )
        ) || matches!(self.output, Some(Err(_)))
    }

    fn prepare_next_chunk(&mut self) {
        self.prepare_next_chunk_with_wait(PrepareWait::Wait);
    }

    fn prepare_next_chunk_with_wait(&mut self, wait: PrepareWait) {
        if self.has_prepared_result() || matches!(self.prefill, Prefill::Seeking(_)) {
            return;
        }
        if matches!(self.prefill, Prefill::Buffered(_)) {
            self.prefill = Prefill::Needed;
        }
        if !self.codec.has_output()
            && let Err(error) = self.demuxer.prepare_frame_with_wait(wait)
        {
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

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, false, PrepareWait::Wait)
    }

    fn seek_with_wait(
        &mut self,
        pos: Duration,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, false, wait)
    }

    fn seek_prefix(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, true, PrepareWait::Wait)
    }

    fn seek_prefix_with_wait(
        &mut self,
        pos: Duration,
        wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(pos, true, wait)
    }

    fn prepare_seek(&mut self) -> DecodeResult<Option<DecoderSeekOutcome>> {
        self.prepare_seek_with_wait(PrepareWait::Wait)
    }

    fn prepare_seek_with_wait(
        &mut self,
        wait: PrepareWait,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        Ok(self
            .demuxer
            .prepare_seek_with_wait(wait)?
            .map(|outcome| self.apply_seek_outcome(outcome)))
    }

    fn release_input(&mut self) -> InputState {
        self.demuxer.release_input()
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
            byte_len_handle: None,
            hooks: None,
        }
    }
}
