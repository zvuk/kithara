use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::{AtomicU8, Ordering},
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
    AudioControl, AudioRead, AudioSession, ReadOutcome, RevisionFloorStatus, SeekOutcome,
    SourceSpan,
};
use kithara_bufpool::PoolRegion;
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_signal::AudioSpec;
use kithara_test_fixtures::play_fixtures::half;
use kithara_test_utils::kithara;
use kithara_warp::{
    GridSegment, RegionPlan, RegionPlanSlot, SessionEpoch, SessionFrame, Warp, WarpConfig, WarpMap,
    WarpMapRevision,
};
use ringbuf::traits::{Consumer, Producer};

use super::*;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, RtMetrics, SharedEq, TrackTransition, slot_channels},
    rt::{
        PlayerNodeProcessor, RenderPass, RenderTargets, StreamShape, TrackSlots,
        track::{PlayerResource, PlayerTrack},
    },
    test_pools::{TestPools, pools},
};

struct Consts;

impl Consts {
    const BLOCK_FRAMES: usize = 512;
    const SAMPLE_RATE: u32 = 44_100;
}

struct DropState;

impl DropState {
    const AFTER_CANCEL: u8 = 2;
    const BEFORE_CANCEL: u8 = 1;
    const NOT_DROPPED: u8 = 0;
}

struct DropProbe {
    state: Arc<AtomicU8>,
    cancel: CancelToken,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        let state = if self.cancel.is_cancelled() {
            DropState::AFTER_CANCEL
        } else {
            DropState::BEFORE_CANCEL
        };
        self.state.store(state, Ordering::SeqCst);
    }
}

struct EofReader {
    spec: AudioSpec,
    bus: EventBus,
    _drop_probe: Option<DropProbe>,
    meta: TrackMetadata,
    position_frames: usize,
    total_frames: usize,
    samples: Vec<f32>,
}

impl Default for EofReader {
    fn default() -> Self {
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            spec: AudioSpec::new(
                2,
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("static rate"),
            ),
            position_frames: 0,
            total_frames: 0,
            samples: Vec::new(),
            _drop_probe: None,
        }
    }
}

impl EofReader {
    fn eof(&self) -> ReadOutcome {
        ReadOutcome::Eof {
            position: self.position_duration(),
        }
    }

    fn position_duration(&self) -> Duration {
        let frames = u32::try_from(self.position_frames).expect("test frame count fits u32");
        Duration::from_secs_f64(f64::from(frames) / f64::from(Consts::SAMPLE_RATE))
    }

    fn take_frames(&mut self, capacity: usize) -> Option<NonZeroUsize> {
        let frames = capacity.min(self.total_frames - self.position_frames);
        self.position_frames += frames;
        NonZeroUsize::new(frames)
    }

    fn with_drop_probe(cancel: CancelToken, state: Arc<AtomicU8>) -> Self {
        Self {
            _drop_probe: Some(DropProbe { state, cancel }),
            ..Self::default()
        }
    }

    fn with_frames(samples: Vec<f32>) -> Self {
        Self {
            total_frames: samples.len() / 2,
            samples,
            ..Self::default()
        }
    }
}

impl AudioSession for EofReader {
    fn duration(&self) -> Option<Duration> {
        let frames = u32::try_from(self.total_frames).expect("test frame count fits u32");
        Some(Duration::from_secs_f64(
            f64::from(frames) / f64::from(Consts::SAMPLE_RATE),
        ))
    }
    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }
}

impl AudioRead for EofReader {
    fn position(&self) -> Duration {
        self.position_duration()
    }
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let Some(frames) = self.take_frames(buf.len() / 2) else {
            return Ok(self.eof());
        };
        let samples = frames.get() * 2;
        let end = self.position_frames * 2;
        buf[..samples].copy_from_slice(&self.samples[end - samples..end]);
        Ok(ReadOutcome::Frames {
            count: NonZeroUsize::new(samples).expect("non-zero stereo sample count"),
            position: self.position_duration(),
            source_span: SourceSpan::new(
                u64::try_from(self.position_frames - frames.get()).expect("fixture frame fits u64"),
                u64::try_from(self.position_frames).expect("fixture frame fits u64"),
                self.spec.sample_rate,
            ),
        })
    }
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let capacity = output.first().map_or(0, |channel| channel.len());
        let Some(frames) = self.take_frames(capacity) else {
            return Ok(self.eof());
        };
        let start = self.position_frames - frames.get();
        for (index, channel) in output.iter_mut().enumerate() {
            for (offset, sample) in channel[..frames.get()].iter_mut().enumerate() {
                *sample = self.samples[(start + offset) * 2 + index];
            }
        }
        Ok(ReadOutcome::Frames {
            count: frames,
            position: self.position_duration(),
            source_span: SourceSpan::new(
                u64::try_from(self.position_frames - frames.get()).expect("fixture frame fits u64"),
                u64::try_from(self.position_frames).expect("fixture frame fits u64"),
                self.spec.sample_rate,
            ),
        })
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for EofReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn present_seek(&mut self, _epoch: u64) -> kithara_audio::SeekPresentation {
        kithara_audio::SeekPresentation::Current
    }
}

