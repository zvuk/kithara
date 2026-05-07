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
}

/// Per-track state in the processor arena.
///
/// Manages the `MixDSP` fade, track state, cached position/duration,
/// and notification logic for a single loaded track.
pub struct PlayerTrack {
    resource: Arc<Mutex<PlayerResource>>,
    state: TrackState,
    state_dirty: bool,
    notified_track_requested: bool,
    notified_prefetch_requested: bool,
    mix: MixDSP,
    fade_curve: FadeCurve,
    /// Current crossfade duration used for near-end trigger checks.
    fade_duration: f32,
    /// Lead time before EOF at which the prefetch trigger fires.
    ///
    /// Effective preload threshold is
    /// `max(prefetch_duration, fade_duration) + block_seconds`, so prefetch
    /// is at least as eager as the crossfade trigger and can be set
    /// independently to cover network/probe latency.
    prefetch_duration: f32,
    sample_rate: u32,
    /// Cumulative frames this track has actually served into the mix output.
    ///
    /// Used as the source of truth for near-end trigger position so the
    /// trigger reflects what has been rendered to the audio output, not the
    /// decoder's pre-buffered position (which can be ~200 ms ahead of the
    /// mixer thanks to `PlayerResource`'s scratch buffer).
    served_frames: u64,
    /// Last observed duration snapshot.
    ///
    /// Mirrors `PlayerResource::duration()` (post-gapless-trim, visible
    /// duration) captured under the resource lock.
    observed_duration: f64,
    item_id: Option<Arc<str>>,
    src: Arc<str>,
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
        let track = Self {
            resource,
            state: TrackState::Preloading,
            state_dirty: false,
            notified_track_requested: false,
            notified_prefetch_requested: false,
            mix: MixDSP::new(Mix::FULLY_WET, fade_curve, fade_conf, sample_rate),
            fade_curve,
            fade_duration,
            prefetch_duration: prefetch_duration.max(0.0),
            sample_rate: sample_rate.get(),
            served_frames: 0,
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
    pub fn read(
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
                    position: self.position(),
                    duration: self.observed_duration,
                    frames_until_eof: None,
                };
            };
            let (scratch_left, scratch_right) = scratch_bufs.split_at_mut(1);
            let mut scratch_window = [
                &mut scratch_left[0][range.clone()],
                &mut scratch_right[0][range.clone()],
            ];

