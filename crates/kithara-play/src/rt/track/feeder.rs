use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_audio::{RevisionFloorStatus, SourceEnd, SourceSpan};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_events::TrackId;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};
use kithara_signal::FrameCount;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, RenderReader};

use super::feeder_read::ScheduledSeekPresentation;

#[rustfmt::skip]
use crate::resource::Resource;
use crate::{
    bridge::{RtMetrics, ScheduledSeekDisposition},
    resource::RenderActivation,
    worker::ServiceClass,
};

/// RT-safe resource wrapper with internal scratch buffers.
///
/// Wraps a [`Resource`] and maintains per-channel scratch buffers
/// that are filled from the underlying `AudioReader`. The audio thread
/// reads from these buffers, avoiding direct interaction with the
/// potentially-blocking decoder on every callback.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerResource {
    #[field(get, deref = false)]
    pub(super) src: Arc<str>,
    pub(super) last_source_end: Option<SourceEnd>,
    pub(super) last_warp_map_revision: u64,
    pub(super) source_spans: VecDeque<SourceWindow>,
    pub(super) resource: WasmSend<Resource>,
    pub(super) channel_buffers: [SampleBuffer; Self::STEREO_CHANNELS],
    pub(super) activation_tail: Option<[SampleBuffer; Self::STEREO_CHANNELS]>,
    pub(super) activation_blend_frames: usize,
    pub(super) activation_blend_pos: usize,
    pub(super) eof_seen: bool,
    pub(super) failed: bool,
    pub(super) render_revision_floor: u64,
    pub(super) scheduled_seek: Option<(u64, ScheduledSeekDisposition, bool)>,
    pub(super) write_len: usize,
    pub(super) write_pos: usize,
}

#[derive(Clone, Copy)]
pub(super) struct SourceWindow {
    pub(super) source: Option<SourceSpan>,
    pub(super) frames: usize,
    pub(super) media_frames: u64,
    pub(super) consumed_frames: usize,
}

impl SourceWindow {
    pub(super) fn remaining(&self) -> usize {
        self.frames - self.consumed_frames
    }

    pub(super) fn source_for(&self, frames: usize) -> Option<SourceSpan> {
        let source = self.source?;
        let start = partial_source_end(source, self.consumed_frames, self.frames)?;
        let end = partial_source_end(
            source,
            self.consumed_frames + frames.min(self.remaining()),
            self.frames,
        )?;
        SourceSpan::new(start, end, source.sample_rate())
            .map(|span| span.with_render_revision(source.render_revision()))
    }

    pub(super) fn take(&mut self, frames: usize) -> (Option<SourceSpan>, u64) {
        let consumed = frames.min(self.remaining());
        let taken = self.source_for(consumed);
        let before = partial_frames(self.media_frames, self.consumed_frames, self.frames);
        self.consumed_frames += consumed;
        let after = partial_frames(self.media_frames, self.consumed_frames, self.frames);
        (taken, after - before)
    }
}

pub(super) fn partial_frames(total: u64, frames: usize, span_frames: usize) -> u64 {
    let numerator = u128::from(total) * u128::try_from(frames).unwrap_or(u128::MAX);
    let denominator = u128::try_from(span_frames).unwrap_or(u128::MAX);
    u64::try_from(numerator / denominator).unwrap_or(total)
}

pub(super) fn partial_source_end(
    source: SourceSpan,
    frames: usize,
    span_frames: usize,
) -> Option<u64> {
    let source_frames = source.end().checked_sub(source.start())?;
    let numerator = u128::from(source_frames).checked_mul(u128::try_from(frames).ok()?)?;
    let denominator = u128::try_from(span_frames).ok()?;
    let consumed = u64::try_from(numerator.checked_div(denominator)?).ok()?;
    source.start().checked_add(consumed)
}

pub(super) fn activation_prefix(
    context: &RenderContext,
    activation: RenderActivation,
) -> Option<usize> {
    let start = i64::from(context.output_frames().start);
    let end = i64::from(context.output_frames().end);
    let output = i64::from(activation.output);
    if output >= end {
        return None;
    }
    if output <= start {
        return Some(0);
    }
    usize::try_from(output - start).ok()
}

