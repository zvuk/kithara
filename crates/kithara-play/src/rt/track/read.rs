use std::ops::Range;

use kithara_audio::{PresentationAdvance, PresentationCursor, PresentationPoint, SessionFrame};
use kithara_platform::sync::Arc;
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapProd, traits::Producer};

use super::{
    PlayerTrack, ReadOutcome, RtSink,
    core::MediaPresentationAnchor,
    triggers::{TrackTriggers, TriggerInput},
};
use crate::bridge::{PlayerNotification, RtMetrics, TrackPlaybackStopReason, TrackState};

struct TrackReadContext<'a> {
    sink: RtSink<'a>,
    range: Range<usize>,
}

#[derive(Clone, Copy)]
struct PartialRead {
    duration: f64,
    frames: usize,
    presentation: Option<PresentationCursor>,
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
        /// Producer point mapped from an exactly consumed final boundary.
        presentation: Option<PresentationCursor>,
    },
    /// Only the first `frames` samples were written; EOF was reached in-block.
    Partial {
        /// Number of frames written into the destination block.
        frames: usize,
        /// Visible duration snapshot in seconds.
        duration: f64,
        /// Producer point mapped from an exactly consumed final boundary.
        presentation: Option<PresentationCursor>,
    },
    /// No frames were written because the track is already finished.
    Eof,
    /// The source reported a non-recoverable error mid-stream.
    Failed,
}

