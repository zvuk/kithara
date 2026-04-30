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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "position/duration fields are inspected only by tests"
    )
)]
pub(crate) enum TrackReadOutcome {
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
pub(crate) struct PlayerTrack {
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
    pub(crate) fn new(
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
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "buffered frame count precision is sufficient for duration snapshots"
                    )]
                    let observed_eof =
                        self.position() + remaining_frames as f64 / f64::from(sample_rate);
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
            .try_push(PlayerNotification::TrackPlaybackStopped {
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
        #[expect(
            clippy::cast_precision_loss,
            reason = "block duration precision is sufficient for near-end threshold checks"
        )]
        let block_seconds = block_frames as f32 / self.sample_rate as f32;
        let fade_threshold = self.fade_duration + block_seconds;
        let prefetch_threshold = self.prefetch_duration.max(self.fade_duration) + block_seconds;
        // Both triggers are emitted independently. `PrefetchRequested` is a
        // contract precondition for `HandoverRequested`: consumers arm the
        // next slot on prefetch and commit on handover. Suppressing prefetch
        // when the windows coincide leaves the consumer without an armed
        // slot and the handover becomes a no-op. Per-cycle dedup is enforced
        // inside `emit_*` via `notified_*` flags.

        if let Some(frames_until_eof) = frames_until_eof {
            #[expect(
                clippy::cast_precision_loss,
                reason = "buffered frame count precision is sufficient for near-end threshold checks"
            )]
            let remaining = frames_until_eof as f32 / self.sample_rate as f32;
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

        #[expect(
            clippy::cast_possible_truncation,
            reason = "position/duration in seconds; f32 precision is sufficient for threshold checks"
        )]
        let pos = self.position() as f32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "position/duration in seconds; f32 precision is sufficient for threshold checks"
        )]
        let dur = duration as f32;

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
            .try_push(PlayerNotification::TrackRequested(Arc::clone(&self.src)))
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
            .try_push(PlayerNotification::TrackHandoverRequested(Arc::clone(
                &self.src,
            )))
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
        self.notified_prefetch_requested = false;
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
        self.notified_prefetch_requested = false;
    }

    /// Instantly stop (silent, finished state).
    pub(crate) fn stop(&mut self) {
        self.set_state(TrackState::Finished);
        self.mix.set_mix(Mix::FULLY_WET, self.fade_curve);
        self.mix.reset_to_target();
    }

    /// Seek the underlying resource and re-sync the served-frame counter
    /// so trigger thresholds reflect the new playback origin.
    pub(crate) fn seek(&mut self, seconds: f64) {
        if let Ok(mut resource) = self.resource.try_lock() {
            resource.seek(seconds);
        }
        let sample_rate = self.sample_rate.max(1);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "negative seek targets are clamped to zero by Resource::seek"
        )]
        let frames = (seconds.max(0.0) * f64::from(sample_rate)) as u64;
        self.served_frames = frames;
        self.notified_track_requested = false;
        self.notified_prefetch_requested = false;
    }

    /// Current position in seconds.
    ///
    /// Tracks `served_frames / sample_rate` — i.e. what has actually been
    /// mixed into the output — so the value matches the trigger evaluator
    /// instead of the decoder's pre-buffered position.
    pub(crate) fn position(&self) -> f64 {
        let sample_rate = self.sample_rate.max(1);
        #[expect(
            clippy::cast_precision_loss,
            reason = "served-frame count precision is sufficient for position snapshots"
        )]
        {
            self.served_frames as f64 / f64::from(sample_rate)
        }
    }

    /// Current visible (post-gapless-trim) duration in seconds.
    ///
    /// Mirrors `PlayerResource::duration()` captured under the resource
    /// lock during the last `read()`. Falls back to a fresh `try_lock`
    /// when invoked before any `read()` (e.g. immediately after `seek`).
    pub(crate) fn duration(&self) -> f64 {
        if self.observed_duration > 0.0 {
            return self.observed_duration;
        }
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
        self.sample_rate = sample_rate.get();
    }

    /// Update the fade curve used for future fade operations.
    pub(crate) fn set_fade_curve(&mut self, curve: FadeCurve) {
        self.fade_curve = curve;
    }

    /// Update the prefetch lead time used for the preload trigger.
    pub(crate) fn set_prefetch_duration(&mut self, prefetch_duration: f32) {
        self.prefetch_duration = prefetch_duration.max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use kithara_audio::{PcmReader, mock::TestPcmReader};
    use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
    use kithara_events::EventBus;
    use kithara_platform::time::Duration;
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
        make_track_from_resource(resource, src, item_id)
    }

    fn make_track_from_resource(
        resource: Resource,
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    ) -> PlayerTrack {
        let player_resource =
            PlayerResource::new(resource, Arc::clone(&src), kithara_bufpool::pcm_pool());
        let arc_resource = Arc::new(Mutex::new(player_resource));
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
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

    fn make_track() -> PlayerTrack {
        make_track_with(60.0, None)
    }

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
        fn read(&mut self, buf: &mut [f32]) -> usize {
            let channels = usize::from(self.spec.channels);
            let frames = (buf.len() / channels).min(self.remaining_frames);
            let samples = frames.saturating_mul(channels);
            buf[..samples].fill(0.5);
            self.remaining_frames -= frames;
            self.position_frames += frames;
            samples
        }

        fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize {
            let frames = output
                .iter()
                .map(|channel| channel.len())
                .min()
                .unwrap_or(0)
                .min(self.remaining_frames);
            for channel in output.iter_mut() {
                channel[..frames].fill(0.5);
            }
            self.remaining_frames -= frames;
            self.position_frames += frames;
            frames
        }

        fn seek(&mut self, _position: Duration) -> DecodeResult<()> {
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn is_eof(&self) -> bool {
            self.remaining_frames == 0
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

    fn drain_eof_stop_notifications(
        rx: &mut impl Consumer<Item = PlayerNotification>,
        saw_partial: bool,
    ) -> usize {
        let mut eof_stop_count = 0;

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

        eof_stop_count
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
    async fn track_seek_position_is_derived_from_served_frames() {
        let mut track = make_track();
        let seconds = 9.791_337;
        track.seek(seconds);

        let sample_rate = 44_100.0;
        let expected = (seconds * sample_rate).floor() / sample_rate;

        assert!((track.position() - expected).abs() < f64::EPSILON);
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
                duration,
                ..
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
                TrackReadOutcome::Full { .. } => {}
            }

            eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);
        }

        eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);

        assert!(saw_partial, "expected a Partial outcome before EOF");
        assert!(saw_eof_after_partial, "expected EOF after Partial");
        assert_eq!(
            eof_stop_count, 1,
            "expected exactly one EOF stop notification"
        );
    }

    #[kithara::test(tokio)]
    async fn handover_emits_once_when_position_crosses_fade_threshold() {
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

        let mut handover_count = 0;
        let mut saw_eof_stop = false;

        for _ in 0..4 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
            for notification in collect_notifications(&mut rx) {
                match notification {
                    PlayerNotification::TrackHandoverRequested(src)
                        if src.as_ref() == "test.mp3" =>
                    {
                        handover_count += 1;
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

            if handover_count > 0 {
                break;
            }
        }

        assert_eq!(handover_count, 1);
        assert!(
            !saw_eof_stop,
            "threshold-triggered handover should precede EOF"
        );

        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
        let notifications = collect_notifications(&mut rx);
        assert!(
            notifications.iter().all(|notification| !matches!(
                notification,
                PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "test.mp3"
            )),
            "TrackHandoverRequested must not be emitted twice in one playback cycle"
        );
    }

    #[kithara::test(tokio)]
    async fn handover_uses_buffered_eof_when_duration_is_overestimated() {
        let src = Arc::from("misreported.mp3");
        let resource =
            Resource::from_reader_with_src(MisreportedDurationReader::new(900), Arc::clone(&src));
        let mut track = make_track_from_resource(resource, src, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
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
                matches!(
                    notification,
                    PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "misreported.mp3"
                )
            }),
            "handover must be emitted before the EOF block when the resource has already observed EOF"
        );
        assert!(
            !notifications.iter().any(|notification| {
                matches!(
                    notification,
                    PlayerNotification::TrackPlaybackStopped {
                        reason: TrackPlaybackStopReason::Eof,
                        ..
                    }
                )
            }),
            "first full block must only request preload, not emit EOF"
        );
    }

    #[kithara::test(tokio)]
    async fn handover_backstops_eof_when_threshold_was_not_reached_earlier() {
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
        let handover_count = notifications
            .iter()
            .filter(|notification| {
                matches!(notification, PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "test.mp3")
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

        assert_eq!(handover_count, 1);
        assert_eq!(eof_stop_count, 1);
    }

    #[kithara::test(tokio)]
    async fn handover_is_not_duplicated_at_eof_after_early_trigger() {
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

        let mut handover_count = 0;
        let mut eof_stop_count = 0;

        for _ in 0..600 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

            for notification in collect_notifications(&mut rx) {
                match notification {
                    PlayerNotification::TrackHandoverRequested(src)
                        if src.as_ref() == "test.mp3" =>
                    {
                        handover_count += 1;
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

        assert_eq!(handover_count, 1);
        assert_eq!(eof_stop_count, 1);
    }

    #[kithara::test(tokio)]
    async fn prefetch_fires_before_handover_when_prefetch_exceeds_fade() {
        let mut track = make_track_with(10.0, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.0, sample_rate);
        track.set_prefetch_duration(2.0);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();
        // Position 8.5s leaves ~1.5s to EOF: inside the 2.0s prefetch window
        // but well outside the 0s crossfade window.
        track.seek(8.5);

        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

        let notifications = collect_notifications(&mut rx);
        let saw_prefetch = notifications.iter().any(|notification| {
            matches!(
                notification,
                PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3"
            )
        });
        let saw_handover = notifications.iter().any(|notification| {
            matches!(
                notification,
                PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "test.mp3"
            )
        });
        assert!(
            saw_prefetch,
            "TrackRequested (preload) must fire inside the prefetch lead window"
        );
        assert!(
            !saw_handover,
            "TrackHandoverRequested must not fire while pos < dur - fade"
        );
    }

    #[kithara::test(tokio)]
    async fn handover_fires_after_prefetch_when_position_reaches_fade_threshold() {
        let mut track = make_track_with(10.0, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.2, sample_rate);
        track.set_prefetch_duration(2.0);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(64).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();
        track.seek(8.5);

        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
        let after_prefetch = collect_notifications(&mut rx);
        assert!(after_prefetch.iter().any(|notification| matches!(
            notification,
            PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3"
        )));
        assert!(after_prefetch.iter().all(|notification| !matches!(
            notification,
            PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "test.mp3"
        )));

        // Move close to EOF so the crossfade-aligned trigger fires too.
        track.seek(9.79);

        let mut saw_handover = false;
        for _ in 0..4 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
            for notification in collect_notifications(&mut rx) {
                if matches!(
                    notification,
                    PlayerNotification::TrackHandoverRequested(src) if src.as_ref() == "test.mp3"
                ) {
                    saw_handover = true;
                }
            }
            if saw_handover {
                break;
            }
        }
        assert!(
            saw_handover,
            "handover trigger must fire near EOF after prefetch already fired"
        );
    }

    #[kithara::test(tokio)]
    async fn prefetch_fires_immediately_when_track_shorter_than_prefetch_duration() {
        let mut track = make_track_with(0.5, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.0, sample_rate);
        track.set_prefetch_duration(5.0);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();
        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);

        let notifications = collect_notifications(&mut rx);
        assert!(notifications.iter().any(|notification| matches!(
            notification,
            PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3"
        )));
    }

    #[kithara::test(tokio)]
    async fn prefetch_and_handover_both_fire_when_thresholds_coincide() {
        // Contract: PrefetchRequested always precedes HandoverRequested,
        // even when the windows coincide. Consumers (e.g. kithara-queue)
        // arm the next slot on prefetch and commit on handover; suppressing
        // prefetch leaves them without an armed slot and the handover is
        // a no-op.
        let mut track = make_track_with(10.0, None);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero sample rate");
        track.update_fade_duration(0.2, sample_rate);
        track.set_prefetch_duration(0.0);
        let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
        let notification_tx = Mutex::new(tx);
        let mut scratch_l = [0.0; 512];
        let mut scratch_r = [0.0; 512];
        let mut mix_l = [0.0; 512];
        let mut mix_r = [0.0; 512];
        let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
        let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

        track.play();
        // Mid-track: outside both fade and (zero) prefetch windows.
        track.seek(5.0);
        let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
        let mid = collect_notifications(&mut rx);
        assert!(mid.iter().all(|notification| !matches!(
            notification,
            PlayerNotification::TrackRequested(_) | PlayerNotification::TrackHandoverRequested(_)
        )));

        // Inside the fade window: both triggers must fire (each at most
        // once per playback cycle thanks to `notified_*` flags).
        track.seek(9.79);
        let mut prefetch_count = 0;
        let mut handover_count = 0;
        for _ in 0..4 {
            let _ = track.read(&mut scratch_bufs, &mut mix_bufs, 0..512, &notification_tx);
            for notification in collect_notifications(&mut rx) {
                match notification {
                    PlayerNotification::TrackRequested(src) if src.as_ref() == "test.mp3" => {
                        prefetch_count += 1;
                    }
                    PlayerNotification::TrackHandoverRequested(src)
                        if src.as_ref() == "test.mp3" =>
                    {
                        handover_count += 1;
                    }
                    _ => {}
                }
            }
            if handover_count > 0 && prefetch_count > 0 {
                break;
            }
        }
        assert_eq!(prefetch_count, 1, "prefetch must fire exactly once");
        assert_eq!(handover_count, 1, "handover must fire exactly once");
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