struct RevisionReader {
    bus: EventBus,
    meta: TrackMetadata,
    position_frames: u64,
    replacement_frames: usize,
    revision: u64,
    sample: f32,
    spec: AudioSpec,
}

impl RevisionReader {
    fn new(replacement_frames: usize) -> Self {
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            position_frames: 0,
            replacement_frames,
            revision: 0,
            sample: 1.0,
            spec: AudioSpec::new(
                2,
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("static rate"),
            ),
        }
    }
}

impl AudioSession for RevisionReader {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }
}

impl AudioRead for RevisionReader {
    fn position(&self) -> Duration {
        Duration::from_secs_f64(
            self.position_frames as f64 / f64::from(self.spec.sample_rate.get()),
        )
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        unreachable!("fixture is read through the planar path")
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = output.first().map_or(0, |channel| channel.len());
        let Some(count) = NonZeroUsize::new(frames) else {
            return Ok(ReadOutcome::Pending {
                position: self.position(),
                reason: kithara_audio::PendingReason::Buffering,
            });
        };
        for channel in output.iter_mut() {
            channel[..frames].fill(self.sample);
        }
        let start = self.position_frames;
        self.position_frames = self
            .position_frames
            .checked_add(u64::try_from(frames).unwrap_or(u64::MAX))
            .unwrap_or(u64::MAX);
        Ok(ReadOutcome::Frames {
            count,
            position: self.position(),
            source_span: SourceSpan::new(start, self.position_frames, self.spec.sample_rate)
                .map(|span| span.with_render_revision(self.revision)),
        })
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for RevisionReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn set_render_revision_floor(
        &mut self,
        revision: u64,
        required_frames: NonZeroUsize,
        _presented_source: Option<kithara_audio::SourceEnd>,
    ) -> RevisionFloorStatus {
        if self.revision >= revision {
            return RevisionFloorStatus::Current;
        }
        if self.replacement_frames < required_frames.get() {
            return RevisionFloorStatus::WaitingForReplacement;
        }
        self.revision = revision;
        self.sample = 2.0;
        RevisionFloorStatus::Switched
    }

    fn present_seek(&mut self, _epoch: u64) -> kithara_audio::SeekPresentation {
        kithara_audio::SeekPresentation::Current
    }
}

fn scheduled_revision_resource(
    activation_output: i64,
    replacement_frames: usize,
) -> PlayerResource {
    let activation = WarpMap::identity(WarpMapRevision::first()).reanchor(
        1_000,
        SessionFrame::new(activation_output),
        kithara_warp::SessionBeat::default(),
    );
    let plan = RegionPlan::new(vec![GridSegment::new(0, 10_000, 2.0)])
        .expect("fixture plan")
        .with_activation(activation);
    let slot = Arc::new(RegionPlanSlot::default());
    slot.install(Some(Arc::new(plan)));
    let mut resource = Resource::from_reader(RevisionReader::new(replacement_frames), None);
    resource.region_plan = Some(slot);
    resource.activation_blend_frames =
        Some(NonZeroUsize::new(40).expect("fixture activation blend is non-zero"));
    PlayerResource::new(resource, Arc::from("revision"), &pools())
        .unwrap_or_else(|error| panic!("test player resource: {error}"))
}

#[kithara::test]
#[case::callback_start(100, 10, Some(0))]
#[case::callback_middle(105, 5, Some(5))]
#[case::callback_end(110, 10, None)]
#[case::partial_replacement_waits(105, 4, None)]
fn scheduled_revision_switches_only_at_its_host_frame_with_a_complete_suffix(
    #[case] activation_output: i64,
    #[case] replacement_frames: usize,
    #[case] switched_at: Option<usize>,
) {
    let mut resource = scheduled_revision_resource(activation_output, replacement_frames);
    let context = RenderContext::new(
        SessionFrame::new(100)..SessionFrame::new(110),
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("static rate"),
        None,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture context is valid");
    let mut left = [0.0; 10];
    let mut right = [0.0; 10];
    let mut output = [&mut left[..], &mut right[..]];

    let (outcome, source_frames) = resource.read_with_context(
        Some(&context),
        None,
        &mut output,
        0..10,
        &RtMetrics::default(),
    );

    assert_eq!(outcome, crate::rt::track::ReadOutcome::Full { frames: 10 });
    assert_eq!(source_frames, 10);
    let mut expected = [1.0; 10];
    if let Some(start) = switched_at {
        for (frame, sample) in expected[start..].iter_mut().enumerate() {
            let incoming_gain = frame as f32 / 40.0;
            *sample = 1.0f32.mul_add(1.0 - incoming_gain, 2.0 * incoming_gain);
        }
    }
    assert_eq!(left, expected);
    assert_eq!(right, expected);
    let expected_revision = switched_at.map_or(0, |_| u64::from(WarpMapRevision::first()));
    assert_eq!(
        resource
            .presentation_source_end(context.sample_rate())
            .map(|(_source, revision)| revision),
        Some(expected_revision)
    );
}

#[kithara::test]
fn armed_prepared_launch_renders_only_its_ready_suffix() {
    let (mut inputs, _control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        max_block_frames: NonZeroU32::new(10).expect("static block size"),
    };
    let mut pass = RenderPass::new(
        &pools(),
        shape,
        inputs.stretch,
        inputs.rate_smoothing,
        inputs.grid,
        crate::DEFAULT_GATE_SMOOTHING,
    );
    let item = TrackId::allocate();
    let mut track = PlayerTrack::builder()
        .sample_rate(shape.sample_rate)
        .item_id(item)
        .build(Box::new(scheduled_revision_resource(105, 5)));
    track.schedule_seek(
        7,
        crate::bridge::ScheduledSeekDisposition::PreparedLaunch(
            crate::bridge::PreparedLaunchIdentity {
                activation: SessionFrame::new(105),
                warp_map: WarpMapRevision::first(),
            },
        ),
        true,
    );
    track.play();
    let mut tracks = TrackSlots::default();
    assert!(tracks.insert(track).is_none());
    let context = RenderContext::new(
        SessionFrame::new(100)..SessionFrame::new(110),
        shape.sample_rate,
        None,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture context is valid");
    let mut left = [0.0; 10];
    let mut right = [0.0; 10];
    let input: [&[f32]; 0] = [];
    let mut output = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &input,
        outputs: &mut output,
    };

    let pre_activation = RenderContext::new(
        SessionFrame::new(90)..SessionFrame::new(100),
        shape.sample_rate,
        None,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture pre-activation context is valid");
    let (started, prepared_launch_started, _) = pass.render_audio(
        Some(&pre_activation),
        RenderTargets {
            notification_tx: &mut inputs.notif_tx,
            metrics: inputs.playback.metrics(),
            tracks: &mut tracks,
            seek_epoch: 7,
        },
        &mut buffers,
        10,
        false,
    );
    drop(buffers);
    assert!(!started);
    assert!(!prepared_launch_started);
    assert!(left.iter().all(|sample| *sample == 0.0));
    assert!(right.iter().all(|sample| *sample == 0.0));

    let mut output = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &input,
        outputs: &mut output,
    };
    let (mut activation_inputs, _) = slot_channels(SharedEq::new(0));
    let mut activation_pass = RenderPass::new(
        &pools(),
        shape,
        activation_inputs.stretch,
        activation_inputs.rate_smoothing,
        activation_inputs.grid,
        crate::DEFAULT_GATE_SMOOTHING,
    );

    let (started, prepared_launch_started, _) = activation_pass.render_audio(
        Some(&context),
        RenderTargets {
            notification_tx: &mut activation_inputs.notif_tx,
            metrics: activation_inputs.playback.metrics(),
            tracks: &mut tracks,
            seek_epoch: 7,
        },
        &mut buffers,
        10,
        false,
    );

    assert!(started);
    assert!(prepared_launch_started);
    assert!(left[..5].iter().all(|sample| *sample == 0.0));
    assert!(right[..5].iter().all(|sample| *sample == 0.0));
    assert!(
        left[5..].iter().all(|sample| *sample == 2.0),
        "left={left:?}"
    );
    assert!(right[5..].iter().all(|sample| *sample == 2.0));
}

#[kithara::test]
fn unarmed_prepared_launch_remains_silent_at_its_ready_activation() {
    let (mut inputs, _control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        max_block_frames: NonZeroU32::new(10).expect("static block size"),
    };
    let mut pass = RenderPass::new(
        &pools(),
        shape,
        inputs.stretch,
        inputs.rate_smoothing,
        inputs.grid,
        crate::DEFAULT_GATE_SMOOTHING,
    );
    let item = TrackId::allocate();
    let mut track = PlayerTrack::builder()
        .sample_rate(shape.sample_rate)
        .item_id(item)
        .build(Box::new(scheduled_revision_resource(105, 5)));
    track.schedule_seek(
        7,
        crate::bridge::ScheduledSeekDisposition::PreparedLaunch(
            crate::bridge::PreparedLaunchIdentity {
                activation: SessionFrame::new(105),
                warp_map: WarpMapRevision::first(),
            },
        ),
        false,
    );
    let mut tracks = TrackSlots::default();
    assert!(tracks.insert(track).is_none());
    let context = RenderContext::new(
        SessionFrame::new(100)..SessionFrame::new(110),
        shape.sample_rate,
        None,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture context is valid");
    let mut left = [1.0; 10];
    let mut right = [1.0; 10];
    let input: [&[f32]; 0] = [];
    let mut output = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &input,
        outputs: &mut output,
    };

    let (started, _, _) = pass.render_audio(
        Some(&context),
        RenderTargets {
            notification_tx: &mut inputs.notif_tx,
            metrics: inputs.playback.metrics(),
            tracks: &mut tracks,
            seek_epoch: 7,
        },
        &mut buffers,
        10,
        false,
    );

    assert!(!started);
    assert!(left.iter().all(|sample| *sample == 0.0));
    assert!(right.iter().all(|sample| *sample == 0.0));
}

#[kithara::test]
fn activation_blends_real_old_and_new_pcm_for_forty_frames_across_chunks() {
    const BLEND_FRAMES: usize = 40;
    const CHUNK_FRAMES: usize = 31;
    let mut resource = scheduled_revision_resource(100, BLEND_FRAMES);
    let mut rendered = Vec::new();

    for start in [100, 131] {
        let context = RenderContext::new(
            SessionFrame::new(start)
                ..SessionFrame::new(
                    start + i64::try_from(CHUNK_FRAMES).expect("chunk length fits i64"),
                ),
            NonZeroU32::new(Consts::SAMPLE_RATE).expect("static rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture context is valid");
        let mut left = [0.0; CHUNK_FRAMES];
        let mut right = [0.0; CHUNK_FRAMES];
        let mut output = [&mut left[..], &mut right[..]];
        let (outcome, _) = resource.read_with_context(
            Some(&context),
            None,
            &mut output,
            0..CHUNK_FRAMES,
            &RtMetrics::default(),
        );
        assert_eq!(
            outcome,
            crate::rt::track::ReadOutcome::Full {
                frames: CHUNK_FRAMES
            }
        );
        assert_eq!(left, right);
        rendered.extend(left);
    }

    for (frame, sample) in rendered.iter().copied().enumerate() {
        let expected = if frame < BLEND_FRAMES {
            let incoming_gain = f32::from(u16::try_from(frame).expect("blend frame fits u16"))
                / f32::from(u16::try_from(BLEND_FRAMES).expect("blend length fits u16"));
            1.0f32.mul_add(1.0 - incoming_gain, 2.0 * incoming_gain)
        } else {
            2.0
        };
        assert_eq!(sample.to_bits(), expected.to_bits(), "frame {frame}");
    }
}

fn warped_player_resource(
    pools: &PoolRegion<TestPools>,
    controls: &Arc<StretchControls>,
    src: &str,
    samples: Vec<f32>,
) -> Box<PlayerResource> {
    let resource = Resource::from_reader(EofReader::with_frames(samples), None)
        .with_playback_rate(PlaybackRate::for_warp(Arc::clone(controls)));
    PlayerResource::new(resource, Arc::from(src), pools)
        .map(Box::new)
        .unwrap_or_else(|error| panic!("test player resource: {error}"))
}

fn process_block(processor: &mut PlayerNodeProcessor, extra: &mut ProcExtra) {
    let info = ProcInfo {
        sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        frames: Consts::BLOCK_FRAMES,
        in_silence_mask: SilenceMask::default(),
        out_silence_mask: SilenceMask::default(),
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        prev_output_was_silent: false,
        sample_rate_recip: f64::from(Consts::SAMPLE_RATE).recip(),
        clock_samples: InstantSamples(0),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    };
    let inputs: [&[f32]; 0] = [];
    let mut left = [0.0; Consts::BLOCK_FRAMES];
    let mut right = [0.0; Consts::BLOCK_FRAMES];
    let mut outputs = [&mut left[..], &mut right[..]];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let mut immediate: [Option<NodeEvent>; 0] = [];
    let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
    let mut indices: Vec<ProcEventsIndex> = Vec::new();
    let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
    let _ = processor.process(&info, buffers, &mut events, extra);
}

fn rate_notifications(control: &mut crate::bridge::SlotControl) -> Vec<f32> {
    let mut rates = Vec::new();
    while let Some(notification) = control.notif_rx.try_pop() {
        if let PlayerNotification::RateChanged { rate } = notification {
            rates.push(rate);
        }
    }
    rates
}

#[kithara::test(native, flash(false))]
fn playback_rate_reports_only_a_real_warp_control() {
    let fixed = Resource::from_reader(EofReader::default(), None);
    assert_eq!(fixed.apply_playback_rate(1.5), 1.0);
    assert_eq!(fixed.playback_rate(), 1.0);

    let controls = StretchControls::new(1.0);
    let warped = Resource::from_reader(EofReader::default(), None)
        .with_playback_rate(PlaybackRate::for_warp(Arc::clone(&controls)));
    if supports_playback_rate() {
        assert_eq!(warped.apply_playback_rate(1.5), 1.5);
        assert!((controls.speed() - 1.5).abs() < f32::EPSILON);
        controls.set_speed(1.25);
        assert_eq!(warped.playback_rate(), 1.25);
    } else {
        assert_eq!(warped.apply_playback_rate(1.5), 1.0);
        assert!((controls.speed() - 1.0).abs() < f32::EPSILON);
        controls.set_speed(1.25);
        assert_eq!(warped.playback_rate(), 1.0);
    }
}

#[kithara::test(native, flash(false))]
fn loading_next_warp_resource_preserves_shared_target_and_effective_capability(half: Vec<f32>) {
    let controls = StretchControls::new(1.0);
    let pools = pools();
    let effective_rate = if supports_playback_rate() { 1.5 } else { 1.0 };
    let (inputs, mut control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        max_block_frames: NonZeroU32::new(
            u32::try_from(Consts::BLOCK_FRAMES).expect("block size fits u32"),
        )
        .expect("static block size"),
    };
    let mut processor =
        PlayerNodeProcessor::new(inputs, shape, &pools, crate::DEFAULT_GATE_SMOOTHING);
    let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    let mut extra = ProcExtra {
        logger,
        store: ProcStore::with_capacity(0),
        scratch_buffers: ChannelBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(Consts::BLOCK_FRAMES),
        declick_values: DeclickValues::new(NonZeroU32::new(16).expect("static declick length")),
    };
    let first: Arc<str> = Arc::from("first");
    let first_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: warped_player_resource(&pools, &controls, &first, half.clone()),
            item_id: first_id,
        })
        .expect("load first track");
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(first_id)))
        .expect("fade in first track");
    control
        .cmd_tx
        .try_push(PlayerCmd::SetPaused {
            paused: false,
            item_id: None,
        })
        .expect("start playback");
    process_block(&mut processor, &mut extra);
    let _ = rate_notifications(&mut control);

    controls.set_speed(1.5);
    let first_position = processor
        .track(first_id)
        .expect("first track loaded")
        .position();
    process_block(&mut processor, &mut extra);
    let first_advance = processor
        .track(first_id)
        .expect("first track loaded")
        .position()
        - first_position;
    let block_frames = u32::try_from(Consts::BLOCK_FRAMES).expect("block size fits u32");
    let expected_advance = f64::from(block_frames) / f64::from(Consts::SAMPLE_RATE);
    assert!((first_advance - expected_advance).abs() < f64::EPSILON);
    assert_eq!(
        processor.playback().rate.load(Ordering::Relaxed),
        effective_rate
    );
    let notifications = rate_notifications(&mut control);
    if supports_playback_rate() {
        assert_eq!(notifications, [1.5]);
    } else {
        assert!(notifications.is_empty());
    }

    let next: Arc<str> = Arc::from("next");
    let next_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: warped_player_resource(&pools, &controls, &next, half),
            item_id: next_id,
        })
        .expect("load next track");
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(next_id)))
        .expect("fade in next track");
    assert_eq!(controls.speed(), 1.5);

    process_block(&mut processor, &mut extra);

    assert_eq!(controls.speed(), 1.5);
    assert_eq!(
        processor.playback().rate.load(Ordering::Relaxed),
        effective_rate
    );
    assert_eq!(
        processor
            .track(next_id)
            .expect("next track loaded")
            .position(),
        expected_advance
    );
    assert!(rate_notifications(&mut control).is_empty());
}

