use std::num::NonZeroU32;

use bon::Builder;
use firewheel::dsp::fade::FadeCurve;
use kithara_audio::ServiceClass;
use kithara_platform::sync::Arc;
use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::{PlayerResource, fade::TrackFade, triggers::TrackTriggers};
use crate::bridge::TrackState;

/// Parameters used to create a track around an owned resource.
#[derive(Builder)]
pub struct TrackParams {
    item_id: Option<Arc<str>>,
    src: Arc<str>,
    #[builder(default)]
    fade_duration: f32,
    #[builder(default)]
    prefetch_duration: f32,
    sample_rate: NonZeroU32,
    #[builder(default = FadeCurve::SquareRoot)]
    fade_curve: FadeCurve,
}

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerTrack {
    pub(super) resource: Box<PlayerResource>,
    pub(super) fade: TrackFade,
    pub(super) item_id: Option<Arc<str>>,
    #[field(get, copy)]
    pub(super) state: TrackState,
    /// Set only when the track reaches *natural* EOF (`handle_natural_end`).
    /// Marks a played-out track as eligible to be kept warm at end-of-queue
    /// and revived by a later in-range seek (Superpowered-style resume).
    /// Cleared by `seek`/`play`. A `Finished` state from `stop()` or a
    /// faded-out crossfade leaves this `false`, so those are discarded as usual.
    #[field(get)]
    pub(super) ended_at_eof: bool,
    pub(super) triggers: TrackTriggers,
    pub(super) state_dirty: bool,
    /// Lead time before EOF at which the prefetch trigger fires.
    ///
    /// Effective preload threshold is
    /// `max(prefetch_duration, fade_duration) + block_seconds`, so prefetch
    /// is at least as eager as the crossfade trigger and can be set
    /// independently to cover network/probe latency.
    pub(super) prefetch_duration: f32,
    /// Last observed duration snapshot.
    ///
    /// Mirrors `PlayerResource::duration()` (post-gapless-trim, visible
    /// duration) captured under the resource lock.
    pub(super) observed_duration: f64,
    pub(super) sample_rate: u32,
    /// Cumulative frames this track has actually served into the mix output.
    ///
    /// Used as the source of truth for near-end trigger position so the
    /// trigger reflects what has been rendered to the audio output, not the
    /// decoder's pre-buffered position (which can be ~200 ms ahead of the
    /// mixer thanks to `PlayerResource`'s scratch buffer).
    pub(super) served_frames: u64,
}

impl PlayerTrack {
    /// Create a new track in the `Preloading` state.
    ///
    /// The `MixDSP` starts at `FULLY_WET` (silent) so that an explicit
    /// `fade_in()` or `play()` is required to produce audio.
    #[must_use]
    pub fn new(resource: Box<PlayerResource>, params: TrackParams) -> Self {
        let TrackParams {
            item_id,
            src: _src,
            fade_duration,
            prefetch_duration,
            sample_rate,
            fade_curve,
        } = params;
        let observed_duration = resource.duration();
        let track = Self {
            resource,
            item_id,
            state: TrackState::Preloading,
            state_dirty: false,
            triggers: TrackTriggers::default(),
            fade: TrackFade::new(fade_duration, fade_curve, sample_rate),
            prefetch_duration: prefetch_duration.max(0.0),
            sample_rate: sample_rate.get(),
            served_frames: 0,
            observed_duration,
            ended_at_eof: false,
        };
        track.update_service_class(TrackState::Preloading);
        track
    }

    /// Decoded-ahead frontier in seconds.
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource.decoded_frontier()
    }

    /// Current visible (post-gapless-trim) duration in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        observed_duration(self.observed_duration, self.resource.duration())
    }

    /// Start a fade-in: transitions to `FadingIn`, targets `FULLY_DRY` (audible).
    pub fn fade_in(&mut self) {
        self.set_state(TrackState::FadingIn);
        self.fade.fade_in();
        self.triggers.reset();
    }

    /// Start a fade-out: transitions to `FadingOut`, targets `FULLY_WET` (silent).
    pub fn fade_out(&mut self) {
        self.set_state(TrackState::FadingOut);
        self.fade.fade_out();
    }

    /// Instantly start playing at full volume.
    pub fn play(&mut self) {
        self.set_state(TrackState::Playing);
        self.fade.play();
        self.triggers.reset();
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

    /// Reference to the owned resource.
    #[must_use]
    pub fn resource(&self) -> &PlayerResource {
        &self.resource
    }

    /// Seek the underlying resource and re-sync the served-frame counter
    /// so trigger thresholds reflect the new playback origin.
    pub fn seek(&mut self, seconds: f64) {
        self.resource.seek(seconds);
        let frames = seek_frame_index(seconds, self.sample_rate, self.observed_duration);
        self.served_frames = frames;
        self.triggers.reset();
        self.ended_at_eof = false;
    }

    /// Update the prefetch lead time used for the preload trigger.
    pub fn set_prefetch_duration(&mut self, prefetch_duration: f32) {
        self.prefetch_duration = prefetch_duration.max(0.0);
    }

    /// Set the track state and mark as dirty.
    ///
    /// Also updates the shared worker's scheduling priority via
    /// [`ServiceClass`] bridge: Audible tracks get highest priority.
    pub(super) fn set_state(&mut self, new_state: TrackState) {
        if self.state != new_state {
            self.state = new_state;
            self.state_dirty = true;
            self.update_service_class(new_state);
        }
    }

    /// Source identifier.
    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        self.resource.src()
    }

    /// Instantly stop (silent, finished state).
    pub fn stop(&mut self) {
        self.set_state(TrackState::Finished);
        self.fade.stop();
    }

    /// Re-create the `MixDSP` with a new fade duration.
    pub fn update_fade_duration(&mut self, fade_duration: f32, sample_rate: NonZeroU32) {
        self.fade
            .update_duration(fade_duration, sample_rate, self.state.is_leading());
        self.sample_rate = sample_rate.get();
    }

    /// Map track state to worker scheduling priority and push the update.
    fn update_service_class(&self, state: TrackState) {
        self.resource
            .set_service_class(service_class_for_state(state));
    }
}

fn observed_duration(observed: f64, resource: f64) -> f64 {
    if observed > 0.0 { observed } else { resource }
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

fn service_class_for_state(state: TrackState) -> ServiceClass {
    match state {
        TrackState::Playing | TrackState::FadingIn | TrackState::FadingOut => ServiceClass::Audible,
        TrackState::Preloading => ServiceClass::Warm,
        TrackState::Finished => ServiceClass::Idle,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn seek_frame_index_clamps_unrepresentable_targets() {
        assert_eq!(seek_frame_index(f64::INFINITY, 44_100, 10.0), 441_000);
        assert_eq!(seek_frame_index(f64::INFINITY, 44_100, 0.0), 0);
        assert_eq!(seek_frame_index(f64::NAN, 44_100, 10.0), 0);
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