pub(super) fn combine_reads(
    prefix_frames: usize,
    prefix_source_frames: u64,
    suffix: ReadOutcome,
    suffix_source_frames: u64,
) -> (ReadOutcome, u64) {
    let Some(source_frames) = prefix_source_frames.checked_add(suffix_source_frames) else {
        return (ReadOutcome::Failed, 0);
    };
    let outcome = match suffix {
        ReadOutcome::Full { frames } => ReadOutcome::Full {
            frames: prefix_frames.saturating_add(frames),
        },
        ReadOutcome::Partial { frames } => ReadOutcome::Partial {
            frames: prefix_frames.saturating_add(frames),
        },
        ReadOutcome::Eof => ReadOutcome::Partial {
            frames: prefix_frames,
        },
        ReadOutcome::Failed => ReadOutcome::Failed,
    };
    (outcome, source_frames)
}

/// Result of a bounded audio-thread read from [`PlayerResource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The requested range was filled completely.
    ///
    /// `frames` counts real audio frames copied out of the wrapped reader or
    /// scratch buffer. The remainder may be zero-filled during a non-terminal
    /// underrun and must not advance playback position.
    Full { frames: usize },
    /// A strict prefix of the requested range was written.
    ///
    /// The payload is the number of written frames. This outcome is reserved
    /// for natural EOF inside the requested block; the next read must return
    /// [`ReadOutcome::Eof`].
    Partial { frames: usize },
    /// The resource was already drained and nothing was written.
    Eof,
    /// The underlying decoder/source reported a non-recoverable error
    /// mid-stream. Distinct from [`Eof`](Self::Eof): the track did NOT
    /// reach its natural end — surface this as a track-failed signal
    /// upstream instead of letting the queue auto-advance as if the
    /// track played out.
    Failed,
}

impl PlayerResource {
    /// Buffer duration divisor: `sample_rate` / `BUFFER_DURATION_DIVISOR` gives ~200ms of frames.
    pub(super) const BUFFER_DURATION_DIVISOR: usize = 5;

    /// Number of stereo output channels.
    pub(super) const STEREO_CHANNELS: usize = 2;

    /// Create a new `PlayerResource` wrapping the given resource.
    ///
    /// Allocates two per-channel scratch buffers through the given pool facade,
    /// each holding [`Self::scratch_frames`] frames.
    pub fn new<S>(
        resource: Resource,
        src: Arc<str>,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let buffer_frames = Self::scratch_frames(resource.spec().sample_rate.get()).get();
        let left = pools.get_with_len::<f32>(buffer_frames)?;
        let right = pools.get_with_len::<f32>(buffer_frames)?;
        let activation_blend_frames = resource
            .activation_blend_frames()
            .map_or(0, NonZeroUsize::get);
        let activation_tail = if activation_blend_frames == 0 {
            None
        } else {
            Some([
                pools.get_with_len::<f32>(activation_blend_frames)?,
                pools.get_with_len::<f32>(activation_blend_frames)?,
            ])
        };

        Ok(Self {
            src,
            channel_buffers: [left, right],
            activation_tail,
            activation_blend_frames,
            activation_blend_pos: activation_blend_frames,
            source_spans: VecDeque::with_capacity(buffer_frames),
            resource: WasmSend::new(resource),
            write_len: 0,
            write_pos: 0,
            last_source_end: None,
            last_warp_map_revision: 0,
            eof_seen: false,
            failed: false,
            render_revision_floor: 0,
            scheduled_seek: None,
        })
    }

    delegate::delegate! {
        to self.resource.get() {
            pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32;
            pub(crate) fn playback_rate(&self) -> f32;
        }
    }

    /// Cached span in seconds: how much of the source is on disk and needs no
    /// further network.
    #[must_use]
    pub fn cached_span(&self) -> f64 {
        self.resource.get().cached_span().as_secs_f64()
    }

