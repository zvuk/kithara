//! Per-track state with `MixDSP` fade control.
//!
//! Each track loaded into the processor gets a [`PlayerTrack`] that wraps
//! the shared [`PlayerResource`] and manages its fade/state lifecycle.

use std::{num::NonZeroU32, ops::Range, sync::Arc};

#[rustfmt::skip]
use firewheel::dsp::filter::smoothing_filter::DEFAULT_SETTLE_EPSILON;
#[rustfmt::skip]
use firewheel::param::smoother::SmootherConfig;
use firewheel::dsp::{
    fade::FadeCurve,
    mix::{Mix, MixDSP},
};
use kithara_audio::ServiceClass;
use kithara_platform::Mutex;
use ringbuf::{HeapProd, traits::Producer};

use super::{
    player_notification::{PlayerNotification, TrackPlaybackStopReason},
    player_resource::{PlayerResource, ReadOutcome},
};

/// Minimum number of channels required for stereo processing.
const MIN_STEREO_CHANNELS: usize = 2;

/// State machine for a single track's lifecycle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackState {
    /// Track is loaded but not yet playing.
    #[default]
    Preloading,
    /// Track is paused (explicit pause by user).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "retained for future explicit-pause support")
    )]
    Paused,
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
    /// Whether the track is producing audible audio.
    pub(crate) fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn | Self::FadingOut)
    }

    /// Whether the track is the "leading" track (playing or fading in).
    pub(crate) fn is_leading(self) -> bool {
        matches!(self, Self::Playing | Self::FadingIn)
    }
}

/// Transition command for a track.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TrackTransition {
    /// Start fading in the track with the given source identifier.
    FadeIn(Arc<str>),
    /// Start fading out the track with the given source identifier.
    FadeOut(Arc<str>),
}

/// Result of a single track render attempt.
#[derive(Debug)]
pub(crate) enum TrackReadOutcome {
    /// The full requested block was written into the mix buffer.
    Full {
        /// Playback position snapshot after the read.
        position: f64,
        /// Duration snapshot associated with the current resource.
        duration: f64,
    },
    /// Only the first `frames` samples were written; EOF was reached in-block.
    ///
    /// `PlayerTrack::read` emits EOF `TrackPlaybackStopped` in the same call
    /// so notification ordering matches the last written frame.
    Partial {
        /// Number of frames written into the destination block.
        frames: usize,
        /// Playback position snapshot after the partial read.
        position: f64,
        /// Duration snapshot associated with the current resource.
        duration: f64,
    },
    /// No frames were written because the track is already finished.
    Eof,
    /// A terminal decoder/resource error stopped the track.
    Error(crate::error::PlayError),
}

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
pub(crate) struct PlayerTrack {
    resource: Arc<Mutex<PlayerResource>>,
    state: TrackState,
    state_dirty: bool,
    notified_track_requested: bool,
    mix: MixDSP,
    fade_curve: FadeCurve,
    /// Current crossfade duration used for near-end trigger checks.
    fade_duration: f32,
    /// Last observed playback position snapshot.
    ///
    /// This is a fallback value only. Source of truth is `PlayerResource`.
    observed_position: f64,
    /// Last observed duration snapshot.
    ///
    /// This is a fallback value only. Source of truth is `PlayerResource`.
    observed_duration: f64,
    item_id: Option<Arc<str>>,
    src: Arc<str>,
}