            match guard.read(&mut scratch_window, 0..range.len()) {
                ReadOutcome::Full => {
                    let duration = guard.duration();
                    let frames_until_eof = guard.frames_until_eof();
                    drop(guard);
                    TrackReadOutcome::Full {
                        position: 0.0, // overwritten below from served_frames
                        duration,
                        frames_until_eof,
                    }
                }
                ReadOutcome::Partial(frames) => {
                    let duration = guard.duration();
                    drop(guard);
                    TrackReadOutcome::Partial { frames, duration }
                }
                ReadOutcome::Eof => TrackReadOutcome::Eof,
            }
        };

        match read_outcome {
            TrackReadOutcome::Full {
                duration,
                frames_until_eof,
                ..
            } => {
                self.advance_served_frames(range.len() as u64);
                self.observed_duration = duration;
                if let Some(remaining_frames) = frames_until_eof {
                    let sample_rate = self.sample_rate.max(1);
                    let remaining_f64: f64 = num_traits::cast::AsPrimitive::as_(remaining_frames);
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
                    frames_until_eof,
                }
            }
            TrackReadOutcome::Partial {
                frames, duration, ..
            } => {
                self.advance_served_frames(frames as u64);
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
        }
    }

    /// Add `frames` to the rolling served-frames counter so trigger checks
    /// reflect what has been mixed into the output, not what the decoder has
    /// pre-buffered.
    fn advance_served_frames(&mut self, frames: u64) {
        self.served_frames = self.served_frames.saturating_add(frames);
    }

    /// Handle natural EOF.
    fn handle_natural_end(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.state == TrackState::Finished {
            return;
        }
        // EOF backstop: emit a single handover trigger if the crossfade
        // window never fired earlier. Mark the prefetch channel as used
        // too so we never emit a stale preload event after EOF.
        self.notified_prefetch_requested = true;
        self.emit_handover_requested(notification_tx);
        self.set_state(TrackState::Finished);
        notification_tx
            .lock_sync()
            .try_push(PlayerNotification::PlaybackStopped {
                src: Arc::clone(&self.src),
                item_id: self.item_id.clone(),
                reason: TrackPlaybackStopReason::Eof,
            })
            .ok();
        self.state_dirty = false;
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
        use num_traits::cast::AsPrimitive;
        let block_frames_f32: f32 = block_frames.as_();
        let sr_f32: f32 = self.sample_rate.as_();
        let block_seconds = block_frames_f32 / sr_f32;
        let fade_threshold = self.fade_duration + block_seconds;
        let prefetch_threshold = self.prefetch_duration.max(self.fade_duration) + block_seconds;
        // Both triggers are emitted independently. `PrefetchRequested` is a
        // contract precondition for `HandoverRequested`: consumers arm the
        // next slot on prefetch and commit on handover. Suppressing prefetch
        // when the windows coincide leaves the consumer without an armed
        // slot and the handover becomes a no-op. Per-cycle dedup is enforced
        // inside `emit_*` via `notified_*` flags.

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

    /// Emit the prefetch (preload-only) trigger once per playback cycle.
    fn emit_track_requested(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.notified_prefetch_requested {
            return;
        }

        if notification_tx
            .lock_sync()
            .try_push(PlayerNotification::Requested)
            .is_ok()
        {
            self.notified_prefetch_requested = true;
        }
    }

    /// Emit the crossfade-aligned handover trigger once per playback cycle.
    fn emit_handover_requested(&mut self, notification_tx: &Mutex<HeapProd<PlayerNotification>>) {
        if self.notified_track_requested {
            return;
        }

        if notification_tx
            .lock_sync()
            .try_push(PlayerNotification::HandoverRequested)
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
            TrackState::Preloading => PlayerNotification::Loaded {
                src: Arc::clone(&self.src),
            },
            TrackState::FadingIn => PlayerNotification::FadingIn,
            TrackState::FadingOut => PlayerNotification::FadingOut,
            TrackState::Playing => PlayerNotification::PlaybackStarted,
            TrackState::Finished => PlayerNotification::PlaybackStopped {
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
            TrackState::Finished => ServiceClass::Idle,
        };
        if let Ok(resource) = self.resource.try_lock() {
            resource.set_service_class(class);
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

    /// Instantly start playing at full volume.
    pub fn play(&mut self) {
        self.set_state(TrackState::Playing);
        self.mix.set_mix(Mix::FULLY_DRY, self.fade_curve);
        self.mix.reset_to_target();
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
    }

    /// Instantly stop (silent, finished state).
    pub fn stop(&mut self) {
        self.set_state(TrackState::Finished);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
        self.mix.reset_to_target();
    }

    /// Seek the underlying resource and re-sync the served-frame counter
    /// so trigger thresholds reflect the new playback origin.
    pub fn seek(&mut self, seconds: f64) {
        if let Ok(mut resource) = self.resource.try_lock() {
            resource.seek(seconds);
        }
        let sample_rate = self.sample_rate.max(1);
        // Saturating clamp: negative inputs already pinned to 0 above; the
        // overflow path (target so far in the future the result exceeds u64)
        // hits `u64::MAX` and the timeline reports terminal-end semantics.
        let frames =
            num_traits::cast::ToPrimitive::to_u64(&(seconds.max(0.0) * f64::from(sample_rate)))
                .unwrap_or(u64::MAX);
        self.served_frames = frames;
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
    }

    /// Current position in seconds.
    ///
    /// Tracks `served_frames / sample_rate` — i.e. what has actually been
    /// mixed into the output — so the value matches the trigger evaluator
    /// instead of the decoder's pre-buffered position.
    #[must_use]
    pub fn position(&self) -> f64 {
        let sample_rate = self.sample_rate.max(1);
        let served_f64: f64 = num_traits::cast::AsPrimitive::as_(self.served_frames);
        served_f64 / f64::from(sample_rate)
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

    /// Current track state.
    #[must_use]
    pub fn state(&self) -> TrackState {
        self.state
    }

    /// Source identifier.
    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        &self.src
    }

    /// Reference to the underlying shared resource.
    #[must_use]
    pub fn resource(&self) -> &Arc<Mutex<PlayerResource>> {
        &self.resource
    }

    /// Re-create the `MixDSP` with a new fade duration.
    pub fn update_fade_duration(&mut self, fade_duration: f32, sample_rate: NonZeroU32) {
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
        self.sample_rate = sample_rate.get();
    }

    /// Update the prefetch lead time used for the preload trigger.
    pub fn set_prefetch_duration(&mut self, prefetch_duration: f32) {
        self.prefetch_duration = prefetch_duration.max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use kithara_audio::PcmReader;
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
        PcmSpec {
            channels: 2,
            sample_rate: 44100,
        }
    }

    fn make_track_from_resource(
        resource: Resource,
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    ) -> PlayerTrack {
        let player_resource = PlayerResource::new(
            resource,
            Arc::clone(&src),
            &kithara_bufpool::PcmPool::default(),
        );
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

    /// PcmReader fixture that reports a 10s duration but actually serves
    /// a much shorter buffer. Used to verify that the gapless trigger
    /// logic relies on observed-EOF, not on a possibly-stale duration.
    struct MisreportedDurationReader {
        bus: EventBus,
        metadata: TrackMetadata,
        remaining_frames: usize,
        position_frames: usize,
        spec: PcmSpec,
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

        fn position(&self) -> Duration {
            Duration::from_secs_f64(self.position_frames as f64 / f64::from(self.spec.sample_rate))
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(10))
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
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
