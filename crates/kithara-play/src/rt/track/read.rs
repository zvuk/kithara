use std::ops::Range;

use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, WarpMapRevision};
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapProd, traits::Producer};

use super::{
    PlayerTrack, ReadOutcome, RtSink, SyncFadeTail,
    triggers::{TrackTriggers, TriggerInput, TriggerTrack},
};
use crate::bridge::{
    PlaybackFault, PlayerNotification, RtMetrics, TrackPlaybackStopReason, TrackState,
    sync::PreparedFirst,
};

struct TrackReadContext<'a> {
    range: Range<usize>,
    sink: RtSink<'a>,
}

#[derive(Clone, Copy)]
struct PartialRead {
    duration: f64,
    frames: usize,
}

/// Result of a single track render attempt.
#[derive(Debug)]
pub enum TrackReadOutcome {
    /// The full requested block was written into the mix buffer.
    Full {
        /// Playback position snapshot after the read (seconds).
        position: f64,
        /// Real audio frames copied from the underlying resource/scratch buffer.
        frames: usize,
        /// Visible duration snapshot in seconds.
        duration: f64,
        /// Exact remaining buffered frames after EOF has been observed.
        frames_until_eof: Option<usize>,
    },
    /// Only the first `frames` samples were written; EOF was reached in-block.
    Partial {
        /// Number of frames written into the destination block.
        frames: usize,
        /// Visible duration snapshot in seconds.
        duration: f64,
    },
    /// No frames were written because the track is already finished.
    Eof,
    /// The source reported a non-recoverable error mid-stream, or the render
    /// context could not serve this track. The payload names which.
    Failed(PlaybackFault),
}

impl PlayerTrack {
    /// Consume the already-decoded activation frame before the gate is released.
    pub(crate) fn render_first(
        &mut self,
        first: &PreparedFirst,
        context: &RenderContext,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        at: usize,
        sink: &mut RtSink<'_>,
    ) {
        scratch_bufs[0][at] = first.stereo[0];
        scratch_bufs[1][at] = first.stereo[1];
        self.fade.mix_range(scratch_bufs, mix_bufs, at..at + 1, 1);
        self.advance_media_clock(1);
        self.resource.publish_render(
            context,
            PresentationFrontier::builder()
                .source(first.source.end())
                .output(context.output().output_frames().end)
                .build()
                .with_warp_map(first.source.mapping_revision().map(WarpMapRevision::from)),
        );
        self.update_after_mix(sink.notifications);
    }

    /// Advance the media clock by `frames` of mixed output.
    ///
    /// The mix output runs on the output clock; one output frame carries the
    /// resource's current effective rate in media frames.
    fn advance_media_clock(&mut self, frames: usize) {
        let output_frames: f64 = AsPrimitive::as_(frames);
        let playback_rate = self.playback_rate();
        self.served_media_frames =
            output_frames.mul_add(f64::from(playback_rate), self.served_media_frames);
    }

    fn check_notifications(
        triggers: &mut TrackTriggers,
        notification_tx: &mut HeapProd<PlayerNotification>,
        track: TriggerTrack<'_>,
        input: TriggerInput,
    ) {
        triggers.check(notification_tx, track, input);
    }

