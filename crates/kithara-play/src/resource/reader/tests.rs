use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::{AtomicU8, Ordering},
};

use firewheel::{
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    event::{NodeEvent, ProcEvents, ProcEventsIndex, ScheduledEventEntry},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
        StreamStatus,
    },
};
use kithara_audio::{AudioControl, AudioRead, AudioSession, ReadOutcome, SeekOutcome};
use kithara_bufpool::PoolRegion;
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_signal::{AudioSpec, OutputContext, SessionEpoch, SessionFrame};
use kithara_test_fixtures::play_fixtures::half;
use kithara_test_utils::kithara;
use kithara_warp::{Warp, WarpConfig};
use ringbuf::traits::{Consumer, Producer};

use super::*;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, SharedEq, TrackTransition, slot_channels},
    rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
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
    samples: Vec<f32>,
    position_frames: usize,
    total_frames: usize,
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
            source_span: None,
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
            source_span: None,
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
        total_cpu_seconds_recip: 1.0,
        process_to_playback_delay: None,
        did_just_unbypass: false,
        last_marker_instant: InstantSamples(0),
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
    processor.events(&info, &mut events, extra);
    let _ = processor.process(&info, buffers, extra);
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
        scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(
            Consts::BLOCK_FRAMES,
        ),
        declick_values: DeclickValues::new(NonZeroU32::new(16).expect("static declick length")),
    };
    let first: Arc<str> = Arc::from("first");
    let first_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            load: kithara_sync::LoadGeneration::first(),
            resource: warped_player_resource(&pools, &controls, &first, half.clone()),
            item_id: first_id,
        })
        .expect("load first track");
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn {
            item_id: first_id,
            settings: crate::CrossfadeSettings::default(),
        }))
        .expect("fade in first track");
    control
        .cmd_tx
        .try_push(PlayerCmd::SetPaused(false))
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
    let expected_advance =
        f64::from(block_frames) * f64::from(effective_rate) / f64::from(Consts::SAMPLE_RATE);
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
            load: kithara_sync::LoadGeneration::first(),
            resource: warped_player_resource(&pools, &controls, &next, half),
            item_id: next_id,
        })
        .expect("load next track");
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn {
            item_id: next_id,
            settings: crate::CrossfadeSettings::default(),
        }))
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
    let output = OutputContext::new(
        SessionFrame::new(0)..SessionFrame::new(1),
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture output range is ordered");
    let context = RenderContext::new(output, None).expect("fixture context is valid");
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
