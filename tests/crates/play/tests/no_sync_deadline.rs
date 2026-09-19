#![cfg(not(target_arch = "wasm32"))]

use std::{collections::BTreeMap, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::node::ProcBuffers;
use kithara::{
    events::TrackId,
    platform::sync::Arc,
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
    signal::AudioSpec,
};
use kithara_integration_tests::{audio_mock::TestPcmReader, offline::peak};
use kithara_test_fixtures::integration_fixtures::deadline_tracks;
use kithara_test_utils::test::usdt::{self, ProbeEvent};
use ringbuf::traits::Producer;

use crate::bufpool_ext::pools;

struct Consts;

impl Consts {
    const BLOCK_FRAMES: [u32; 4] = [128, 256, 512, 1_024];
    /// Callbacks one census window covers. A mix that walked its track list per
    /// frame would fire the probe `block_frames` times per track per callback,
    /// and this window keeps even that history inside `usdt::MAX_EVENTS`, so
    /// the walk count is what fails there, not the recorder.
    const CENSUS_BLOCKS: usize = 32;
    const CHANNELS: u16 = 2;
    /// Callbacks a cell renders after warmup, read as
    /// `MEASURED_BLOCKS / CENSUS_BLOCKS` windows. Every one of them is checked
    /// for the exact sum of all its tracks, so the length is the PCM evidence;
    /// the walk count is already exact inside a single window.
    const MEASURED_BLOCKS: usize = 4_096;
    const SAMPLE_RATE: u32 = 48_000;
    const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    const TRACK_SECONDS: f64 = 300.0;
    const WARMUP_BLOCKS: usize = 512;
}

fn non_zero(value: u32, label: &str) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("{label} must be non-zero"))
}

fn spec() -> AudioSpec {
    AudioSpec::new(
        Consts::CHANNELS,
        non_zero(Consts::SAMPLE_RATE, "sample rate"),
    )
}

fn processor(block_frames: u32) -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: non_zero(Consts::SAMPLE_RATE, "sample rate"),
        max_block_frames: non_zero(block_frames, "block frames"),
    };
    (
        PlayerNodeProcessor::new(
            inputs,
            shape,
            &pools(),
            kithara::play::DEFAULT_GATE_SMOOTHING,
        ),
        control,
    )
}

fn send(control: &mut SlotControl, cmd: PlayerCmd) {
    if control.cmd_tx.try_push(cmd).is_err() {
        panic!("no-SYNC deadline command ring must accept setup commands");
    }
}

fn load_tracks(
    processor: &mut PlayerNodeProcessor,
    control: &mut SlotControl,
    count: usize,
    deadline_tracks: [&'static [u8]; 4],
) -> f32 {
    let pools = pools();
    let tracks: Vec<(Arc<str>, TrackId)> = (0..count)
        .map(|idx| {
            (
                Arc::from(format!("no-sync-deadline-track-{idx}").as_str()),
                TrackId::allocate(),
            )
        })
        .collect();
    let mut expected_sample = 0.0;

    for (idx, (src, item_id)) in tracks.iter().enumerate() {
        let value = f32::from(u16::try_from(idx + 1).expect("track index fits u16")) * 0.02;
        expected_sample += value;
        let resource = Resource::from_reader(
            TestPcmReader::from_pcm(spec(), Consts::TRACK_SECONDS, deadline_tracks[idx]),
            Some(Arc::clone(src)),
        );
        send(
            control,
            PlayerCmd::LoadTrack {
                resource: Box::new(
                    PlayerResource::new(resource, Arc::clone(src), &pools)
                        .expect("player resource fits the test pool budget"),
                ),
                item_id: *item_id,
            },
        );
    }
    send(control, PlayerCmd::SetPaused(false));
    processor.drain_commands();

    for (src, item_id) in &tracks {
        match processor.track_mut(*item_id) {
            Some(track) => track.play(),
            None => panic!("no-SYNC deadline track {src} did not reach the processor"),
        }
    }
    expected_sample
}

fn render_block(
    processor: &mut PlayerNodeProcessor,
    control: &SlotControl,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    let frames = out_l.len();
    let is_playing = control.playback.playing.load(Ordering::SeqCst);
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [out_l, out_r];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };

    processor.drain_commands();
    processor.cleanup_finished_tracks();
    let _ = processor.render_audio(&mut buffers, frames, is_playing);
}

fn assert_all_tracks_contributed(
    samples: &[f32],
    expected_sample: f32,
    block_frames: u32,
    tracks: usize,
) {
    assert!(
        samples
            .iter()
            .all(|sample| (*sample - expected_sample).abs() <= 1.0e-5),
        "deadline cell must render the exact sum of all {tracks} track(s) at {block_frames} frames: expected {expected_sample}, observed peak {}",
        peak(samples),
    );
}

