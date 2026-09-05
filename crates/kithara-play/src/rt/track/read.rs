use std::ops::Range;

use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext};
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapProd, traits::Producer};

use super::{
    PlayerTrack, ReadOutcome, RtSink,
    triggers::{TrackTriggers, TriggerInput, near_end_threshold},
};
use crate::bridge::{PlayerNotification, RtMetrics, TrackPlaybackStopReason, TrackState};

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
    /// The source reported a non-recoverable error mid-stream.
    Failed,
}

impl PlayerTrack {
    /// One track's contribution to one output block.
    ///
    /// `range` is the block-relative span this track covers: the outer loop
    /// renders `0..frames`, while a gapless handover and a promotion render
    /// `offset..frames`, so the span carries the in-block seam. Together with
    /// the session-axis base in `context` it names the exact output frames
    /// this track wrote, which is what attributes a frame to a track.
    #[kithara::probe(
        track_id = self.item_id.as_u64(),
        output_base = context.map(|ctx| i64::from(ctx.output_frames().start)),
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
            return self.read(scratch_bufs, mix_bufs, range, sink);
        };
        if context.sample_rate().get() != self.sample_rate {
            self.resource.clear_render();
            self.handle_failed_end(sink.notifications);
            return TrackReadOutcome::Failed;
        }
        let Some(context) = context.for_output_range(range.clone()) else {
            self.resource.clear_render();
            self.handle_failed_end(sink.notifications);
            return TrackReadOutcome::Failed;
        };
        if let Some(source) = self.resource.presentation_source_end(context.sample_rate()) {
            self.resource
                .publish_render(&context, presentation_frontier(&context, source.frame()));
        } else {
            self.resource.clear_render();
        }
        self.read(scratch_bufs, mix_bufs, range, sink)
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
        input: TriggerInput,
    ) {
        triggers.check(notification_tx, input);
    }

    /// Whether an end at `at_seconds` is one the media's own length does not
    /// account for.
    ///
    /// An end is a track finishing only if the media says so. A body that ran
    /// out reaches the reader as the same end, and taken at face value it arms
    /// the crossfade a fade before it and advances the queue after it - the
    /// fade a listener hears in the middle of a track. The declared length is
    /// the one number a lost body cannot move, and the distance is the one the
    /// triggers already use, so an end this side of it is one the position
    /// never would have announced.
    fn ends_short_of_its_length(&self, at_seconds: f64, block_frames: usize) -> bool {
        let declared = self.resource.duration();
        if declared <= 0.0 {
            return false;
        }
        let near: f64 = AsPrimitive::as_(near_end_threshold(
            self.fade.duration(),
            block_frames,
            self.sample_rate.max(1),
        ));
        at_seconds + near < declared
    }

    /// The remaining frames the resource announced, kept only when the media's
    /// declared length accounts for the end they point at.
    ///
    /// The announcement is what arms the crossfade and what shrinks the visible
    /// duration onto itself, so a body that ran out has to be refused here or
    /// both are already committed to the wrong end.
    fn announced_end_frames(
        &self,
        frames_until_eof: Option<usize>,
        block_frames: usize,
    ) -> Option<usize> {
        let remaining = frames_until_eof?;
        let remaining_f64: f64 = AsPrimitive::as_(remaining);
        let end = self.position() + remaining_f64 / f64::from(self.sample_rate.max(1));
        (!self.ends_short_of_its_length(end, block_frames)).then_some(remaining)
    }

    fn handle_failed_end(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id,
                reason: TrackPlaybackStopReason::Failed,
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

        let TrackReadContext { sink, range } = ctx;
        let range_len = range.len();

        self.advance_media_clock(frames);
        self.observed_duration = duration;
        let frames_until_eof = self.announced_end_frames(frames_until_eof, range_len);
        self.update_observed_eof(frames_until_eof);
        let position = self.position();
        let duration = self.observed_duration;

        self.fade
            .mix_range(scratch_bufs, mix_bufs, range, range_len);
        Self::check_notifications(
            &mut self.triggers,
            sink.notifications,
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
        self.triggers.emit_handover_requested(notification_tx);
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
        if self.ends_short_of_its_length(position, block_frames) {
            self.handle_failed_end(notification_tx);
            return TrackReadOutcome::Partial { frames, duration };
        }
        Self::check_notifications(
            &mut self.triggers,
            notification_tx,
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

    /// Read audio from this track into scratch/mix buffers.
    pub fn read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome = self.read_resource(scratch_bufs, range.clone(), sink.metrics);
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
            TrackReadOutcome::Eof
                if self.ends_short_of_its_length(self.position(), range.len()) =>
            {
                self.handle_failed_end(sink.notifications);
                TrackReadOutcome::Failed
            }
            TrackReadOutcome::Eof => {
                self.handle_natural_end(sink.notifications, sink.seek_epoch);
                TrackReadOutcome::Eof
            }
            TrackReadOutcome::Failed => {
                self.handle_failed_end(sink.notifications);
                TrackReadOutcome::Failed
            }
        }
    }

    fn read_resource(
        &mut self,
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

        match resource.read(&mut scratch_window, 0..range.len(), metrics) {
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
            ReadOutcome::Failed => TrackReadOutcome::Failed,
        }
    }

    fn update_after_mix(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.fade.has_settled() {
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

fn presentation_frontier(context: &RenderContext, source: u64) -> PresentationFrontier {
    PresentationFrontier::builder()
        .source(source)
        .output(context.output_frames().start)
        .build()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use kithara_warp::{RenderContext, SessionEpoch, SessionFrame};

    use super::presentation_frontier;

    #[kithara::test]
    fn publication_uses_the_derived_subrange_start() {
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_200),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture context is valid")
        .for_output_range(40..80)
        .expect("fixture subrange is valid");

        let frontier = presentation_frontier(&context, 8_000);

        assert_eq!(frontier.source(), 8_000);
        assert_eq!(frontier.output(), SessionFrame::new(1_040));
    }
}