    fn handle_failed_end(
        &mut self,
        notification_tx: &mut HeapProd<PlayerNotification>,
        fault: PlaybackFault,
    ) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id,
                reason: TrackPlaybackStopReason::Failed(fault),
                seek_epoch: self.seek_epoch,
            })
            .ok();
        self.state_dirty = false;
    }

    fn handle_full_read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        ctx: TrackReadContext<'_>,
        outcome: TrackReadOutcome,
    ) -> TrackReadOutcome {
        let TrackReadOutcome::Full {
            duration,
            frames,
            frames_until_eof,
            ..
        } = outcome
        else {
            return outcome;
        };

        self.advance_media_clock(frames);
        self.observed_duration = duration;
        self.update_observed_eof(frames_until_eof);
        let position = self.position();
        let duration = self.observed_duration;

        let TrackReadContext { sink, range } = ctx;
        let range_len = range.len();
        self.fade
            .mix_range(scratch_bufs, mix_bufs, range, range_len);
        Self::check_notifications(
            &mut self.triggers,
            sink.notifications,
            TriggerTrack {
                src: self.resource.src(),
                item_id: self.item_id,
            },
            TriggerInput {
                duration,
                frames_until_eof,
                position,
                block_frames: range_len,
                fade_duration: self.fade.duration(),
                prefetch_duration: self.prefetch_duration,
                sample_rate: self.sample_rate,
            },
        );
        self.update_after_mix(sink.notifications);

        TrackReadOutcome::Full {
            position,
            duration,
            frames,
            frames_until_eof,
        }
    }

    /// Finalize the track at its natural end — unless the control thread has
    /// already published a seek this track has not been re-based onto yet.
    ///
    /// The publish happens before the matching `PlayerCmd::Seek` is sent, so a
    /// newer `published_seek_epoch` means the user left this position while the
    /// render block was in flight. Ending the track there would hand the queue a
    /// `ItemDidPlayToEnd` for a position nobody is at, and the queue would
    /// auto-advance out from under the seek the processor is about to apply.
    /// Holding costs the caller one block of silence: the seek command that
    /// releases the hold is already on its way.
    fn handle_natural_end(
        &mut self,
        notification_tx: &mut HeapProd<PlayerNotification>,
        published_seek_epoch: u64,
    ) {
        if self.state == TrackState::Finished {
            return;
        }
        if published_seek_epoch != self.seek_epoch {
            return;
        }
        self.triggers.mark_prefetch_requested();
        self.triggers.emit_handover_requested(
            notification_tx,
            TriggerTrack {
                src: self.resource.src(),
                item_id: self.item_id,
            },
        );
        self.set_state(TrackState::Finished);
        self.ended_at_eof = true;
        notification_tx
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id,
                reason: TrackPlaybackStopReason::Eof,
                seek_epoch: self.seek_epoch,
            })
            .ok();
        self.state_dirty = false;
    }

    fn handle_partial_read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        ctx: TrackReadContext<'_>,
        partial: PartialRead,
    ) -> TrackReadOutcome {
        let TrackReadContext { sink, range } = ctx;
        let published_seek_epoch = sink.seek_epoch;
        let notification_tx = sink.notifications;
        let PartialRead { frames, duration } = partial;
        self.advance_media_clock(frames);
        let position = self.position();
        self.observed_duration = if position > 0.0 { position } else { duration };
        let duration = self.observed_duration;
        let block_frames = range.len();
        let mix_range = range.start..range.start + frames;

        self.fade
            .mix_range(scratch_bufs, mix_bufs, mix_range, frames);
        Self::check_notifications(
            &mut self.triggers,
            notification_tx,
            TriggerTrack {
                src: self.resource.src(),
                item_id: self.item_id,
            },
            TriggerInput {
                block_frames,
                duration,
                position,
                fade_duration: self.fade.duration(),
                frames_until_eof: Some(0),
                prefetch_duration: self.prefetch_duration,
                sample_rate: self.sample_rate,
            },
        );
        self.handle_natural_end(notification_tx, published_seek_epoch);

        TrackReadOutcome::Partial { frames, duration }
    }

    fn notify_state_change(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if !self.state_dirty {
            return;
        }
        let notification = match self.state {
            TrackState::Preloading => PlayerNotification::Loaded {
                src: Arc::clone(self.src()),
            },
            TrackState::FadingIn => PlayerNotification::FadingIn {
                src: Arc::clone(self.src()),
            },
            TrackState::FadingOut => PlayerNotification::FadingOut {
                src: Arc::clone(self.src()),
            },
            TrackState::Playing => PlayerNotification::PlaybackStarted {
                src: Arc::clone(self.src()),
                item_id: self.item_id,
            },
            TrackState::Finished => PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id,
                reason: TrackPlaybackStopReason::Stop,
                seek_epoch: self.seek_epoch,
            },
        };

        if notification_tx.try_push(notification).is_ok() {
            self.state_dirty = false;
        }
    }
}

