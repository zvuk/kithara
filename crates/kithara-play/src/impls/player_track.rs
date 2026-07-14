use std::{num::NonZeroU32, ops::Range};

use kithara_platform::sync::Arc;

#[rustfmt::skip]
use firewheel::dsp::filter::smoothing_filter::DEFAULT_SETTLE_EPSILON;
#[rustfmt::skip]
use firewheel::param::smoother::SmootherConfig;
use firewheel::dsp::{
    fade::FadeCurve,
    mix::{Mix, MixDSP},
};
use kithara_audio::ServiceClass;
use kithara_platform::sync::Mutex;
use num_traits::cast::{AsPrimitive, ToPrimitive};
use ringbuf::{HeapProd, traits::Producer};

use super::{
    player_notification::{PlayerNotification, TrackPlaybackStopReason},
    player_resource::{PlayerResource, ReadOutcome},
};

/// State machine for a single track's lifecycle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    /// Track is loaded but not yet playing.
    #[default]
    Preloading,
    /// Track is actively playing at full volume.
    Playing,
    /// Track is fading in (volume ramping up).
    FadingIn,
    /// Track is fading out (volume ramping down).
    FadingOut,
    /// Track has finished playback (EOF or stopped).
    Finished,
}

impl TrackState {
    /// Whether the track is the "leading" track (playing or fading in).
    pub(crate) fn is_leading(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn)
    }

    /// Whether the track is producing audible audio.
    pub(crate) fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn | Self::FadingOut)
    }
}

/// Transition command for a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackTransition {
    /// Start fading in the track with the given source identifier.
    FadeIn(Arc<str>),
    /// Start fading out the track with the given source identifier.
    FadeOut(Arc<str>),
}

/// Result of a single track render attempt.
#[derive(Debug)]
pub enum TrackReadOutcome {
    /// The full requested block was written into the mix buffer.
    Full {
        /// Playback position snapshot after the read (seconds).
        ///
        /// Tracks `served_frames / sample_rate`, i.e. what has actually been
        /// mixed into the output for this track. Returned for diagnostic
        /// and unit-test inspection.
        position: f64,
        /// Real PCM frames copied from the underlying resource/scratch buffer.
        /// The destination block may include additional zero-filled underrun
        /// frames, which do not count as playback progress.
        frames: usize,
        /// Visible (post-gapless-trim) duration snapshot in seconds.
        duration: f64,
        /// Exact remaining buffered frames when the resource has already
        /// observed EOF while filling its scratch buffer.
        frames_until_eof: Option<usize>,
    },
    /// Only the first `frames` samples were written; EOF was reached in-block.
    ///
    /// `PlayerTrack::read` emits EOF `TrackPlaybackStopped` in the same call
    /// so notification ordering matches the last written frame.
    Partial {
        /// Number of frames written into the destination block.
        frames: usize,
        /// Visible (post-gapless-trim) duration snapshot in seconds.
        duration: f64,
    },
    /// No frames were written because the track is already finished.
    Eof,
    /// The underlying decoder / source reported a non-recoverable error
    /// mid-stream. Distinct from `Eof`: track did NOT reach natural
    /// end. Upstream surfaces this as a track-failed signal (a
    /// `PlaybackStopped { Failed }` notification) so callers don't
    /// confuse it with an auto-advance trigger.
    Failed,
}

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerTrack {
    #[field(get, deref = false)]
    resource: Arc<Mutex<PlayerResource>>,
    #[field(get, deref = false)]
    src: Arc<str>,
    fade_curve: FadeCurve,
    mix: MixDSP,
    item_id: Option<Arc<str>>,
    #[field(get, copy)]
    state: TrackState,
    /// Set only when the track reaches *natural* EOF (`handle_natural_end`).
    /// Marks a played-out track as eligible to be kept warm at end-of-queue
    /// and revived by a later in-range seek (Superpowered-style resume).
    /// Cleared by `seek`/`play`. A `Finished` state from `stop()` or a
    /// faded-out crossfade leaves this `false`, so those are discarded as usual.
    #[field(get)]
    ended_at_eof: bool,
    notified_prefetch_requested: bool,
    notified_track_requested: bool,
    state_dirty: bool,
    /// Current crossfade duration used for near-end trigger checks.
    fade_duration: f32,
    /// Lead time before EOF at which the prefetch trigger fires.
    ///
    /// Effective preload threshold is
    /// `max(prefetch_duration, fade_duration) + block_seconds`, so prefetch
    /// is at least as eager as the crossfade trigger and can be set
    /// independently to cover network/probe latency.
    prefetch_duration: f32,
    /// Last observed duration snapshot.
    ///
    /// Mirrors `PlayerResource::duration()` (post-gapless-trim, visible
    /// duration) captured under the resource lock.
    observed_duration: f64,
    /// Last decoded-ahead frontier (seconds) captured under the resource lock
    /// during `read()`. Fallback for [`Self::decoded_frontier`] when the
    /// resource lock is momentarily held by the decoder; the live resource is
    /// the authoritative source. Always `>=` the served position.
    observed_frontier: f64,
    sample_rate: u32,
    /// Cumulative frames this track has actually served into the mix output.
    ///
    /// Used as the source of truth for near-end trigger position so the
    /// trigger reflects what has been rendered to the audio output, not the
    /// decoder's pre-buffered position (which can be ~200 ms ahead of the
    /// mixer thanks to `PlayerResource`'s scratch buffer).
    served_frames: u64,
}

