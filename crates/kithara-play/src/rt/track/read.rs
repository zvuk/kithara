use std::{ops::Range, sync::Arc};

use num_traits::cast::{AsPrimitive, ToPrimitive};
use ringbuf::{HeapProd, traits::Producer};

use super::{
    PlayerTrack, ReadOutcome,
    triggers::{TrackTriggers, TriggerInput},
};
use crate::bridge::{PlayerNotification, TrackPlaybackStopReason, TrackState};

struct TrackReadContext<'a> {
    notification_tx: &'a mut HeapProd<PlayerNotification>,
    range: Range<usize>,
}

#[derive(Clone, Copy)]
struct PartialRead {
    frames: usize,
    duration: f64,
}

/// Result of a single track render attempt.
#[derive(Debug)]
pub enum TrackReadOutcome {
    /// The full requested block was written into the mix buffer.
    Full {
        /// Playback position snapshot after the read (seconds).
        position: f64,
        /// Real PCM frames copied from the underlying resource/scratch buffer.
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
    fn advance_served_frames(&mut self, frames: u64) {
        self.served_frames = self.served_frames.saturating_add(frames);
    }

    fn check_notifications(
        triggers: &mut TrackTriggers,
        notification_tx: &mut HeapProd<PlayerNotification>,
        input: TriggerInput,
    ) {
        triggers.check(notification_tx, input);
    }

    fn handle_failed_end(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Failed,
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

        let produced_frames = frames.to_u64().unwrap_or(0);
        self.advance_served_frames(produced_frames);
        self.observed_duration = duration;
        self.update_observed_eof(frames_until_eof);
        let position = self.position();
        let duration = self.observed_duration;

        let range_len = ctx.range.len();
        self.fade
            .mix_range(scratch_bufs, mix_bufs, ctx.range, range_len);
        Self::check_notifications(
            &mut self.triggers,
            ctx.notification_tx,
            TriggerInput {
                block_frames: range_len,
                duration,
                fade_duration: self.fade.duration(),
                frames_until_eof,
                position,
                prefetch_duration: self.prefetch_duration,
                sample_rate: self.sample_rate,
            },
        );
        self.update_after_mix(ctx.notification_tx);

        TrackReadOutcome::Full {
            position,
            duration,
            frames,
            frames_until_eof,
        }
    }

    fn handle_natural_end(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.triggers.mark_prefetch_requested();
        self.triggers.emit_handover_requested(notification_tx);
        self.set_state(TrackState::Finished);
        self.ended_at_eof = true;
        notification_tx
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Eof,
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
        let TrackReadContext {
            notification_tx,
            range,
        } = ctx;
        let PartialRead { frames, duration } = partial;
        self.advance_served_frames(frames as u64);
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
            TriggerInput {
                block_frames,
                duration,
                fade_duration: self.fade.duration(),
                frames_until_eof: Some(0),
                position,
                prefetch_duration: self.prefetch_duration,
                sample_rate: self.sample_rate,
            },
        );
        self.handle_natural_end(notification_tx);

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
            TrackState::FadingIn => PlayerNotification::FadingIn,
            TrackState::FadingOut => PlayerNotification::FadingOut,
            TrackState::Playing => PlayerNotification::PlaybackStarted,
            TrackState::Finished => PlayerNotification::PlaybackStopped {
                src: Arc::clone(self.src()),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Stop,
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
        notification_tx: &mut HeapProd<PlayerNotification>,
    ) -> TrackReadOutcome {
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome = self.read_resource(scratch_bufs, range.clone());
        match read_outcome {
            TrackReadOutcome::Full { .. } => self.handle_full_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    notification_tx,
                    range,
                },
                read_outcome,
            ),
            TrackReadOutcome::Partial { frames, duration } => self.handle_partial_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    notification_tx,
                    range,
                },
                PartialRead { frames, duration },
            ),
            TrackReadOutcome::Eof => {
                self.handle_natural_end(notification_tx);
                TrackReadOutcome::Eof
            }
            TrackReadOutcome::Failed => {
                self.handle_failed_end(notification_tx);
                TrackReadOutcome::Failed
            }
        }
    }

    fn read_resource(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        range: Range<usize>,
    ) -> TrackReadOutcome {
        let resource = &mut self.resource;
        let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
        let mut scratch_window = [
            &mut scratch_left[0][range.clone()],
            &mut scratch_right[0][range.clone()],
        ];

        match resource.read(&mut scratch_window, 0..range.len()) {
            ReadOutcome::Full { frames } => TrackReadOutcome::Full {
                duration: resource.duration(),
                frames,
                frames_until_eof: resource.frames_until_eof(),
                position: 0.0,
            },
            ReadOutcome::Partial(frames) => TrackReadOutcome::Partial {
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
