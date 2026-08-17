use std::{collections::VecDeque, num::NonZeroU32, sync::atomic::Ordering};

use bon::Builder;
use firewheel::{
    StreamInfo,
    dsp::fade::FadeCurve,
    event::ProcEvents,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_audio::SessionFrame;
use kithara_bufpool::PcmPool;
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapCons, HeapProd, traits::Producer};
use smallvec::SmallVec;

use super::track::PlayerTrack;
use crate::{
    bridge::{
        NodeInputs, PlaybackPresentationPublisher, PlaybackShared, PlayerCmd, PlayerNotification,
        TrackState, TrackTransition,
    },
    rt::{RenderOutcome, RenderPass, RenderTargets, TrackSlot, TrackSlots},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CrossfadeCurve {
    #[default]
    EqualPower,
}

const fn map_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::EqualPower => FadeCurve::SquareRoot,
    }
}

#[derive(Clone, Debug, Builder)]
pub(crate) struct CrossfadeSettings {
    #[builder(default)]
    pub(crate) curve: CrossfadeCurve,
    #[builder(default = 1.0)]
    pub(crate) duration: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl CrossfadeSettings {
    pub(crate) const fn fade_curve(&self) -> FadeCurve {
        map_curve(self.curve)
    }
}

/// The realtime audio processor for the player node.
///
/// Owns the loaded tracks, handles transitions, and renders mixed stereo audio into the Firewheel
/// output buffers.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerNodeProcessor {
    #[field(get, deref = false)]
    pub(super) playback: Arc<PlaybackShared>,
    pub(super) tracks: TrackSlots<{ Self::MAX_TRACKS }>,
    pub(super) crossfade: CrossfadeSettings,
    pub(super) cmd_rx: HeapCons<PlayerCmd>,
    pub(super) notif_tx: HeapProd<PlayerNotification>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) render: RenderPass,
    pub(super) tracks_transitions: VecDeque<TrackTransition>,
    /// Media seconds consumed per output second, applied to every track the
    /// processor owns and seeded into every track it loads.
    pub(super) playback_rate: f32,
    pub(super) prefetch_duration: f32,
    trash_tx: HeapProd<PlayerTrack>,
    presentation: PlaybackPresentationPublisher,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy)]
pub struct StreamShape {
    pub max_block_frames: NonZeroU32,
    pub sample_rate: NonZeroU32,
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    pub(super) const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    pub const MAX_TRACKS: usize = 4;

    /// Create a new processor with the given command receiver and shared state.
    #[must_use]
    pub fn new(inputs: NodeInputs, shape: StreamShape, pool: &PcmPool) -> Self {
        Self {
            cmd_rx: inputs.cmd_rx,
            notif_tx: inputs.notif_tx,
            trash_tx: inputs.trash_tx,
            playback: inputs.playback,
            sample_rate: shape.sample_rate,
            render: RenderPass::new(pool, shape),
            crossfade: CrossfadeSettings::default(),
            prefetch_duration: 0.0,
            playback_rate: 1.0,
            tracks: TrackSlots::default(),
            tracks_transitions: VecDeque::with_capacity(Self::MAX_TRACKS),
            presentation: inputs.presentation,
        }
    }