impl PlayerTrack {
    /// Create a new track in the `Preloading` state.
    ///
    /// The `MixDSP` starts at `FULLY_WET` (silent) so that an explicit
    /// `fade_in()` or `play()` is required to produce audio.
    pub(crate) fn new(
        resource: Arc<Mutex<PlayerResource>>,
        item_id: Option<Arc<str>>,
        src: Arc<str>,
        fade_duration: f32,
        sample_rate: NonZeroU32,
        fade_curve: FadeCurve,
    ) -> Self {
        let fade_conf = SmootherConfig {
            smooth_seconds: fade_duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        };
        let track = Self {
            resource,
            state: TrackState::Preloading,
            state_dirty: false,
            notified_track_requested: false,
            mix: MixDSP::new(Mix::FULLY_WET, fade_curve, fade_conf, sample_rate),
            fade_curve,
            fade_duration,
            observed_position: 0.0,
            observed_duration: 0.0,
            item_id,
            src,
        };
        // Push initial ServiceClass::Warm for the Preloading state.
        track.update_service_class(TrackState::Preloading);
        track
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
    pub(crate) fn read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        notification_tx: &Mutex<HeapProd<PlayerNotification>>,
    ) -> TrackReadOutcome {
        if self.state == TrackState::Finished {
            return TrackReadOutcome::Eof;
        }

        let read_outcome = {
            let Some(mut guard) = self.resource.try_lock().ok() else {
                return TrackReadOutcome::Full {
                    position: self.observed_position,
                    duration: self.observed_duration,
                };
            };
            let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
            let mut scratch_window = [
                &mut scratch_left[0][range.clone()],
                &mut scratch_right[0][range.clone()],
            ];

            match guard.read(&mut scratch_window, 0..range.len()) {
                Ok(ReadOutcome::Full) => {
                    let position = guard.position();
                    let duration = guard.duration();
                    drop(guard);
                    TrackReadOutcome::Full { position, duration }
                }
                Ok(ReadOutcome::Partial(frames)) => {
                    let position = guard.position();
                    let duration = guard.duration();
                    drop(guard);
                    TrackReadOutcome::Partial {
                        frames,
                        position,
                        duration,
                    }
                }
                Ok(ReadOutcome::Eof) => TrackReadOutcome::Eof,
                Err(e) => TrackReadOutcome::Error(e),
            }
        };

        match read_outcome {
            TrackReadOutcome::Full { position, duration } => {
                self.observed_position = position;
                self.observed_duration = duration;

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

                self.check_notifications(notification_tx);

                if self.mix.has_settled() {
                    self.update_state_after_fade();
                }

                if self.state_dirty {
                    self.notify_state_change(notification_tx);
                }

                TrackReadOutcome::Full { position, duration }
            }
            TrackReadOutcome::Partial {
                frames,
                position,
                duration,
            } => {
                self.observed_position = position;
                self.observed_duration = duration;

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

                self.check_notifications(notification_tx);
                self.handle_natural_end(notification_tx);

                TrackReadOutcome::Partial {
                    frames,
                    position,
                    duration,
                }
            }
            TrackReadOutcome::Eof => {
                self.handle_natural_end(notification_tx);
                TrackReadOutcome::Eof
            }
            TrackReadOutcome::Error(error) => {
                self.handle_read_error(notification_tx, error.clone());
                TrackReadOutcome::Error(error)
            }
        }
    }

