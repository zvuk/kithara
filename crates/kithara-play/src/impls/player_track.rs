//! Per-track state with `MixDSP` fade control.
//!
//! Each track loaded into the processor gets a [`PlayerTrack`] that wraps
//! the shared [`PlayerResource`] and manages its fade/state lifecycle.

use std::{num::NonZeroU32, ops::Range, sync::Arc};

use firewheel::{
    dsp::{
        fade::FadeCurve,
        filter::smoothing_filter::DEFAULT_SETTLE_EPSILON,
        mix::{Mix, MixDSP},
    },
    param::smoother::SmootherConfig,
};
use kithara_platform::Mutex;
use ringbuf::{HeapProd, traits::Producer};

use super::{player_notification::PlayerNotification, player_resource::PlayerResource};

/// Default fade duration in seconds.
pub(crate) const DEFAULT_FADE_DURATION: f32 = 1.0;

/// Default threshold (fraction of duration) for "about to end" notification.
pub(crate) const DEFAULT_TRACK_END_THRESHOLD: f32 = 0.8;

/// State machine for a single track's lifecycle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackState {
    /// Track is loaded but not yet playing.
    #[default]
    Preloading,
    /// Track is paused (explicit pause by user).
    #[expect(dead_code, reason = "retained for future explicit-pause support")]
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

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
pub(crate) struct PlayerTrack {
    resource: Arc<Mutex<PlayerResource>>,
    state: TrackState,
    state_dirty: bool,
    notified_about_to_end: bool,
    notified_track_requested: bool,
    mix: MixDSP,
    fade_curve: FadeCurve,
    /// Last observed playback position snapshot.
    ///
    /// This is a fallback value only. Source of truth is `PlayerResource`.
    observed_position: f64,
    /// Last observed duration snapshot.
    ///
    /// This is a fallback value only. Source of truth is `PlayerResource`.
    observed_duration: f64,
    src: Arc<str>,
}

impl PlayerTrack {
    /// Create a new track in the `Preloading` state.
    ///
    /// The `MixDSP` starts at `FULLY_WET` (silent) so that an explicit
    /// `fade_in()` or `play()` is required to produce audio.
    pub(crate) fn new(
        resource: Arc<Mutex<PlayerResource>>,
        src: Arc<str>,
        fade_duration: f32,
        sample_rate: NonZeroU32,
        fade_curve: FadeCurve,
    ) -> Self {
        let fade_conf = SmootherConfig {
            smooth_seconds: fade_duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        };
        Self {
            resource,
            state: TrackState::Preloading,
            state_dirty: false,
            notified_about_to_end: false,
            notified_track_requested: false,
            mix: MixDSP::new(Mix::FULLY_WET, fade_curve, fade_conf, sample_rate),
            fade_curve,
            observed_position: 0.0,
            observed_duration: 0.0,
            src,
        }
    }

    /// Read audio from this track into scratch/mix buffers.
    ///
    /// 1. Tries to lock the resource (non-blocking).
    /// 2. Reads PCM into `scratch_bufs`.
    /// 3. Mixes through `MixDSP` into `mix_bufs`.
    /// 4. Detects fade completion and updates state.
    pub(crate) fn read(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        notification_tx: &Mutex<HeapProd<PlayerNotification>>,
    ) {
        // Read data from resource inside a scoped lock
        let read_outcome = {
            let Some(mut guard) = self.resource.try_lock().ok() else {
                return; // Can't lock, skip this cycle
            };
            match guard.read(scratch_bufs, range.clone()) {
                Ok(()) => {
                    let position = guard.position();
                    let duration = guard.duration();
                    drop(guard);
                    Ok((position, duration))
                }
                Err(e) => Err(e),
            }
        };

        // Process outcome outside the lock
        if let Ok((position, duration)) = read_outcome {
            self.observed_position = position;
            self.observed_duration = duration;

            if scratch_bufs.len() >= 2 && mix_bufs.len() >= 2 {
                let (output_l_slice, output_r_slice) = mix_bufs.split_at_mut(1);
                let output_l = &mut output_l_slice[0];
                let output_r = &mut output_r_slice[0];

                self.mix.mix_dry_into_wet_stereo(
                    scratch_bufs[0],
                    scratch_bufs[1],
                    output_l,
                    output_r,
                    range.len(),
                );
            }

            self.check_notifications(notification_tx);
        } else {
            self.handle_eof(notification_tx);
            return;
        }

        // Post-processing: check if fade settled
        if self.mix.has_settled() {
            self.update_state_after_fade();
        }

        if self.state_dirty {
            self.notify_state_change(notification_tx);
        }
    }

    /// Handle EOF or read error.
    fn handle_eof(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.state == TrackState::Finished {
            return;
        }
        self.set_state(TrackState::Finished);
        notification_tx
            .lock_sync()
            .try_push(PlayerNotification::TrackPlaybackStopped(Arc::clone(
                &self.src,
            )))
            .ok();
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
        let fade = DEFAULT_FADE_DURATION;

        // TrackAboutToEnd
        if !self.notified_about_to_end {
            let threshold = (dur * DEFAULT_TRACK_END_THRESHOLD) - fade;
            if pos >= threshold.max(0.0) {
                notification_tx
                    .lock_sync()
                    .try_push(PlayerNotification::TrackAboutToEnd(Arc::clone(&self.src)))
                    .ok();
                self.notified_about_to_end = true;
            }
        }

        // TrackRequested
        if !self.notified_track_requested {
            let threshold = dur - fade;
            if pos >= threshold.max(0.0)
                && notification_tx
                    .lock_sync()
                    .try_push(PlayerNotification::TrackRequested(Arc::clone(&self.src)))
                    .is_ok()
            {
                self.notified_track_requested = true;
            }
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
            TrackState::Finished => PlayerNotification::TrackPlaybackStopped(Arc::clone(&self.src)),
        };

        if notification_tx.lock_sync().try_push(notification).is_ok() {
            self.state_dirty = false;
        }
    }

    /// Set the track state and mark as dirty.
    fn set_state(&mut self, new_state: TrackState) {
        if self.state != new_state {
            self.state = new_state;
            self.state_dirty = true;
        }
    }

    /// Start a fade-in: transitions to `FadingIn`, targets `FULLY_DRY` (audible).
    pub(crate) fn fade_in(&mut self) {
        self.set_state(TrackState::FadingIn);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.notified_about_to_end = false;
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
        self.notified_about_to_end = false;
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
    fn make_track() -> PlayerTrack {
        let src: Arc<str> = Arc::from("test.mp3");
        let resource = Resource::from_reader(TestPcmReader::new(mock_spec(), 60.0));
        let player_resource =
            PlayerResource::new(resource, Arc::clone(&src), kithara_bufpool::pcm_pool());
        let arc_resource = Arc::new(Mutex::new(player_resource));
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        PlayerTrack::new(arc_resource, src, 1.0, sample_rate, FadeCurve::SquareRoot)
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
}