impl PlayerTrack {
    /// Create a new track in the `Preloading` state.
    ///
    /// The `MixDSP` starts at `FULLY_WET` (silent) so that an explicit
    /// `fade_in()` or `play()` is required to produce audio.
    #[must_use]
    pub fn new(
        resource: Arc<Mutex<PlayerResource>>,
        item_id: Option<Arc<str>>,
        src: Arc<str>,
        fade_duration: f32,
        prefetch_duration: f32,
        sample_rate: NonZeroU32,
        fade_curve: FadeCurve,
    ) -> Self {
        let fade_conf = SmootherConfig {
            smooth_seconds: fade_duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        };
        // Seed from the resource's already-known duration (set synchronously at
        // source open, before `Loaded`) so a freshly-loaded track reports its
        // duration on the first render cycle — before any decode-backed `read`.
        // Without this the render path publishes 0.0 whenever the decode worker
        // holds the resource lock, leaving `duration_seconds()` racy at load.
        let observed_duration = resource.try_lock().ok().map_or(0.0, |r| r.duration());
        let track = Self {
            resource,
            fade_curve,
            fade_duration,
            item_id,
            src,
            state: TrackState::Preloading,
            state_dirty: false,
            notified_track_requested: false,
            notified_prefetch_requested: false,
            mix: MixDSP::new(Mix::FULLY_WET, fade_curve, fade_conf, sample_rate),
            prefetch_duration: prefetch_duration.max(0.0),
            sample_rate: sample_rate.get(),
            served_frames: 0,
            observed_duration,
            observed_frontier: 0.0,
            ended_at_eof: false,
        };
        track.update_service_class(TrackState::Preloading);
        track
    }

    /// Add `frames` to the rolling served-frames counter so trigger checks
    /// reflect what has been mixed into the output, not what the decoder has
    /// pre-buffered.
    fn advance_served_frames(&mut self, frames: u64) {
        self.served_frames = self.served_frames.saturating_add(frames);
    }