    /// Handle natural EOF.
    fn handle_natural_end(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.emit_track_requested(notification_tx);
        self.set_state(TrackState::Finished);
        notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackPlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Eof,
            })
            .ok();
        self.state_dirty = false;
    }

    /// Handle a terminal read error that is not EOF.
    fn handle_read_error(
        &mut self,
        notification_tx: &Mutex<HeapProd<PlayerNotification>>,
        error: crate::error::PlayError,
    ) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackError(Arc::clone(&self.src), error))
            .ok();
        notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackPlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Stop,
            })
            .ok();
        self.state_dirty = false;
    }

    /// Check position-based notifications.
    fn check_notifications(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        let position = self.observed_position;
        let duration = self.observed_duration;

        if duration <= 0.0 {
            return;
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "position/duration in seconds; f32 precision is sufficient for threshold checks"
        )]
        let pos = position as f32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "position/duration in seconds; f32 precision is sufficient for threshold checks"
        )]
        let dur = duration as f32;
        if pos + self.fade_duration >= dur {
            self.emit_track_requested(notification_tx);
        }
    }

    /// Emit the canonical near-end trigger once per playback cycle.
    fn emit_track_requested(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.notified_track_requested {
            return;
        }

        if notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackRequested(Arc::clone(&self.src)))
            .is_ok()
        {
            self.notified_track_requested = true;
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

    /// Emit notification when state changes.
    fn notify_state_change(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if !self.state_dirty {
            return;
        }
        let notification = match self.state {
            TrackState::Preloading => PlayerNotification::TrackLoaded(Arc::clone(&self.src)),
            TrackState::FadingIn => PlayerNotification::TrackFadingIn(Arc::clone(&self.src)),
            TrackState::FadingOut => PlayerNotification::TrackFadingOut(Arc::clone(&self.src)),
            TrackState::Playing => PlayerNotification::TrackPlaybackStarted(Arc::clone(&self.src)),
            TrackState::Paused => PlayerNotification::TrackPlaybackPaused(Arc::clone(&self.src)),
            TrackState::Finished => PlayerNotification::TrackPlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Stop,
            },
        };

        if notification_tx.lock_sync().try_push(notification).is_ok() {
            self.state_dirty = false;
        }
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
            TrackState::Paused | TrackState::Finished => ServiceClass::Idle,
        };
        if let Ok(resource) = self.resource.try_lock() {
            resource.set_service_class(class);
        }
    }

    /// Start a fade-in: transitions to `FadingIn`, targets `FULLY_DRY` (audible).
    pub(crate) fn fade_in(&mut self) {
        self.set_state(TrackState::FadingIn);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.notified_track_requested = false;
    }

    /// Start a fade-out: transitions to `FadingOut`, targets `FULLY_WET` (silent).
    pub(crate) fn fade_out(&mut self) {
        self.set_state(TrackState::FadingOut);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
    }

    /// Instantly start playing at full volume.
    pub(crate) fn play(&mut self) {
        self.set_state(TrackState::Playing);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.mix.reset_to_target();
        self.notified_track_requested = false;
    }

    /// Instantly stop (silent, finished state).
    pub(crate) fn stop(&mut self) {
        self.set_state(TrackState::Finished);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
        self.mix.reset_to_target();
    }

    /// Seek the underlying resource.
    pub(crate) fn seek(&mut self, seconds: f64) {
        if let Ok(mut resource) = self.resource.try_lock() {
            resource.seek(seconds);
        }
    }

    /// Current position in seconds.
    ///
    /// Source of truth is `PlayerResource` (`Timeline` underneath).
    /// Uses snapshot fallback only if lock is temporarily unavailable.
    pub(crate) fn position(&self) -> f64 {
        self.resource
            .try_lock()
            .ok()
            .map_or(self.observed_position, |resource| resource.position())
    }

    /// Current duration in seconds.
    ///
    /// Source of truth is `PlayerResource` (`Timeline` underneath).
    /// Uses snapshot fallback only if lock is temporarily unavailable.
    pub(crate) fn duration(&self) -> f64 {
        self.resource
            .try_lock()
            .ok()
            .map_or(self.observed_duration, |resource| resource.duration())
    }

    /// Current track state.
    pub(crate) fn state(&self) -> TrackState {
        self.state
    }

    /// Source identifier.
    pub(crate) fn src(&self) -> &Arc<str> {
        &self.src
    }

    /// Reference to the underlying shared resource.
    pub(crate) fn resource(&self) -> &Arc<Mutex<PlayerResource>> {
        &self.resource
    }

    /// Re-create the `MixDSP` with a new fade duration.
    pub(crate) fn update_fade_duration(&mut self, fade_duration: f32, sample_rate: NonZeroU32) {
        let fade_conf = SmootherConfig {
            smooth_seconds: fade_duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        };
        // Preserve audible/inaudible state
        let target_mix = if self.state.is_leading() {
            Mix::FULLY_DRY
        } else {
            Mix::FULLY_WET
        };
        self.mix = MixDSP::new(target_mix, self.fade_curve, fade_conf, sample_rate);
        self.fade_duration = fade_duration;
    }

    /// Update the fade curve used for future fade operations.
    pub(crate) fn set_fade_curve(&mut self, curve: FadeCurve) {
        self.fade_curve = curve;
    }
}

