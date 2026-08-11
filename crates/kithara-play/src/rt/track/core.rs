use std::num::NonZeroU32;

use bon::bon;
use firewheel::dsp::fade::FadeCurve;
use kithara_audio::ServiceClass;
use kithara_platform::sync::Arc;
use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::{PlayerResource, fade::TrackFade, start::TrackStart, triggers::TrackTriggers};
use crate::bridge::TrackState;

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerTrack {
    pub(super) resource: Box<PlayerResource>,
    pub(super) item_id: Option<Arc<str>>,
    pub(super) fade: TrackFade,
    #[field(get, copy)]
    pub(super) state: TrackState,
    /// When this track becomes audible. Read once, when it is promoted.
    #[field(get, copy)]
    pub(super) start: TrackStart,
    /// Offset inside the block a beat-anchored start landed on, consumed by
    /// the first read after the promotion so the track is silent before its
    /// own beat.
    pub(super) start_offset: Option<usize>,
    pub(super) triggers: TrackTriggers,
    /// Set only when the track reaches *natural* EOF (`handle_natural_end`).
    /// Marks a played-out track as eligible to be kept warm at end-of-queue
    /// and revived by a later in-range seek (Superpowered-style resume).
    /// Cleared by `seek`/`play`. A `Finished` state from `stop()` or a
    /// faded-out crossfade leaves this `false`, so those are discarded as usual.
    #[field(get)]
    pub(super) ended_at_eof: bool,
    pub(super) state_dirty: bool,
    /// Media seconds consumed per output second, mirroring the speed the
    /// time-stretch slot runs the source at. The mix output is on the output
    /// clock; `duration`, the near-end triggers and every position consumer
    /// are on the media clock, so served output frames only become a position
    /// once scaled by this.
    pub(super) playback_rate: f32,
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
    /// Cumulative *media* frames this track has served into the mix output:
    /// output frames scaled by [`Self::playback_rate`].
    ///
    /// Used as the source of truth for near-end trigger position so the
    /// trigger reflects what has been rendered to the audio output, not the
    /// decoder's pre-buffered position (which can be ~200 ms ahead of the
    /// mixer thanks to `PlayerResource`'s scratch buffer).
    pub(super) served_media_frames: f64,
    pub(super) sample_rate: u32,
}

#[bon]
impl PlayerTrack {
    /// Create a new track in the `Preloading` state.
    ///
    /// The `MixDSP` starts at `FULLY_WET` (silent) so that an explicit
    /// `fade_in()` or `play()` is required to produce audio.
    #[builder]
    #[must_use]
    pub fn new(
        #[builder(finish_fn)] resource: Box<PlayerResource>,
        sample_rate: NonZeroU32,
        item_id: Option<Arc<str>>,
        #[builder(default)] fade_duration: f32,
        #[builder(default = FadeCurve::SquareRoot)] fade_curve: FadeCurve,
        #[builder(default = 1.0)] playback_rate: f32,
        #[builder(default)] prefetch_duration: f32,
    ) -> Self {
        let observed_duration = resource.duration();
        let track = Self {
            resource,
            item_id,
            playback_rate,
            observed_duration,
            state: TrackState::Preloading,
            start: TrackStart::default(),
            start_offset: None,
            state_dirty: false,
            triggers: TrackTriggers::default(),
            fade: TrackFade::new(fade_duration, fade_curve, sample_rate),
            prefetch_duration: prefetch_duration.max(0.0),
            sample_rate: sample_rate.get(),
            served_media_frames: 0.0,
            ended_at_eof: false,
        };
        track.update_service_class(TrackState::Preloading);
        track
    }

    delegate::delegate! {
        to self.resource {
            /// Cached span in seconds: how much of the source is on disk.
            #[must_use]
            pub fn cached_span(&self) -> f64;
            /// Decoded-ahead frontier in seconds.
            #[must_use]
            pub fn decoded_frontier(&self) -> f64;
            /// Current visible (post-gapless-trim) duration in seconds.
            #[must_use]
            #[expr(observed_duration(self.observed_duration, $))]
            pub fn duration(&self) -> f64;
            /// Control-plane handle used to begin this track's seeks off the audio thread.
            #[must_use]
            pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>>;
            /// Source identifier.
            #[must_use]
            pub fn src(&self) -> &Arc<str>;
            /// Propagate the host sample rate to the owned resource.
            pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
        }
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

    /// Plan when this track becomes audible.
    pub fn set_start(&mut self, start: TrackStart) {
        self.start = start;
    }

    /// Offset this block's first read must begin at, consumed on read.
    pub(crate) fn take_start_offset(&mut self) -> Option<usize> {
        self.start_offset.take()
    }

    /// Promote a beat-anchored track at `offset` inside the current block.
    pub fn play_at_offset(&mut self, offset: usize) {
        self.start_offset = Some(offset);
        self.play();
    }

    /// Instantly start playing at full volume.
    pub fn play(&mut self) {
        self.set_state(TrackState::Playing);
        self.fade.play();
        self.triggers.reset();
        self.ended_at_eof = false;
    }

    /// Current media position in seconds.
    ///
    /// Tracks `served_media_frames / sample_rate` — i.e. what has actually
    /// been mixed into the output, on the media clock — so the value matches
    /// the trigger evaluator and `duration` instead of the decoder's
    /// pre-buffered position.
    #[must_use]
    pub fn position(&self) -> f64 {
        let sample_rate = self.sample_rate.max(1);
        self.served_media_frames / f64::from(sample_rate)
    }

    /// Re-base the track on a seek the control thread already begun.
    ///
    /// Lock-free, so it is safe from the audio callback: it drops what the feeder buffered and
    /// moves the media clock, while the begin half of the seek — the epoch, the event, the wakes —
    /// happened on the control thread through [`PlayerResource::seek_handle`].
    pub fn seek(&mut self, seconds: f64) {
        self.resource.reset_for_seek();
        let frames = seek_frame_index(seconds, self.sample_rate, self.observed_duration);
        self.served_media_frames = AsPrimitive::as_(frames);
        self.triggers.reset();
        self.ended_at_eof = false;
    }

    /// Set the media seconds consumed per output second for this track,
    /// pushing the same speed into the resource's stretch slot.
    pub fn set_playback_rate(&mut self, rate: f32) {
        self.playback_rate = rate;
        self.resource.set_playback_rate(rate);
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