    /// Check position-based notifications.
    ///
    /// Two independent triggers may fire in parallel via the same
    /// `TrackRequested` notification:
    ///
    /// - prefetch — at `max(prefetch_duration, fade_duration) + block_seconds`
    ///   before EOF; preload-only (`imminent = false` unless this coincides
    ///   with the crossfade threshold).
    /// - crossfade — at `fade_duration + block_seconds` before EOF;
    ///   `imminent = true` so the handler can fade-in / hand over.
    fn check_notifications(
        &mut self,
        notification_tx: &Mutex<HeapProd<PlayerNotification>>,
        block_frames: usize,
        frames_until_eof: Option<usize>,
    ) {
        let block_frames_f32: f32 = block_frames.as_();
        let sr_f32: f32 = self.sample_rate.as_();
        let block_seconds = block_frames_f32 / sr_f32;
        let fade_threshold = self.fade_duration + block_seconds;
        let prefetch_threshold = self.prefetch_duration.max(self.fade_duration) + block_seconds;

        if let Some(frames_until_eof) = frames_until_eof {
            let frames_f32: f32 = frames_until_eof.as_();
            let remaining = frames_f32 / sr_f32;
            if remaining <= prefetch_threshold {
                self.emit_track_requested(notification_tx);
            }
            if remaining <= fade_threshold {
                self.emit_handover_requested(notification_tx);
            }
            return;
        }

        let duration = self.observed_duration;
        if duration <= 0.0 {
            return;
        }

        let pos: f32 = self.position().as_();
        let dur: f32 = duration.as_();

        if pos + prefetch_threshold >= dur {
            self.emit_track_requested(notification_tx);
        }
        if pos + fade_threshold >= dur {
            self.emit_handover_requested(notification_tx);
        }
    }

    /// Current visible (post-gapless-trim) duration in seconds.
    ///
    /// Mirrors `PlayerResource::duration()` captured under the resource
    /// lock during the last `read()`. Falls back to a fresh `try_lock`
    /// when invoked before any `read()` (e.g. immediately after `seek`).
    #[must_use]
    pub fn duration(&self) -> f64 {
        if self.observed_duration > 0.0 {
            return self.observed_duration;
        }
        self.resource
            .try_lock()
            .ok()
            .map_or(self.observed_duration, |resource| resource.duration())
    }