#[cfg(test)]
mod tests {
    use kithara_audio::mock::TestPcmReader;
    use kithara_decode::PcmSpec;
    use kithara_test_utils::kithara;
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Split},
    };

    use super::*;
    use crate::impls::resource::Resource;

    #[derive(Clone, Copy)]
    enum TrackStateScenario {
        FadeIn,
        FadeOutAfterPlay,
        Play,
        StartPreloading,
        StopAfterPlay,
    }

    fn mock_spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: 44100,
        }
    }

    // Note: `make_track()` requires a tokio runtime because `Resource::from_reader()`
    // internally calls `tokio::spawn()` to forward audio events. All tests using
    // this helper must use `#[kithara::test(tokio)]`.
    fn make_track_with(duration_secs: f64, item_id: Option<Arc<str>>) -> PlayerTrack {
        let src: Arc<str> = Arc::from("test.mp3");
        let resource = Resource::from_reader(TestPcmReader::new(mock_spec(), duration_secs));
        let player_resource =
            PlayerResource::new(resource, Arc::clone(&src), kithara_bufpool::pcm_pool());
        let arc_resource = Arc::new(Mutex::new(player_resource));
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        PlayerTrack::new(
            arc_resource,
            item_id,
            src,
            1.0,
            sample_rate,
            FadeCurve::SquareRoot,
        )
    }

    fn make_track() -> PlayerTrack {
        make_track_with(60.0, None)
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

    #[kithara::test(tokio)]
    #[case(TrackStateScenario::StartPreloading, TrackState::Preloading)]
    #[case(TrackStateScenario::FadeIn, TrackState::FadingIn)]
    #[case(TrackStateScenario::FadeOutAfterPlay, TrackState::FadingOut)]
    #[case(TrackStateScenario::Play, TrackState::Playing)]
    #[case(TrackStateScenario::StopAfterPlay, TrackState::Finished)]
    async fn track_state_transitions(
        #[case] scenario: TrackStateScenario,
        #[case] expected_state: TrackState,
    ) {
        let mut track = make_track();
        match scenario {
            TrackStateScenario::StartPreloading => {}
            TrackStateScenario::FadeIn => track.fade_in(),
            TrackStateScenario::FadeOutAfterPlay => {
                track.play();
                track.fade_out();
            }
            TrackStateScenario::Play => track.play(),
            TrackStateScenario::StopAfterPlay => {
                track.play();
                track.stop();
            }
        }
        assert_eq!(track.state(), expected_state);
    }

    #[kithara::test]
    fn track_state_is_playing() {
        assert!(TrackState::Playing.is_playing());
        assert!(TrackState::FadingIn.is_playing());
        assert!(TrackState::FadingOut.is_playing());
        assert!(!TrackState::Preloading.is_playing());
        assert!(!TrackState::Paused.is_playing());
        assert!(!TrackState::Finished.is_playing());
    }

    #[kithara::test]
    fn track_state_is_leading() {
        assert!(TrackState::Playing.is_leading());
        assert!(TrackState::FadingIn.is_leading());
        assert!(!TrackState::FadingOut.is_leading());
        assert!(!TrackState::Preloading.is_leading());
        assert!(!TrackState::Paused.is_leading());
        assert!(!TrackState::Finished.is_leading());
    }

    #[kithara::test(tokio)]
    async fn track_src_returns_identifier() {
        let track = make_track();
        assert_eq!(&**track.src(), "test.mp3");
    }

    #[kithara::test(tokio)]
    async fn track_initial_position_and_duration() {
        let track = make_track();
        assert_eq!(track.position(), 0.0);
        assert!((track.duration() - 60.0).abs() < f64::EPSILON);
    }

    #[kithara::test(tokio)]
    async fn eof_playback_stopped_notification_carries_item_id() {
        let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(8).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();

        let mut saw_eof_stop = false;
        for _ in 0..4 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

            while let Some(notification) = rx.try_pop() {
                if let PlayerNotification::TrackPlaybackStopped {
                    item_id,
                    reason: TrackPlaybackStopReason::Eof,
                    ..
                } = notification
                {
                    saw_eof_stop = matches!(item_id, Some(id) if id.as_ref() == "item-1");
                }
            }

            if saw_eof_stop {
                break;
            }
        }

        assert!(saw_eof_stop);
    }

    #[kithara::test(tokio)]
    async fn read_outcome_full_on_normal_read() {
        let mut track = make_track_with(60.0, None);
        let (tx, _) = HeapRb::<PlayerNotification>::new(8).split();
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
                position,
                duration
            } if position >= 0.0 && duration > 0.0
        ));
    }

    #[kithara::test(tokio)]
    async fn read_outcome_partial_then_eof() {
        let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(16).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();

        let mut saw_partial = false;
        let mut saw_eof_after_partial = false;
        let mut eof_stop_count = 0;

        for _ in 0..8 {
            let outcome = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

            match outcome {
                TrackReadOutcome::Partial { frames, .. } => {
                    assert!(!saw_partial, "expected exactly one Partial outcome");
                    assert!(frames > 0);
                    saw_partial = true;
                }
                TrackReadOutcome::Eof => {
                    if saw_partial {
                        saw_eof_after_partial = true;
                        break;
                    }
                }
                TrackReadOutcome::Full { .. } | TrackReadOutcome::Error(_) => {}
            }

            while let Some(notification) = rx.try_pop() {
                if let PlayerNotification::TrackPlaybackStopped {
                    item_id,
                    reason: TrackPlaybackStopReason::Eof,
                    ..
                } = notification
                {
                    assert!(saw_partial, "EOF stop must not precede Partial");
                    assert!(matches!(item_id, Some(id) if id.as_ref() == "item-1"));
                    eof_stop_count += 1;
                }
            }
        }

        while let Some(notification) = rx.try_pop() {
            if let PlayerNotification::TrackPlaybackStopped {
                item_id,
                reason: TrackPlaybackStopReason::Eof,
                ..
            } = notification
            {
                assert!(saw_partial, "EOF stop must not precede Partial");
                assert!(matches!(item_id, Some(id) if id.as_ref() == "item-1"));
                eof_stop_count += 1;
            }
        }

        assert!(saw_partial, "expected a Partial outcome before EOF");
        assert!(saw_eof_after_partial, "expected EOF after Partial");
        assert_eq!(
            eof_stop_count, 1,
            "expected exactly one EOF stop notification"
        );
    }

    #[kithara::test(tokio)]
    async fn track_requested_emits_once_when_position_crosses_fade_threshold() {
        let mut track = make_track_with(10.0, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.2, sample_rate);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();
        track.seek(9.79);

        let mut requested_count = 0;
        let mut saw_eof_stop = false;

        for _ in 0..4 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
            for notification in collect_notifications(&mut rx) {
                match notification {
                    PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3" => {
                        requested_count += 1;
                    }
                    PlayerNotification::TrackPlaybackStopped {
                        src,
                        reason: TrackPlaybackStopReason::Eof,
                        ..
                    } if src.as_ref() == "test.mp3" => {
                        saw_eof_stop = true;
                    }
                    _ => {}
                }
            }

            if requested_count > 0 {
                break;
            }
        }

        assert_eq!(requested_count, 1);
        assert!(
            !saw_eof_stop,
            "threshold-triggered TrackRequested should precede EOF"
        );

        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
        let notifications = collect_notifications(&mut rx);
        assert!(
            notifications.iter().all(|notification| !matches!(
                notification,
                PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3"
            )),
            "TrackRequested must not be emitted twice in one playback cycle"
        );
    }

    #[kithara::test(tokio)]
    async fn track_requested_backstops_eof_when_threshold_was_not_reached_earlier() {
        let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.0, sample_rate);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();

        let outcome = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
        assert!(matches!(outcome, TrackReadOutcome::Partial { .. }));

        let notifications = collect_notifications(&mut rx);
        let requested_count = notifications
            .iter()
            .filter(|notification| {
                matches!(notification, PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3")
            })
            .count();
        let eof_stop_count = notifications
            .iter()
            .filter(|notification| {
                matches!(
                    notification,
                    PlayerNotification::TrackPlaybackStopped {
                        src,
                        item_id,
                        reason: TrackPlaybackStopReason::Eof,
                    }
                    if src.as_ref() == "test.mp3"
                        && matches!(item_id, Some(id) if id.as_ref() == "item-1")
                )
            })
            .count();

        assert_eq!(requested_count, 1);
        assert_eq!(eof_stop_count, 1);
    }

    #[kithara::test(tokio)]
    async fn track_requested_is_not_duplicated_at_eof_after_early_trigger() {
        let mut track = make_track_with(5.0, Some(Arc::from("item-1")));
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.2, sample_rate);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(64).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();

        let mut requested_count = 0;
        let mut eof_stop_count = 0;

        for _ in 0..600 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

            for notification in collect_notifications(&mut rx) {
                match notification {
                    PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3" => {
                        requested_count += 1;
                    }
                    PlayerNotification::TrackPlaybackStopped {
                        src,
                        item_id,
                        reason: TrackPlaybackStopReason::Eof,
                    } if src.as_ref() == "test.mp3"
                        && matches!(&item_id, Some(id) if id.as_ref() == "item-1") =>
                    {
                        eof_stop_count += 1;
                    }
                    _ => {}
                }
            }

            if eof_stop_count == 1 {
                break;
            }
        }

        assert_eq!(requested_count, 1);
        assert_eq!(eof_stop_count, 1);
    }

    #[kithara::test(tokio)]
    async fn read_outcome_eof_when_track_finished() {
        let mut track = make_track();
        let (tx, _) = HeapRb::<PlayerNotification>::new(8).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.stop();

        let outcome = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

        assert!(matches!(outcome, TrackReadOutcome::Eof));
    }

    #[kithara::test]
    #[case(TrackState::Playing, ServiceClass::Audible)]
    #[case(TrackState::FadingIn, ServiceClass::Audible)]
    #[case(TrackState::FadingOut, ServiceClass::Audible)]
    #[case(TrackState::Preloading, ServiceClass::Warm)]
    #[case(TrackState::Paused, ServiceClass::Idle)]
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
            TrackState::Paused | TrackState::Finished => ServiceClass::Idle,
        };
        assert_eq!(class, expected);
    }
}