    /// Clean up finished tracks, dropping `playing` once none is audible.
    pub fn cleanup_finished_tracks(&mut self) {
        let finished: SmallVec<[(TrackSlot, bool); Self::MAX_TRACKS]> = self
            .tracks
            .iter()
            .filter(|(_, track)| track.state() == TrackState::Finished)
            .map(|(slot, track)| (slot, track.ended_at_eof()))
            .collect();

        // Superpowered-style end-of-queue resume: if removing the finished tracks would empty the
        // slot set (the queue has played out) and one of them reached *natural* EOF, keep that
        // single track resident (warm) so a later in-range seek can revive it (`apply_seek`). It is
        // reclaimed by `evict_tracks_if_needed` (Finished evicts first) when the next track loads.
        // Tracks that finished via `stop()` or a faded-out crossfade (not `ended_at_eof`) are
        // discarded as usual.
        let retain: Option<TrackSlot> = if finished.len() == self.tracks.len() {
            finished
                .iter()
                .find_map(|(slot, ended_at_eof)| ended_at_eof.then_some(*slot))
        } else {
            None
        };

        for (slot, _) in finished.iter().filter(|(slot, _)| Some(*slot) != retain) {
            if let Some(track) = self.tracks.remove_at(*slot) {
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src })
                    .ok();
            }
        }

        // The retained track is `Finished`, so `render_audio` skips it and `is_playing()` stays
        // false until a seek revives it.
        if self.tracks.len() == 0 || retain.is_some() {
            self.playback.playing.store(false, Ordering::SeqCst);
        }
    }

    pub(super) fn discard_track(&mut self, track: PlayerTrack) {
        if self.trash_tx.try_push(track).is_err() {
            self.playback.metrics().record_trash_overflow();
        }
    }

    pub(super) fn evict_tracks_if_needed(&mut self) {
        while self.tracks.is_full() {
            let Some((slot, state)) = self
                .tracks
                .iter()
                .min_by_key(|(_, track)| super::render::eviction_priority(track.state()))
                .map(|(slot, track)| (slot, track.state()))
            else {
                break;
            };

            if state == TrackState::Playing {
                self.playback.metrics().record_evicted_playing();
            }
            if let Some(track) = self.tracks.remove_at(slot) {
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src })
                    .ok();
            }
        }
    }

    pub fn render_audio(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        let outcome = self.render_block(buffers, frames, is_playing, None);
        self.presentation.publish(None);
        (outcome.playback_started, outcome.leading_position_duration)
    }

    fn render_block(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
        block_frame: Option<SessionFrame>,
    ) -> RenderOutcome {
        self.render.render_audio(
            RenderTargets {
                tracks: &mut self.tracks,
                notification_tx: &mut self.notif_tx,
                metrics: self.playback.metrics(),
            },
            buffers,
            frames,
            is_playing,
            block_frame,
        )
    }

    fn invalidate_tracks_presentation(&mut self) {
        self.tracks
            .iter_mut()
            .for_each(|(_, track)| track.invalidate_presentation());
    }

    fn set_tracks_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.tracks
            .iter()
            .for_each(|(_, track)| track.set_host_sample_rate(sample_rate));
    }

    pub(super) fn unload_track(&mut self, src: &str) {
        if let Some(track) = self.tracks.remove(src) {
            self.retire(track);
        }
    }

    pub(super) fn unload_slot(&mut self, slot: TrackSlot) {
        if let Some(track) = self.tracks.remove_at(slot) {
            self.retire(track);
        }
    }

    fn retire(&mut self, track: PlayerTrack) {
        let src = Arc::clone(track.src());
        self.discard_track(track);
        self.notif_tx
            .try_push(PlayerNotification::Unloaded { src })
            .ok();
    }

    fn update_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        let rate_changed = self.sample_rate != sample_rate;
        self.sample_rate = sample_rate;
        self.playback
            .sample_rate
            .store(sample_rate.get(), Ordering::Relaxed);
        if rate_changed {
            self.set_tracks_host_sample_rate(sample_rate);
            self.render.update_sample_rate(sample_rate);
        }
    }

    /// Update `playback.position` / `playback.duration` from the
    /// leading track's last [`TrackReadOutcome`].
    ///
    /// `render_audio` captures the snapshot directly out of the outcome
    /// returned by `PlayerTrack::read`.
    /// Falls back to `track.position()` / `track.duration()` only when no
    /// leading track produced an outcome this cycle (cold start before
    /// the first render block, or every active track was a non-leading
    /// fade-in).
    fn update_position_duration(&self, leading_outcome: Option<(f64, f64)>) {
        // Both windows come from the leading track's lock-free snapshots: the
        // decoded frontier (always `>=` position) and the cached span the
        // download side published. The queue view unions them into the
        // buffered window the FFI polls for loaded ranges.
        for (_, track) in self.tracks.iter() {
            if track.state().is_leading() {
                self.playback
                    .frontier
                    .store(track.decoded_frontier(), Ordering::Relaxed);
                self.playback
                    .cached
                    .store(track.cached_span(), Ordering::Relaxed);
                break;
            }
        }

        if let Some((position, duration)) = leading_outcome {
            self.playback.position.store(position, Ordering::Relaxed);
            self.playback.duration.store(duration, Ordering::Relaxed);
            return;
        }

        for (_, track) in self.tracks.iter() {
            if track.state().is_leading() {
                self.playback
                    .position
                    .store(track.position(), Ordering::Relaxed);
                self.playback
                    .duration
                    .store(track.duration(), Ordering::Relaxed);
                break;
            }
        }
    }

    delegate::delegate! {
        to self.tracks {
            /// Look up a track by its source identifier.
            #[must_use]
            #[call(get)]
            pub fn track(&self, src: &Arc<str>) -> Option<&PlayerTrack>;
            /// Number of tracks currently held in the processor arena.
            #[must_use]
            #[call(len)]
            pub fn track_count(&self) -> usize;
            /// Look up a track by its source identifier (mutable).
            #[call(get_mut)]
            pub fn track_mut(&mut self, src: &Arc<str>) -> Option<&mut PlayerTrack>;
        }
    }
}