impl PlayerTrack {
    /// Advance the media clock after one read of real output frames.
    ///
    /// The first consumed presentation boundary anchors the decoder's absolute
    /// source origin to the audible clock. Later compatible boundaries replace
    /// accumulated scalar drift with exact source progress.
    fn advance_media_clock(&mut self, frames: usize, advance: Option<PresentationAdvance>) {
        let (served, anchor) = media_frames_after_read(
            self.served_media_frames,
            frames,
            self.playback_rate,
            self.sample_rate,
            advance,
            self.media_presentation,
        );
        self.served_media_frames = served;
        self.media_presentation = anchor;
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
            presentation,
            ..
        } = outcome
        else {
            return outcome;
        };

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
            presentation,
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
        let TrackReadContext { sink, range } = ctx;
        let notification_tx = sink.notifications;
        let PartialRead {
            frames,
            duration,
            presentation,
        } = partial;
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
                position,
                fade_duration: self.fade.duration(),
                frames_until_eof: Some(0),
                prefetch_duration: self.prefetch_duration,
                sample_rate: self.sample_rate,
            },
        );
        self.handle_natural_end(notification_tx);

        TrackReadOutcome::Partial {
            frames,
            duration,
            presentation,
        }
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
                item_id: self.item_id.clone(),
            },
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
        block_frame: Option<SessionFrame>,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome =
            self.read_resource(scratch_bufs, range.clone(), block_frame, sink.metrics);
        match read_outcome {
            TrackReadOutcome::Full { .. } => self.handle_full_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    sink: sink.reborrow(),
                    range,
                },
                read_outcome,
            ),
            TrackReadOutcome::Partial {
                frames,
                duration,
                presentation,
            } => self.handle_partial_read(
                scratch_bufs,
                mix_bufs,
                TrackReadContext {
                    sink: sink.reborrow(),
                    range,
                },
                PartialRead {
                    duration,
                    frames,
                    presentation,
                },
            ),
            TrackReadOutcome::Eof => {
                self.handle_natural_end(sink.notifications);
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
        block_frame: Option<SessionFrame>,
        metrics: &RtMetrics,
    ) -> TrackReadOutcome {
        let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
        let mut scratch_window = [
            &mut scratch_left[0][range.clone()],
            &mut scratch_right[0][range.clone()],
        ];

        let (read_outcome, point, duration, frames_until_eof, presentation_advance) = {
            let resource = &mut self.resource;
            let read_outcome = resource.read(&mut scratch_window, 0..range.len(), metrics);
            let presentation_advance = resource.take_presentation_advance();
            (
                read_outcome,
                resource.presentation_point(),
                resource.duration(),
                resource.frames_until_eof(),
                presentation_advance,
            )
        };

        match read_outcome {
            ReadOutcome::Full { frames } => {
                let presentation = if frames == range.len() && frames > 0 {
                    self.update_presentation(
                        point,
                        presentation_advance,
                        block_frame,
                        &range,
                        frames,
                    )
                } else {
                    self.invalidate_presentation();
                    None
                };
                self.advance_media_clock(frames, presentation_advance);
                TrackReadOutcome::Full {
                    frames,
                    duration,
                    frames_until_eof,
                    position: 0.0,
                    presentation,
                }
            }
            ReadOutcome::Partial { frames } => {
                let presentation = if frames > 0 {
                    self.update_presentation(
                        point,
                        presentation_advance,
                        block_frame,
                        &range,
                        frames,
                    )
                } else {
                    self.invalidate_presentation();
                    None
                };
                self.advance_media_clock(frames, presentation_advance);
                TrackReadOutcome::Partial {
                    frames,
                    duration,
                    presentation,
                }
            }
            ReadOutcome::Eof => {
                self.invalidate_presentation();
                TrackReadOutcome::Eof
            }
            ReadOutcome::Failed => {
                self.invalidate_presentation();
                TrackReadOutcome::Failed
            }
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

fn media_frames_after_read(
    current: f64,
    frames: usize,
    playback_rate: f32,
    host_rate: u32,
    advance: Option<PresentationAdvance>,
    previous: Option<MediaPresentationAnchor>,
) -> (f64, Option<MediaPresentationAnchor>) {
    let scalar = |frames: usize, current: f64| {
        let output_frames: f64 = AsPrimitive::as_(frames);
        output_frames.mul_add(f64::from(playback_rate), current)
    };
    let Some(advance) = advance else {
        return (scalar(frames, current), previous);
    };
    let Some(suffix_frames) = frames.checked_sub(advance.read_offset_frames()) else {
        return (scalar(frames, current), None);
    };
    let consumed = advance.point();
    let host_rate = f64::from(host_rate.max(1));
    let prefix_boundary = scalar(advance.read_offset_frames(), current);
    let boundary = previous
        .filter(|previous| compatible_media_points(previous.point, consumed))
        .and_then(|previous| {
            consumed
                .source_frame()
                .checked_sub(previous.point.source_frame())
                .map(|delta| {
                    let source_frames: f64 = AsPrimitive::as_(delta);
                    source_frames.mul_add(
                        host_rate / f64::from(consumed.sample_rate().get()),
                        previous.frames,
                    )
                })
        })
        .unwrap_or(prefix_boundary);
    let suffix_frames: f64 = AsPrimitive::as_(suffix_frames);
    (
        suffix_frames.mul_add(f64::from(playback_rate), boundary),
        Some(MediaPresentationAnchor {
            frames: boundary,
            point: consumed,
        }),
    )
}

fn compatible_media_points(previous: PresentationPoint, current: PresentationPoint) -> bool {
    previous.seek_epoch() == current.seek_epoch()
        && previous.generation() == current.generation()
        && previous.sample_rate() == current.sample_rate()
        && previous.output_end() <= current.output_end()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{PresentationAdvance, PresentationPoint};
    use kithara_test_utils::kithara;

    use super::media_frames_after_read;

    #[kithara::test]
    fn consumed_presentation_rebases_then_advances_the_read_suffix() {
        let source_rate = NonZeroU32::new(48_000).expect("test source rate is non-zero");
        let point = PresentationPoint::new(0, 48_000, 0, 4_096, source_rate);
        let advance = PresentationAdvance::new(point, 256);

        let (corrected, anchor) =
            media_frames_after_read(44_000.0, 512, 1.5, 44_100, Some(advance), None);
        let (scalar_only, _) = media_frames_after_read(44_000.0, 512, 1.5, 44_100, None, None);

        assert_eq!(corrected, 44_768.0);
        assert_eq!(scalar_only, 44_768.0);
        assert_eq!(
            corrected, scalar_only,
            "the first source proof must anchor without importing its codec-specific origin"
        );

        let later = PresentationAdvance::new(
            PresentationPoint::new(0, 48_240, 0, 4_608, source_rate),
            128,
        );
        let (corrected, later_anchor) =
            media_frames_after_read(corrected, 512, 1.5, 44_100, Some(later), anchor);
        assert_eq!(corrected, 45_180.5);

        let changed_rate = NonZeroU32::new(44_100).expect("test source rate is non-zero");
        let rate_change = PresentationAdvance::new(
            PresentationPoint::new(0, 48_480, 0, 5_120, changed_rate),
            64,
        );
        let (rate_reanchored, rate_anchor) =
            media_frames_after_read(corrected, 512, 1.5, 44_100, Some(rate_change), later_anchor);
        assert_eq!(
            rate_anchor
                .expect("rate change establishes an anchor")
                .frames,
            45_276.5,
            "a sample-rate change reanchors at the scalar prefix boundary"
        );
        assert_eq!(rate_reanchored, 45_948.5);

        let replacement =
            PresentationAdvance::new(PresentationPoint::new(0, 96_000, 1, 512, source_rate), 64);
        let (reanchored, _) =
            media_frames_after_read(corrected, 512, 1.5, 44_100, Some(replacement), later_anchor);
        assert_eq!(reanchored, 45_948.5);
    }
}