    /// Decoded-ahead frontier in seconds — how much content is decoded and
    /// ready to play (always `>=` [`Self::position`]). Reads the live resource
    /// (mirroring [`Self::duration`]) so the buffered/playable window keeps
    /// growing while the player is paused — the decode worker advances the
    /// frontier on its own thread, but `read()` (which refreshes
    /// `observed_frontier`) only runs while rendering. Falls back to the last
    /// rendered value when the resource lock is momentarily held by the decoder.
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource
            .try_lock()
            .ok()
            .map_or(self.observed_frontier, |resource| {
                resource.decoded_frontier()
            })
    }

    /// Emit the crossfade-aligned handover trigger once per playback cycle.
    fn emit_handover_requested(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.notified_track_requested {
            return;
        }

        if notification_tx
            .lock()
            .try_push(PlayerNotification::HandoverRequested)
            .is_ok()
        {
            self.notified_track_requested = true;
        }
    }

    /// Emit the prefetch (preload-only) trigger once per playback cycle.
    fn emit_track_requested(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.notified_prefetch_requested {
            return;
        }

        if notification_tx
            .lock()
            .try_push(PlayerNotification::Requested)
            .is_ok()
        {
            self.notified_prefetch_requested = true;
        }
    }

    /// Start a fade-in: transitions to `FadingIn`, targets `FULLY_DRY` (audible).
    pub fn fade_in(&mut self) {
        self.set_state(TrackState::FadingIn);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
    }

    /// Start a fade-out: transitions to `FadingOut`, targets `FULLY_WET` (silent).
    pub fn fade_out(&mut self) {
        self.set_state(TrackState::FadingOut);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
    }

    /// Handle a mid-stream decode failure. Distinct from natural EOF:
    /// no `handover_requested` (the queue layer decides whether to
    /// advance based on the `Failed` reason), and the stop reason is
    /// `Failed`, not `Eof`. Without this split, transient decoder
    /// errors get conflated with natural EOF and the queue auto-
    /// advances as if the track played out — observed in the GUI as
    /// "track shows pos=91s of 169s and skips to next".
    fn handle_failed_end(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .lock()
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Failed,
            })
            .ok();
        self.state_dirty = false;
    }

    /// Handle natural EOF.
    fn handle_natural_end(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.notified_prefetch_requested = true;
        self.emit_handover_requested(notification_tx);
        self.set_state(TrackState::Finished);
        self.ended_at_eof = true;
        notification_tx
            .lock()
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Eof,
            })
            .ok();
        self.state_dirty = false;
    }

    /// Emit notification when state changes.
    fn notify_state_change(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if !self.state_dirty {
            return;
        }
        let notification = match self.state {
            TrackState::Preloading => PlayerNotification::Loaded {
                src: Arc::clone(&self.src),
            },
            TrackState::FadingIn => PlayerNotification::FadingIn {
                src: Arc::clone(&self.src),
            },
            TrackState::FadingOut => PlayerNotification::FadingOut {
                src: Arc::clone(&self.src),
            },
            TrackState::Playing => PlayerNotification::PlaybackStarted {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
            },
            TrackState::Finished => PlayerNotification::PlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Stop,
            },
        };

        if notification_tx.lock().try_push(notification).is_ok() {
            self.state_dirty = false;
        }
    }

    /// Instantly start playing at full volume.
    pub fn play(&mut self) {
        self.set_state(TrackState::Playing);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.mix.reset_to_target();
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
        self.ended_at_eof = false;
    }

    /// Current position in seconds.
    ///
    /// Tracks `served_frames / sample_rate` — i.e. what has actually been
    /// mixed into the output — so the value matches the trigger evaluator
    /// instead of the decoder's pre-buffered position.
    #[must_use]
    pub fn position(&self) -> f64 {
        let sample_rate = self.sample_rate.max(1);
        let served_f64: f64 = AsPrimitive::as_(self.served_frames);
        served_f64 / f64::from(sample_rate)
    }

    /// Read audio from this track into scratch/mix buffers.
    ///
    /// 1. Tries to lock the resource (non-blocking).
    /// 2. Reads PCM into `scratch_bufs`.
    /// 3. Mixes through `MixDSP` into `mix_bufs`.
    /// 4. Detects fade completion and updates state.
    ///
    /// On [`TrackReadOutcome::Partial`], the last written frame belongs to this
    /// track and EOF stop notifications are emitted immediately in the same
    /// call. The caller owns any remaining tail in the destination block.
    ///
    /// `range` selects the destination subrange inside `scratch_bufs` and
    /// `mix_bufs` that this read may fill.
    pub fn read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        notification_tx: &Mutex<HeapProd<PlayerNotification>>,
    ) -> TrackReadOutcome {
        /// Minimum number of channels required for stereo processing.
        const MIN_STEREO_CHANNELS: usize = 2;
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome = {
            let Some(mut guard) = self.resource.try_lock().ok() else {
                return TrackReadOutcome::Full {
                    position: self.position(),
                    duration: self.observed_duration,
                    frames: 0,
                    frames_until_eof: None,
                };
            };
            self.observed_frontier = guard.decoded_frontier();
            let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
            let mut scratch_window = [
                &mut scratch_left[0][range.clone()],
                &mut scratch_right[0][range.clone()],
            ];

            match guard.read(&mut scratch_window, 0..range.len()) {
                ReadOutcome::Full { frames } => {
                    let duration = guard.duration();
                    let frames_until_eof = guard.frames_until_eof();
                    drop(guard);
                    TrackReadOutcome::Full {
                        duration,
                        frames,
                        frames_until_eof,
                        position: 0.0,
                    }
                }
                ReadOutcome::Partial { frames } => {
                    let duration = guard.duration();
                    drop(guard);
                    TrackReadOutcome::Partial { frames, duration }
                }
                ReadOutcome::Eof => TrackReadOutcome::Eof,
                ReadOutcome::Failed => {
                    drop(guard);
                    TrackReadOutcome::Failed
                }
            }
        };

        match read_outcome {
            TrackReadOutcome::Full {
                duration,
                frames,
                frames_until_eof,
                ..
            } => {
                let produced_frames = frames.to_u64().unwrap_or(0);
                self.advance_served_frames(produced_frames);
                self.observed_duration = duration;
                if let Some(remaining_frames) = frames_until_eof {
                    let sample_rate = self.sample_rate.max(1);
                    let remaining_f64: f64 = AsPrimitive::as_(remaining_frames);
                    let observed_eof = self.position() + remaining_f64 / f64::from(sample_rate);
                    if self.observed_duration <= 0.0 || observed_eof < self.observed_duration {
                        self.observed_duration = observed_eof;
                    }
                }
                let position = self.position();
                let duration = self.observed_duration;

                if scratch_bufs.len() >= MIN_STEREO_CHANNELS
                    && mix_bufs.len() >= MIN_STEREO_CHANNELS
                {
                    let (output_l_slice, output_r_slice) = mix_bufs.split_at_mut(1);
                    let output_l = &mut output_l_slice[0][range.clone()];
                    let output_r = &mut output_r_slice[0][range.clone()];

                    self.mix.mix_dry_into_wet_stereo(
                        &scratch_bufs[0][range.clone()],
                        &scratch_bufs[1][range.clone()],
                        output_l,
                        output_r,
                        range.len(),
                    );
                }

                self.check_notifications(notification_tx, range.len(), frames_until_eof);

                if self.mix.has_settled() {
                    self.update_state_after_fade();
                }

                if self.state_dirty {
                    self.notify_state_change(notification_tx);
                }

                TrackReadOutcome::Full {
                    position,
                    duration,
                    frames,
                    frames_until_eof,
                }
            }
            TrackReadOutcome::Partial { frames, duration } => {
                self.advance_served_frames(frames.to_u64().unwrap_or(0));
                let position = self.position();
                self.observed_duration = if position > 0.0 { position } else { duration };
                let duration = self.observed_duration;

                if scratch_bufs.len() >= MIN_STEREO_CHANNELS
                    && mix_bufs.len() >= MIN_STEREO_CHANNELS
                {
                    let (output_l_slice, output_r_slice) = mix_bufs.split_at_mut(1);
                    let output_l = &mut output_l_slice[0][range.start..range.start + frames];
                    let output_r = &mut output_r_slice[0][range.start..range.start + frames];

                    self.mix.mix_dry_into_wet_stereo(
                        &scratch_bufs[0][range.start..range.start + frames],
                        &scratch_bufs[1][range.start..range.start + frames],
                        output_l,
                        output_r,
                        frames,
                    );
                }

                self.check_notifications(notification_tx, range.len(), Some(0));
                self.handle_natural_end(notification_tx);

                TrackReadOutcome::Partial { frames, duration }
            }
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

    /// Seek the underlying resource and re-sync the served-frame counter
    /// so trigger thresholds reflect the new playback origin.
    pub fn seek(&mut self, seconds: f64) {
        if let Ok(mut resource) = self.resource.try_lock() {
            resource.seek(seconds);
        }
        let frames = Self::seek_frame_index(seconds, self.sample_rate, self.observed_duration);
        self.served_frames = frames;
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
        self.ended_at_eof = false;
    }

    fn seek_frame_index(seconds: f64, sample_rate: u32, duration: f64) -> u64 {
        let sample_rate = sample_rate.max(1);
        let target_seconds = if seconds.is_nan() {
            0.0
        } else if seconds.is_finite() {
            seconds.max(0.0)
        } else if seconds.is_sign_positive() {
            duration.max(0.0)
        } else {
            0.0
        };
        let bounded_seconds = if duration > 0.0 {
            target_seconds.min(duration)
        } else {
            target_seconds
        };
        let frames = bounded_seconds * f64::from(sample_rate);
        ToPrimitive::to_u64(&frames).unwrap_or(0)
    }

    /// Update the prefetch lead time used for the preload trigger.
    pub fn set_prefetch_duration(&mut self, prefetch_duration: f32) {
        self.prefetch_duration = prefetch_duration.max(0.0);
    }

    /// Set the track state and mark as dirty.
    ///
    /// Also updates the shared worker's scheduling priority via
    /// [`ServiceClass`] bridge: Audible tracks get highest priority.
    fn set_state(&mut self, new_state: TrackState) {
        if self.state != new_state {
            self.state = new_state;
            self.state_dirty = true;
            self.update_service_class(new_state);
        }
    }

    /// Instantly stop (silent, finished state).
    pub fn stop(&mut self) {
        self.set_state(TrackState::Finished);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
        self.mix.reset_to_target();
    }

    /// Re-create the `MixDSP` with a new fade duration.
    pub fn update_fade_duration(&mut self, fade_duration: f32, sample_rate: NonZeroU32) {
        let fade_conf = SmootherConfig {
            smooth_seconds: fade_duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        };
        let target_mix = if self.state.is_leading() {
            Mix::FULLY_DRY
        } else {
            Mix::FULLY_WET
        };
        self.mix = MixDSP::new(target_mix, self.fade_curve, fade_conf, sample_rate);
        self.fade_duration = fade_duration;
        self.sample_rate = sample_rate.get();
    }

    /// Map track state to worker scheduling priority and push the update.
    ///
    /// Uses `try_lock()` + `mpsc::send` — not strictly lock-free, but the
    /// update is a best-effort scheduling hint. If the lock is contended or
    /// the send briefly blocks, the track keeps its previous priority until
    /// the next state change.
    fn update_service_class(&self, state: TrackState) {
        let class = match state {
            TrackState::Playing | TrackState::FadingIn | TrackState::FadingOut => {
                ServiceClass::Audible
            }
            TrackState::Preloading => ServiceClass::Warm,
            TrackState::Finished => ServiceClass::Idle,
        };
        if let Ok(resource) = self.resource.try_lock() {
            resource.set_service_class(class);
        }
    }

    /// Transition state after a fade completes.
    ///
    /// `FadingIn` → `Playing` (track is now audible).
    /// `FadingOut` → `Finished` (track is silent and should be cleaned up).
    ///
    /// Using `Finished` instead of `Paused` ensures `cleanup_finished_tracks()`
    /// removes the track from the arena. Previously `Paused` left the track
    /// in the arena indefinitely, leaking resources on every crossfade.
    fn update_state_after_fade(&mut self) {
        let new_state = match self.state {
            TrackState::FadingIn => TrackState::Playing,
            TrackState::FadingOut => TrackState::Finished,
            current => current,
        };
        self.set_state(new_state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use kithara_audio::PcmReader;
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmSpec, TrackMetadata};
    use kithara_events::EventBus;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Split},
    };

    use super::*;
    use crate::impls::resource::Resource;

    fn mock_spec() -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
    }

    #[kithara::test]
    fn seek_frame_index_clamps_unrepresentable_targets() {
        assert_eq!(
            PlayerTrack::seek_frame_index(f64::INFINITY, 44_100, 10.0),
            441_000
        );
        assert_eq!(PlayerTrack::seek_frame_index(f64::INFINITY, 44_100, 0.0), 0);
        assert_eq!(PlayerTrack::seek_frame_index(f64::NAN, 44_100, 10.0), 0);
    }

    fn make_track_from_resource(
        resource: Resource,
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    ) -> PlayerTrack {
        let player_resource = PlayerResource::new(resource, Arc::clone(&src), &PcmPool::default());
        let arc_resource = Arc::new(Mutex::new(player_resource));
        let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
        PlayerTrack::new(
            arc_resource,
            item_id,
            src,
            1.0,
            0.0,
            sample_rate,
            FadeCurve::SquareRoot,
        )
    }

    /// `PcmReader` fixture that reports a 10s duration but actually serves
    /// a much shorter buffer. Used to verify that the gapless trigger
    /// logic relies on observed-EOF, not on a possibly-stale duration.
    struct MisreportedDurationReader {
        bus: EventBus,
        spec: PcmSpec,
        metadata: TrackMetadata,
        position_frames: usize,
        remaining_frames: usize,
    }

    impl MisreportedDurationReader {
        fn new(actual_frames: usize) -> Self {
            Self {
                bus: EventBus::default(),
                metadata: TrackMetadata::default(),
                remaining_frames: actual_frames,
                position_frames: 0,
                spec: mock_spec(),
            }
        }
    }

    impl PcmReader for MisreportedDurationReader {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(10))
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn position(&self) -> Duration {
            let frames =
                u64::try_from(self.position_frames).expect("test mock position non-negative");
            Duration::from_micros(frames * 1_000_000 / u64::from(self.spec.sample_rate.get()))
        }

        fn read(
            &mut self,
            buf: &mut [f32],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            let channels = usize::from(self.spec.channels);
            let frames = (buf.len() / channels).min(self.remaining_frames);
            if frames == 0 {
                return Ok(kithara_audio::ReadOutcome::Eof {
                    position: self.position(),
                });
            }
            let samples = frames.saturating_mul(channels);
            buf[..samples].fill(0.5);
            self.remaining_frames -= frames;
            self.position_frames += frames;
            Ok(kithara_audio::ReadOutcome::Frames {
                count: std::num::NonZeroUsize::new(frames).expect("BUG: frames > 0"),
                position: self.position(),
            })
        }

        fn read_planar<'a>(
            &mut self,
            output: &'a mut [&'a mut [f32]],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            let frames = output
                .iter()
                .map(|channel| channel.len())
                .min()
                .unwrap_or(0)
                .min(self.remaining_frames);
            if frames == 0 {
                return Ok(kithara_audio::ReadOutcome::Eof {
                    position: self.position(),
                });
            }
            for channel in output.iter_mut() {
                channel[..frames].fill(0.5);
            }
            self.remaining_frames -= frames;
            self.position_frames += frames;
            Ok(kithara_audio::ReadOutcome::Frames {
                count: std::num::NonZeroUsize::new(frames).expect("BUG: frames > 0"),
                position: self.position(),
            })
        }

        fn seek(
            &mut self,
            position: Duration,
        ) -> Result<kithara_audio::SeekOutcome, kithara_audio::DecodeError> {
            let _ = position;
            Ok(kithara_audio::SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    fn collect_notifications(
        rx: &mut impl Consumer<Item = PlayerNotification>,
    ) -> Vec<PlayerNotification> {
        let mut notifications = Vec::new();
        while let Some(notification) = rx.try_pop() {
            notifications.push(notification);
        }
        notifications
    }

    #[kithara::test]
    fn track_state_is_playing() {
        assert!(TrackState::Playing.is_playing());
        assert!(TrackState::FadingIn.is_playing());
        assert!(TrackState::FadingOut.is_playing());
        assert!(!TrackState::Preloading.is_playing());
        assert!(!TrackState::Finished.is_playing());
    }

    #[kithara::test]
    fn track_state_is_leading() {
        assert!(TrackState::Playing.is_leading());
        assert!(TrackState::FadingIn.is_leading());
        assert!(!TrackState::FadingOut.is_leading());
        assert!(!TrackState::Preloading.is_leading());
        assert!(!TrackState::Finished.is_leading());
    }

    /// `PcmReader` fixture whose decoded frontier the test advances *after* the
    /// track is built — modelling the decode worker filling lookahead on its
    /// own thread while the player is paused (no render cycle refreshes the
    /// `observed_frontier` cache).
    struct LiveFrontierReader {
        bus: EventBus,
        spec: PcmSpec,
        metadata: TrackMetadata,
        frontier_ns: Arc<AtomicU64>,
    }

    impl PcmReader for LiveFrontierReader {
        fn decoded_frontier(&self) -> Duration {
            Duration::from_nanos(self.frontier_ns.load(Ordering::Relaxed))
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(180))
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn position(&self) -> Duration {
            Duration::ZERO
        }

        fn read(
            &mut self,
            _buf: &mut [f32],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }

        fn read_planar<'a>(
            &mut self,
            _output: &'a mut [&'a mut [f32]],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }

        fn seek(
            &mut self,
            position: Duration,
        ) -> Result<kithara_audio::SeekOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    #[kithara::test]
    fn decoded_frontier_reads_live_resource_not_stale_render_cache() {
        // The decode worker advances the frontier on its own thread; while the
        // player is paused no render cycle runs, so the `observed_frontier`
        // cache stays at its construction value. `decoded_frontier()` must read
        // the live resource (mirroring `duration()`) so the FFI buffered/loaded
        // ranges keep growing past a forward-seek target while paused. Without
        // it the host app's `isPlayable` gate never reopens and seek hangs.
        let frontier_ns = Arc::new(AtomicU64::new(0));
        let reader = LiveFrontierReader {
            bus: EventBus::default(),
            spec: mock_spec(),
            metadata: TrackMetadata::default(),
            frontier_ns: Arc::clone(&frontier_ns),
        };
        let src: Arc<str> = Arc::from("frontier.flac");
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        let track = make_track_from_resource(resource, src, None);

        // No `read()` has run (paused / no render): cache is the initial 0.
        assert_eq!(track.decoded_frontier(), 0.0);

        // Decode worker fills lookahead past the forward-seek target while paused.
        frontier_ns.store(
            u64::try_from(Duration::from_millis(81_000).as_nanos()).expect("fits in u64"),
            Ordering::Relaxed,
        );

        // Must reflect the live decoded frontier, not the stale render cache.
        let live = track.decoded_frontier();
        assert!(
            (live - 81.0).abs() < 1e-6,
            "decoded_frontier must read the live resource, got {live}"
        );
    }

    #[kithara::test(tokio)]
    async fn handover_uses_buffered_eof_when_duration_is_overestimated() {
        let src = Arc::from("misreported.mp3");
        let resource =
            Resource::from_reader(MisreportedDurationReader::new(900), Some(Arc::clone(&src)));
        let mut track = make_track_from_resource(resource, src, None);
        let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
        track.update_fade_duration(0.0, sample_rate);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(16).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();

        let outcome = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

        assert!(matches!(
            outcome,
            TrackReadOutcome::Full {
                frames_until_eof: Some(388),
                duration,
                ..
            } if duration < 10.0
        ));
        let notifications = collect_notifications(&mut rx);
        assert!(
            notifications.iter().any(|notification| {
                matches!(notification, PlayerNotification::HandoverRequested)
            }),
            "handover must be emitted before the EOF block when the resource has already observed EOF"
        );
        assert!(
            !notifications.iter().any(|notification| {
                matches!(
                    notification,
                    PlayerNotification::PlaybackStopped {
                        reason: TrackPlaybackStopReason::Eof,
                        ..
                    }
                )
            }),
            "first full block must only request preload, not emit EOF"
        );
    }

    #[kithara::test]
    #[case(TrackState::Playing, ServiceClass::Audible)]
    #[case(TrackState::FadingIn, ServiceClass::Audible)]
    #[case(TrackState::FadingOut, ServiceClass::Audible)]
    #[case(TrackState::Preloading, ServiceClass::Warm)]
    #[case(TrackState::Finished, ServiceClass::Idle)]
    fn track_state_maps_to_service_class(
        #[case] state: TrackState,
        #[case] expected: ServiceClass,
    ) {
        let class = match state {
            TrackState::Playing | TrackState::FadingIn | TrackState::FadingOut => {
                ServiceClass::Audible
            }
            TrackState::Preloading => ServiceClass::Warm,
            TrackState::Finished => ServiceClass::Idle,
        };
        assert_eq!(class, expected);
    }
}