impl AudioNodeProcessor for PlayerNodeProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.invalidate_tracks_presentation();
        self.presentation.publish(None);
        self.update_host_sample_rate(stream_info.sample_rate);
        self.render.resize(stream_info.max_block_frames.get().as_());
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        mut buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        self.playback.process_count.fetch_add(1, Ordering::Relaxed);

        self.drain_commands();

        self.cleanup_finished_tracks();

        let is_playing = self.playback.playing.load(Ordering::SeqCst);

        let block_frame = Some(SessionFrame::new(info.clock_samples.0));
        let outcome = self.render_block(&mut buffers, info.frames, is_playing, block_frame);

        self.update_position_duration(outcome.leading_position_duration);
        self.presentation.publish(outcome.presentation);

        if outcome.playback_started {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        num::{NonZeroU32, NonZeroUsize},
    };

    use firewheel::{
        clock::InstantSamples,
        dsp::{buffer::ChannelBuffer, declick::DeclickValues},
        event::{NodeEvent, ProcEvents, ProcEventsIndex, ScheduledEventEntry},
        log::{RealtimeLoggerConfig, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{
            AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
            StreamStatus,
        },
    };
    use kithara_audio::{
        PcmControl, PcmRead, PcmSession, PendingReason, PresentationAdvance, PresentationPoint,
        ReadOutcome, SeekOutcome, SessionFrame,
    };
    use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
    use kithara_events::EventBus;
    use kithara_platform::{sync::Arc, time::Duration};

    use super::*;
    use crate::{
        bridge::{SharedEq, slot_channels},
        resource::Resource,
        rt::track::PlayerResource,
    };

    struct Consts;

    impl Consts {
        const BLOCK_FRAMES: usize = 512;
        const SAMPLE_RATE: u32 = 48_000;
    }

    #[derive(Clone, Copy)]
    struct ReadStep {
        frames: Option<NonZeroUsize>,
        point: PresentationPoint,
        advance: Option<PresentationAdvance>,
        expected_request: usize,
    }

    impl ReadStep {
        fn frames(
            frames: usize,
            point: PresentationPoint,
            advance: Option<PresentationAdvance>,
            expected_request: usize,
        ) -> Self {
            Self {
                frames: NonZeroUsize::new(frames),
                point,
                advance,
                expected_request,
            }
        }

        const fn pending(point: PresentationPoint, expected_request: usize) -> Self {
            Self {
                frames: None,
                point,
                advance: None,
                expected_request,
            }
        }
    }

    struct PresentationReader {
        steps: VecDeque<ReadStep>,
        point: Option<PresentationPoint>,
        advance: Option<PresentationAdvance>,
        bus: EventBus,
        metadata: TrackMetadata,
        spec: PcmSpec,
    }

    impl PresentationReader {
        fn new(steps: impl IntoIterator<Item = ReadStep>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                point: None,
                advance: None,
                bus: EventBus::default(),
                metadata: TrackMetadata::default(),
                spec: PcmSpec::new(2, sample_rate()),
            }
        }
    }

    impl PcmRead for PresentationReader {
        fn presentation_point(&self) -> Option<PresentationPoint> {
            self.point
        }

        fn take_presentation_advance(&mut self) -> Option<PresentationAdvance> {
            self.advance.take()
        }

        fn position(&self) -> Duration {
            Duration::ZERO
        }

        fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
            Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: Duration::ZERO,
            })
        }

        fn read_planar<'a>(
            &mut self,
            output: &'a mut [&'a mut [f32]],
        ) -> Result<ReadOutcome, DecodeError> {
            let Some(step) = self.steps.pop_front() else {
                return Ok(ReadOutcome::Pending {
                    reason: PendingReason::Buffering,
                    position: Duration::ZERO,
                });
            };
            assert_eq!(output[0].len(), step.expected_request);
            self.point = Some(step.point);
            self.advance = step.advance;
            let Some(count) = step.frames else {
                return Ok(ReadOutcome::Pending {
                    reason: PendingReason::Buffering,
                    position: Duration::ZERO,
                });
            };
            let frames = count.get();
            for channel in output.iter_mut() {
                channel[..frames].fill(1.0);
            }
            Ok(ReadOutcome::Frames {
                count,
                position: Duration::ZERO,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    impl PcmSession for PresentationReader {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }
    }

    impl PcmControl for PresentationReader {
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate is non-zero")
    }

    fn point(source_frame: u64, generation: u64, output_end: u64) -> PresentationPoint {
        PresentationPoint::new(7, source_frame, generation, output_end, sample_rate())
    }

    fn processor_with_track(
        reader: PresentationReader,
    ) -> (PlayerNodeProcessor, Arc<PlaybackShared>) {
        let pool = PcmPool::default();
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let playback = Arc::clone(&control.playback);
        let src: Arc<str> = Arc::from("presentation-fixture");
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        let resource = Box::new(PlayerResource::new(resource, src, &pool));
        let mut track = PlayerTrack::builder()
            .sample_rate(sample_rate())
            .build(resource);
        track.play();

        let mut processor = PlayerNodeProcessor::new(
            inputs,
            StreamShape {
                max_block_frames: NonZeroU32::new(
                    u32::try_from(Consts::BLOCK_FRAMES).expect("block size fits u32"),
                )
                .expect("block size is non-zero"),
                sample_rate: sample_rate(),
            },
            &pool,
        );
        assert!(processor.tracks.insert(track).is_none());
        playback.playing.store(true, Ordering::SeqCst);
        (processor, playback)
    }

    fn process_block(processor: &mut PlayerNodeProcessor, clock_samples: i64) {
        let info = ProcInfo {
            sample_rate: sample_rate(),
            frames: Consts::BLOCK_FRAMES,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            prev_output_was_silent: true,
            sample_rate_recip: f64::from(Consts::SAMPLE_RATE).recip(),
            clock_samples: InstantSamples(clock_samples),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        let inputs: [&[f32]; 0] = [];
        let mut output_storage = [
            vec![0.0; Consts::BLOCK_FRAMES],
            vec![0.0; Consts::BLOCK_FRAMES],
        ];
        let mut outputs: Vec<&mut [f32]> =
            output_storage.iter_mut().map(Vec::as_mut_slice).collect();
        let buffers = ProcBuffers {
            inputs: &inputs,
            outputs: &mut outputs,
        };
        let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
        let mut extra = ProcExtra {
            logger,
            store: ProcStore::with_capacity(0),
            scratch_buffers: ChannelBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(Consts::BLOCK_FRAMES),
            declick_values: DeclickValues::new(
                NonZeroU32::new(16).expect("declick duration is non-zero"),
            ),
        };
        let mut immediate: [Option<NodeEvent>; 0] = [];
        let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
        let mut indices: Vec<ProcEventsIndex> = Vec::new();
        let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);

        let _ = processor.process(&info, buffers, &mut events, &mut extra);
    }

    #[kithara::test]
    fn presentation_boundary_uses_consumed_offset_not_requested_range_end() {
        let expected = point(9_600, 3, 192);
        let reader = PresentationReader::new([
            ReadStep::frames(128, expected, None, 512),
            ReadStep::frames(
                384,
                expected,
                Some(PresentationAdvance::new(expected, 64)),
                384,
            ),
        ]);
        let (mut processor, playback) = processor_with_track(reader);

        process_block(&mut processor, 10_000);

        let cursor = playback
            .snapshot()
            .presentation()
            .expect("consumed producer boundary must be published");
        assert_eq!(cursor.point(), expected);
        assert_eq!(cursor.session_frame(), SessionFrame::new(10_192));
        assert_ne!(cursor.session_frame(), SessionFrame::new(10_512));
    }

    #[kithara::test]
    fn no_advance_before_the_final_boundary_is_consumed() {
        let ahead = point(9_600, 3, 512);
        let reader = PresentationReader::new([ReadStep::frames(512, ahead, None, 512)]);
        let (mut processor, playback) = processor_with_track(reader);

        process_block(&mut processor, 10_000);

        assert_eq!(playback.snapshot().presentation(), None);
    }

    #[kithara::test]
    fn underrun_invalidates_the_previous_presentation_mapping() {
        let anchored = point(9_600, 3, 192);
        let reader = PresentationReader::new([
            ReadStep::frames(
                512,
                anchored,
                Some(PresentationAdvance::new(anchored, 192)),
                512,
            ),
            ReadStep::pending(anchored, 512),
        ]);
        let (mut processor, playback) = processor_with_track(reader);

        process_block(&mut processor, 10_000);
        assert!(playback.snapshot().presentation().is_some());

        process_block(&mut processor, 10_512);
        assert_eq!(playback.snapshot().presentation(), None);
    }
}