/// How many times the mix walked each track, keyed by the track it walked.
fn walks_per_track(recorded: &[ProbeEvent]) -> BTreeMap<u64, usize> {
    let mut walks = BTreeMap::new();
    for event in recorded.iter().filter(|event| event.probe == "render") {
        let track = event
            .field("track_id")
            .expect("the render probe carries the track it walked");
        *walks.entry(track).or_insert(0_usize) += 1;
    }
    walks
}

fn assert_one_walk_per_track_per_block(recorded: &[ProbeEvent], block_frames: u32, tracks: usize) {
    let walks = walks_per_track(recorded);
    assert_eq!(
        walks.len(),
        tracks,
        "a census window at {block_frames} frames must walk every one of the \
         {tracks} playing track(s), observed {walks:?}"
    );
    assert!(
        walks.values().all(|count| *count == Consts::CENSUS_BLOCKS),
        "the mix must walk each of {tracks} track(s) once per callback; \
         {} callbacks at {block_frames} frames walked {walks:?}",
        Consts::CENSUS_BLOCKS,
    );
}

fn assert_each_walk_covers_the_block(recorded: &[ProbeEvent], block_frames: u32, tracks: usize) {
    let sliced = recorded
        .iter()
        .filter(|event| event.probe == "render")
        .find(|event| {
            event.field("range_start") != Some(0)
                || event.field("range_end") != Some(u64::from(block_frames))
        });
    assert!(
        sliced.is_none(),
        "a mix walk must cover the whole callback of {block_frames} frames with \
         {tracks} track(s), not a slice of it: {sliced:?}"
    );
}

fn census(block_frames: u32, tracks: usize, deadline_tracks: [&'static [u8]; 4]) {
    let (mut processor, mut control) = processor(block_frames);
    let expected_sample = load_tracks(&mut processor, &mut control, tracks, deadline_tracks);
    assert_eq!(
        processor.track_count(),
        tracks,
        "deadline cell must load exactly {tracks} active track(s)"
    );

    let frames = usize::try_from(block_frames).expect("block frames fit usize");
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];
    let metrics_before = control.playback.metrics().snapshot();

    for _ in 0..Consts::WARMUP_BLOCKS {
        render_block(&mut processor, &control, &mut out_l, &mut out_r);
    }
    assert!(
        peak(&out_l).max(peak(&out_r)) > 0.0,
        "deadline cell must reach audible PCM before the census ({block_frames} frames, {tracks} track(s))"
    );
    assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
    assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);

    for _ in 0..Consts::MEASURED_BLOCKS / Consts::CENSUS_BLOCKS {
        let trace = usdt::scope();
        for _ in 0..Consts::CENSUS_BLOCKS {
            render_block(&mut processor, &control, &mut out_l, &mut out_r);
            assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
            assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);
        }
        let recorded = trace.events();
        drop(trace);
        assert_one_walk_per_track_per_block(&recorded, block_frames, tracks);
        assert_each_walk_covers_the_block(&recorded, block_frames, tracks);
    }

    let metrics_after = control.playback.metrics().snapshot();
    assert_eq!(
        metrics_after.underruns(),
        metrics_before.underruns(),
        "no-SYNC rendering must have zero underrun delta ({block_frames} frames, {tracks} track(s))"
    );
    assert_eq!(
        metrics_after.decode_errors(),
        metrics_before.decode_errors(),
        "no-SYNC rendering must have zero decode-error delta ({block_frames} frames, {tracks} track(s))"
    );
    assert_eq!(
        processor.track_count(),
        tracks,
        "deadline cell must finish with exactly {tracks} active track(s)"
    );
    assert!(
        peak(&out_l).max(peak(&out_r)) > 0.0,
        "census callbacks must finish on audible PCM, not silence"
    );
}

/// Mixing a track costs the same however many tracks play.
///
/// The hot path scans the active tracks again inside its per-track loop, so a
/// stray per-frame step there turns the mix quadratic and the audio deadline
/// stops holding as the queue fills. The `render` probe fires on every walk the
/// mix makes over a track, so a callback's firings are that work itself: one
/// per playing track, each covering the whole block. A mix that walks per frame
/// multiplies both by the block size. Counting walks asks no clock, so the
/// verdict holds whatever else the runner is doing. Absolute block cost belongs
/// to the `rt_block_budget` bench, which times this processor and asks the
/// clock for no verdict.
#[kithara::test(native, serial, flash(false))]
fn mixing_a_track_costs_the_same_however_many_tracks_play(deadline_tracks: [&'static [u8]; 4]) {
    for block_frames in Consts::BLOCK_FRAMES {
        for tracks in Consts::TRACK_COUNTS {
            census(block_frames, tracks, deadline_tracks);
        }
    }
}