/// Pin (W3 Task 3.3 (b)): a mid-session unload — i.e. dropping the
/// `Resource` — cancels the whole per-track subtree, not just the `Audio`
/// half. The per-track token `T` is passed by identity into both the inner
/// stream (File/Hls) and the `Audio` config; under propagate-down both take
/// `T.child()`, so `Audio::Drop` alone would only reach its own child and
/// leave the stream-side fetch loops running. `Resource::Drop` must cancel
/// `T` so the stream subtree (modelled here by `stream_sub`) is torn down.
#[kithara::test(native, flash(false))]
fn drop_cancels_whole_per_track_subtree_not_just_audio() {
    let track = CancelToken::never();
    let stream_sub = track.child(); // File/Hls subtree F = T.child()
    let audio_sub = track.child(); // Audio subtree A = T.child()

    let mut resource = Resource::from_reader(EofReader::default(), None);
    resource.reader.0 = CancelGuard(Some(track.clone()));

    assert!(!stream_sub.is_cancelled() && !audio_sub.is_cancelled());
    drop(resource);
    assert!(
        stream_sub.is_cancelled(),
        "unload must cancel the stream-side subtree, not only the Audio half"
    );
    assert!(audio_sub.is_cancelled());
    assert!(track.is_cancelled());
}

/// A resource with no per-track cancel wired in (custom reader) drops
/// without panicking and cancels nothing.
#[kithara::test(native, flash(false))]
fn drop_without_cancel_is_passive() {
    let resource = Resource::from_reader(EofReader::default(), None);
    drop(resource);
}