    pub(super) fn consume_source(
        &mut self,
        mut frames: usize,
        context: Option<&RenderContext>,
        track_id: Option<TrackId>,
    ) -> Option<u64> {
        let mut source_frames = 0u64;
        let mut output_start = 0usize;
        while frames > 0 {
            let mut span = self.source_spans.pop_front()?;
            let consumed = frames.min(span.remaining());
            let output_end = output_start.saturating_add(consumed);
            let (source, consumed_source_frames) = span.take(consumed);
            source_frames = source_frames.checked_add(consumed_source_frames)?;
            match (context, source) {
                (Some(context), Some(source)) => {
                    kithara::probe_event!(
                        pcm_consumed,
                        track_id = track_id.map(TrackId::as_u64),
                        render_revision = source.render_revision(),
                        output_start = i64::from(context.output_frames().start)
                            .saturating_add(i64::try_from(output_start).unwrap_or(i64::MAX)),
                        output_end = i64::from(context.output_frames().start)
                            .saturating_add(i64::try_from(output_end).unwrap_or(i64::MAX)),
                        source_start = source.start(),
                        source_end = source.end()
                    );
                    self.last_source_end = Some(SourceEnd::new(source.end(), source.sample_rate()));
                    self.last_warp_map_revision =
                        kithara_signal::render_warp_map_revision(source.render_revision());
                }
                (None, Some(source)) => {
                    self.last_source_end = Some(SourceEnd::new(source.end(), source.sample_rate()));
                    self.last_warp_map_revision =
                        kithara_signal::render_warp_map_revision(source.render_revision());
                }
                (_, None) => {}
            }
            frames -= consumed;
            output_start = output_end;
            if span.remaining() > 0 {
                self.source_spans.push_front(span);
            }
        }
        Some(source_frames)
    }