// Read and render entrypoints keep each callback's source publication together.
impl PlayerTrack {
    /// Read audio from this track into scratch/mix buffers.
    pub fn read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        self.read_with_context(None, scratch_bufs, mix_bufs, range, sink)
    }

    fn read_resource(
        &mut self,
        context: Option<&RenderContext>,
        scratch_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> TrackReadOutcome {
        let resource = &mut self.resource;
        let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
        let mut scratch_window = [
            &mut scratch_left[0][range.clone()],
            &mut scratch_right[0][range.clone()],
        ];

        match resource.read_with_context(context, &mut scratch_window, 0..range.len(), metrics) {
            ReadOutcome::Full { frames } => TrackReadOutcome::Full {
                frames,
                duration: resource.duration(),
                frames_until_eof: resource.frames_until_eof(),
                position: 0.0,
            },
            ReadOutcome::Partial { frames } => TrackReadOutcome::Partial {
                frames,
                duration: resource.duration(),
            },
            ReadOutcome::Eof => TrackReadOutcome::Eof,
            ReadOutcome::Failed(kind) => TrackReadOutcome::Failed(PlaybackFault::Decode(kind)),
        }
    }

    fn read_with_context(
        &mut self,
        context: Option<&RenderContext>,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome = self.read_resource(context, scratch_bufs, range.clone(), sink.metrics);
        match read_outcome {
            TrackReadOutcome::Full { .. } => self.handle_full_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    range,
                    sink: sink.reborrow(),
                },
                read_outcome,
            ),
            TrackReadOutcome::Partial { frames, duration } => self.handle_partial_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    range,
                    sink: sink.reborrow(),
                },
                PartialRead { duration, frames },
            ),
            TrackReadOutcome::Eof => {
                self.handle_natural_end(sink.notifications, sink.seek_epoch);
                TrackReadOutcome::Eof
            }
            TrackReadOutcome::Failed(fault) => {
                self.handle_failed_end(sink.notifications, fault);
                TrackReadOutcome::Failed(fault)
            }
        }
    }

    /// One track's contribution to one output block.
    ///
    /// `range` is the block-relative span this track covers: the outer loop
    /// renders `0..frames`, while a gapless handover and a promotion render
    /// `offset..frames`, so the span carries the in-block seam. Together with
    /// the session-axis base in `context` it names the exact output frames
    /// this track wrote, which is what attributes a frame to a track.
    #[kithara::probe(
        track_id = self.item_id.as_u64(),
        output_base = context.map(|ctx| i64::from(ctx.output().output_frames().start)),
        range_start = range.start,
        range_end = range.end,
        served_media_frames = AsPrimitive::<u64>::as_(self.served_media_frames)
    )]
    pub(crate) fn render(
        &mut self,
        context: Option<&RenderContext>,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        let Some(context) = context else {
            self.resource.clear_render();
            return self.read_with_context(None, scratch_bufs, mix_bufs, range, sink);
        };
        if context.output().sample_rate().get() != self.sample_rate {
            self.resource.clear_render();
            let fault = PlaybackFault::OutputRateMismatch;
            self.handle_failed_end(sink.notifications, fault);
            return TrackReadOutcome::Failed(fault);
        }
        let Some(context) = context.for_output_range(range.clone()) else {
            self.resource.clear_render();
            let fault = PlaybackFault::OutputRangeUnavailable;
            self.handle_failed_end(sink.notifications, fault);
            return TrackReadOutcome::Failed(fault);
        };
        if self.sync_map.is_none()
            && let Some(source) = self
                .resource
                .presentation_source_end(context.output().sample_rate())
        {
            self.resource.publish_render(
                &context,
                presentation_frontier(&context, source.frame())
                    .with_warp_map(source.mapping_revision().map(WarpMapRevision::from)),
            );
        } else if self.sync_map.is_none() {
            self.resource.clear_render();
        }
        let outcome = self.read_with_context(Some(&context), scratch_bufs, mix_bufs, range, sink);
        let consumed = match &outcome {
            TrackReadOutcome::Full { frames, .. } | TrackReadOutcome::Partial { frames, .. } => {
                *frames
            }
            TrackReadOutcome::Eof | TrackReadOutcome::Failed(_) => 0,
        };
        if self.sync_map.is_some()
            && consumed > 0
            && let Some(source) = self.resource.consumed_source_end()
            && let Some(actual) = context.for_output_range(0..consumed)
        {
            self.resource.publish_render(
                &actual,
                PresentationFrontier::builder()
                    .source(source.frame())
                    .output(actual.output().output_frames().end)
                    .build()
                    .with_warp_map(source.mapping_revision().map(WarpMapRevision::from)),
            );
        }
        outcome
    }

    fn update_after_mix(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.fade.settled() {
            self.update_state_after_fade();
        }

        if self.state_dirty {
            self.notify_state_change(notification_tx);
        }
    }

    fn update_observed_eof(&mut self, frames_until_eof: Option<usize>) {
        if let Some(remaining_frames) = frames_until_eof {
            let sample_rate = self.sample_rate.max(1);
            let remaining_f64: f64 = AsPrimitive::as_(remaining_frames);
            let observed_eof = self.position() + remaining_f64 / f64::from(sample_rate);
            if self.observed_duration <= 0.0 || observed_eof < self.observed_duration {
                self.observed_duration = observed_eof;
            }
        }
    }

    fn update_state_after_fade(&mut self) {
        let new_state = match self.state {
            TrackState::FadingIn => TrackState::Playing,
            TrackState::FadingOut => TrackState::Finished,
            current => current,
        };
        self.set_state(new_state);
    }
}