#[kithara::test(native, flash(false))]
fn drop_cancels_before_inner_reader_teardown() {
    let track = CancelToken::never();
    let state = Arc::new(AtomicU8::new(DropState::NOT_DROPPED));
    let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
    let mut resource = Resource::from_reader(reader, None);
    resource.reader.0 = CancelGuard(Some(track));

    drop(resource);

    assert_eq!(state.load(Ordering::SeqCst), DropState::AFTER_CANCEL);
}

#[kithara::test(native, flash(false))]
fn reader_unwrap_disarms_resource_cancel() {
    let track = CancelToken::never();
    let state = Arc::new(AtomicU8::new(DropState::NOT_DROPPED));
    let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
    let mut resource = Resource::from_reader(reader, None);
    resource.reader.0 = CancelGuard(Some(track.clone()));

    let reader: Box<dyn AudioReader> = resource.into();

    assert!(!track.is_cancelled());
    assert_eq!(state.load(Ordering::SeqCst), DropState::NOT_DROPPED);

    drop(reader);

    assert!(!track.is_cancelled());
    assert_eq!(state.load(Ordering::SeqCst), DropState::BEFORE_CANCEL);
}

#[kithara::test(native, flash(false))]
fn seek_withdraws_the_resident_warp_context(half: Vec<f32>) {
    let mut warp = Warp::new((), &WarpConfig::builder().build());
    let publisher = warp
        .take_publisher()
        .expect("fixture Warp owns its publisher");
    let reader = publisher.reader();
    let mut resource = Resource::from_reader(EofReader::with_frames(half[..2].to_vec()), None);
    let context = RenderContext::new(
        SessionFrame::new(0)..SessionFrame::new(1),
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        None,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture context is valid");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(1)
            .output(SessionFrame::new(0))
            .build(),
    );
    assert!(reader.load().is_some());
    resource.render_publisher = Some(publisher);
    let mut resource = PlayerResource::new(resource, Arc::from("seek"), &pools())
        .unwrap_or_else(|error| panic!("test player resource: {error}"));

    resource.reset_for_seek();

    assert!(reader.load().is_none());
}