    /// Decoded-ahead frontier in seconds: how much content has been decoded
    /// and is ready to play (always `>=` the served playback position).
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource.get().decoded_frontier().as_secs_f64()
    }

    pub(super) fn fill_scratch(&mut self, target_frames: usize, metrics: &RtMetrics) -> bool {
        let mut eof_reached = self.eof_seen;

        while target_frames > self.write_len && !eof_reached {
            let needed = target_frames - self.write_len;
            let avail = (self.channel_buffers[0].len() - self.write_pos).min(needed);
            if avail == 0 {
                break;
            }

            let channel_buffers = &mut self.channel_buffers;
            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];

            let position_before = self.resource.get().position();
            let (n, position, source) = match self.resource.get_mut().read_planar(&mut planar) {
                Ok(kithara_audio::ReadOutcome::Frames {
                    count,
                    position,
                    source_span,
                }) => (count.get(), position, source_span),
                Ok(kithara_audio::ReadOutcome::Pending { position, .. }) => (0, position, None),
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    (0, position_before, None)
                }
                Err(_) => {
                    metrics.record_decode_error();
                    self.failed = true;
                    (0, position_before, None)
                }
            };
            if n == 0 {
                break;
            }
            if source
                .is_some_and(|span| span.sample_rate() != self.resource.get().spec().sample_rate)
            {
                metrics.record_decode_error();
                self.failed = true;
                break;
            }
            let media_frames = source.map_or_else(
                || {
                    let spec = self.resource.get().spec();
                    spec.frame_at(position)
                        .ok()
                        .zip(spec.frame_at(position_before).ok())
                        .map_or(0, |(end, start)| end.saturating_sub(start))
                },
                |span| span.end().saturating_sub(span.start()),
            );
            self.source_spans.push_back(SourceWindow {
                source,
                frames: n,
                media_frames,
                consumed_frames: 0,
            });
            self.write_len += n;
            self.write_pos += n;
        }

        eof_reached
    }

    pub(super) fn prefetch_target(&self, callback_frames: usize) -> usize {
        self.write_len
            .saturating_add(callback_frames)
            .min(self.channel_buffers[0].len())
    }

    /// Remaining buffered frames when the wrapped reader has reached EOF.
    ///
    /// `Some(0)` means the current read drained the last buffered frame exactly;
    /// the next read will return [`ReadOutcome::Eof`].
    #[must_use]
    pub fn frames_until_eof(&self) -> Option<usize> {
        self.eof_seen.then_some(self.write_len)
    }

    pub(crate) fn presentation_source_end(
        &self,
        sample_rate: NonZeroU32,
    ) -> Option<(SourceEnd, u64)> {
        let source_end = self.last_source_end?;
        (source_end.sample_rate() == sample_rate
            && source_end.sample_rate() == self.resource.get().spec().sample_rate)
            .then_some((source_end, self.last_warp_map_revision))
    }

    pub(super) fn sync_render_revision(
        &mut self,
        activation: RenderActivation,
        required_frames: NonZeroUsize,
    ) -> RevisionFloorStatus {
        let revision = activation.revision;
        if revision <= self.render_revision_floor {
            return RevisionFloorStatus::Current;
        }
        let status = self.resource.get_mut().sync_render_revision(
            revision,
            required_frames,
            self.last_source_end,
        );
        if matches!(
            status,
            RevisionFloorStatus::WaitingForReplacement
                | RevisionFloorStatus::ReadyForSeekPresentation
        ) {
            return status;
        }
        self.render_revision_floor = revision;
        if self.source_spans.iter().any(|span| {
            span.source
                .is_some_and(|source| source.render_revision() < revision)
        }) {
            self.source_spans.clear();
            self.write_len = 0;
            self.write_pos = 0;
        }
        status
    }

    pub(crate) fn present_scheduled_seek(&mut self) -> ScheduledSeekPresentation {
        let Some((epoch, disposition, _)) = self.scheduled_seek else {
            return ScheduledSeekPresentation::NoRequest;
        };
        match self.resource.get_mut().present_seek(epoch) {
            kithara_audio::SeekPresentation::Presented
            | kithara_audio::SeekPresentation::Current => {
                self.scheduled_seek = None;
                self.source_spans.clear();
                self.write_len = 0;
                self.write_pos = 0;
                self.last_source_end = None;
                self.eof_seen = false;
                self.failed = false;
                ScheduledSeekPresentation::Presented(disposition)
            }
            kithara_audio::SeekPresentation::Superseded => {
                self.scheduled_seek = None;
                ScheduledSeekPresentation::Superseded
            }
        }
    }

    pub(crate) const fn schedule_seek(
        &mut self,
        epoch: u64,
        disposition: ScheduledSeekDisposition,
        armed: bool,
    ) {
        self.scheduled_seek = Some((epoch, disposition, armed));
    }

    pub(crate) fn set_prepared_launch_armed(&mut self, armed: bool) -> bool {
        let Some((epoch, disposition, current)) = self.scheduled_seek else {
            return false;
        };
        if !disposition.is_prepared_launch() {
            return false;
        }
        if current != armed {
            self.scheduled_seek = Some((epoch, disposition, armed));
        }
        true
    }

    pub(crate) fn has_prepared_launch(&self, epoch: u64) -> bool {
        let Some((scheduled_epoch, disposition, _)) = self.scheduled_seek else {
            return false;
        };
        scheduled_epoch == epoch && disposition.is_prepared_launch()
    }

    pub(crate) fn present_replacement_prepared_launch(
        &mut self,
        prepared_epoch: u64,
        replacement_epoch: u64,
    ) -> bool {
        if !self.has_prepared_launch(prepared_epoch) {
            return false;
        }
        match self.resource.get_mut().present_seek(replacement_epoch) {
            kithara_audio::SeekPresentation::Presented
            | kithara_audio::SeekPresentation::Current => true,
            kithara_audio::SeekPresentation::Superseded => false,
        }
    }

    pub(crate) fn clear_prepared_launch(&mut self, epoch: u64) {
        if self.has_prepared_launch(epoch) {
            self.scheduled_seek = None;
        }
    }

    pub(crate) fn render_reader(&self) -> Option<RenderReader> {
        self.resource.get().render_reader()
    }

    /// Drop everything buffered ahead of a seek the control thread began. Lock-free: the reader
    /// picks up the epoch itself via `sync_seek`.
    pub fn reset_for_seek(&mut self) {
        self.resource.get_mut().defer_seek_until_pcm();
        self.write_len = 0;
        self.write_pos = 0;
        self.source_spans.clear();
        self.last_source_end = None;
        self.resource.get().clear_render();
        self.eof_seen = false;
        self.failed = false;
        self.activation_blend_pos = self.activation_blend_frames;
    }

    pub(super) const fn scratch_frames(sample_rate: u32) -> FrameCount {
        FrameCount::new(sample_rate as usize / Self::BUFFER_DURATION_DIVISOR)
    }

    /// Control-plane handle used to begin a seek off the audio thread.
    #[must_use]
    pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>> {
        self.resource.get().seek_handle()
    }

    delegate::delegate! {
        to self.resource.get() {
            /// Total duration in seconds. Returns 0.0 if unknown.
            #[must_use]
            #[expr($.map_or(0.0, |d| d.as_secs_f64()))]
            pub fn duration(&self) -> f64;
            /// Set the target sample rate of the audio host.
            pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Update the scheduling priority hint for the shared worker.
            pub(crate) fn set_service_class(&self, class: ServiceClass);
            pub(crate) fn clear_render(&self);
            pub(crate) fn publish_render(
                &self,
                context: &RenderContext,
                frontier: PresentationFrontier,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_signal::{AudioSpec, FrameCount, SampleCount};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(44_100, 8_820)]
    #[case(48_000, 9_600)]
    #[case(96_000, 19_200)]
    fn scratch_holds_200ms_of_frames(#[case] sample_rate: u32, #[case] expected: usize) {
        assert_eq!(
            PlayerResource::scratch_frames(sample_rate),
            FrameCount::new(expected)
        );
    }

    #[kithara::test]
    fn an_interleaved_length_is_not_a_frame_count() {
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate is non-zero"));
        let frames = PlayerResource::scratch_frames(48_000);
        assert_eq!(
            spec.sample_count(frames),
            Ok(SampleCount::new(frames.get() * 2))
        );
    }

    #[kithara::test]
    fn partial_scratch_source_position_is_independent_of_read_partition() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let source = SourceSpan::new(100, 107, rate);
        let mut span = SourceWindow {
            source,
            frames: 10,
            media_frames: 7,
            consumed_frames: 0,
        };
        let mut media_frames = 0;
        for output_frame in 1..=10 {
            let (taken, consumed) = span.take(1);
            media_frames += consumed;
            let expected = 7 * output_frame / 10;
            assert_eq!(
                taken.map(|source| source.end()),
                Some(100 + expected),
                "source frontier after {output_frame} output frames"
            );
            assert_eq!(media_frames, expected);
        }
    }

    #[kithara::test]
    #[case(100, Some(0))]
    #[case(105, Some(5))]
    #[case(110, None)]
    fn activation_partition_uses_the_exact_host_frame(
        #[case] activation_output: i64,
        #[case] expected_prefix: Option<usize>,
    ) {
        let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let context = RenderContext::new(
            kithara_warp::SessionFrame::new(100)..kithara_warp::SessionFrame::new(110),
            sample_rate,
            None,
            kithara_warp::SessionEpoch::new(1),
            None,
        )
        .expect("fixture context is valid");
        let activation = RenderActivation {
            output: kithara_warp::SessionFrame::new(activation_output),
            revision: 7,
        };

        assert_eq!(activation_prefix(&context, activation), expected_prefix);
    }

    #[kithara::test]
    fn partial_scratch_consumption_preserves_render_provenance() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let revision = kithara_signal::pack_render_revision(7, 11)
            .expect("fixture revisions fit the provenance word");
        let source =
            SourceSpan::new(100, 130, rate).map(|span| span.with_render_revision(revision));
        let mut span = SourceWindow {
            source,
            frames: 10,
            media_frames: 30,
            consumed_frames: 0,
        };

        assert_eq!(
            span.take(4),
            (
                SourceSpan::new(100, 112, rate).map(|span| span.with_render_revision(revision)),
                12
            )
        );
        assert_eq!(span.source_for(6).map(|source| source.start()), Some(112));
        assert_eq!(
            span.take(6),
            (
                SourceSpan::new(112, 130, rate).map(|span| span.with_render_revision(revision)),
                18
            )
        );
    }
}