impl SyncFadeTail {
    /// Read only the remaining overlap from the old lane. Both gains advance
    /// on output frames even when the old reader starves or reaches EOF.
    pub(crate) fn render(
        &mut self,
        context: &RenderContext,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) {
        if range.is_empty() {
            return;
        }
        let Some(segment) = context.for_output_range(range.clone()) else {
            return;
        };
        for channel in scratch_bufs.iter_mut().take(2) {
            channel[range.clone()].fill(0.0);
        }
        let reading = range.start..range.start + self.fade.remaining_frames().min(range.len());
        let frames = if reading.is_empty() {
            0
        } else {
            let Some(read_context) = context.for_output_range(reading.clone()) else {
                return;
            };
            let (left, right) = scratch_bufs.split_at_mut(1);
            let mut planes = [
                &mut left[0][reading.clone()],
                &mut right[0][reading.clone()],
            ];
            match self.resource.read_with_context(
                Some(&read_context),
                &mut planes,
                0..reading.len(),
                metrics,
            ) {
                ReadOutcome::Full { frames } | ReadOutcome::Partial { frames } => frames,
                ReadOutcome::Eof | ReadOutcome::Failed(_) => 0,
            }
        };
        self.fade
            .mix_range(scratch_bufs, mix_bufs, range.clone(), range.len());
        if frames > 0
            && let Some(source) = self.resource.consumed_source_end()
            && let Some(actual) = segment.for_output_range(0..frames)
        {
            let frontier = PresentationFrontier::builder()
                .source(source.frame())
                .output(actual.output().output_frames().end)
                .build()
                .with_warp_map(source.mapping_revision().map(WarpMapRevision::from));
            self.resource.publish_render(&actual, frontier);
        }
    }
}

fn presentation_frontier(context: &RenderContext, source: u64) -> PresentationFrontier {
    PresentationFrontier::builder()
        .source(source)
        .output(context.output().output_frames().start)
        .build()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame};
    use kithara_test_utils::kithara;
    use kithara_warp::RenderContext;

    use super::presentation_frontier;

    #[kithara::test]
    fn publication_uses_the_derived_subrange_start() {
        let output = OutputContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_200),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture output range is ordered");
        let context = RenderContext::new(output, None)
            .expect("fixture context is valid")
            .for_output_range(40..80)
            .expect("fixture subrange is valid");

        let frontier = presentation_frontier(&context, 8_000);

        assert_eq!(frontier.source(), 8_000);
        assert_eq!(frontier.output(), SessionFrame::new(1_040));
    }
}
