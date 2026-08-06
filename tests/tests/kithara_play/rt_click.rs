//! A click is a step in the waveform, so every test here renders PCM through
//! the same calls `process()` makes and measures the largest jump between
//! neighbouring frames. The mock source is constant DC: whatever step the
//! render adds is the transport or the fade, never the material.

#![cfg(not(target_arch = "wasm32"))]

use std::{num::NonZeroU32, sync::atomic::Ordering};

use firewheel::node::ProcBuffers;
use kithara::{
    bufpool::PcmPool,
    decode::PcmSpec,
    platform::sync::Arc,
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, SlotControl, TrackTransition, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
};
use kithara_integration_tests::audio_mock::{TEST_PCM_DEFAULT_VALUE, TestPcmReader};
use ringbuf::traits::Producer;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const TRACK_SECS: f64 = 60.0;
const SECOND_LEVEL: f32 = 0.25;
const FADE_SECONDS: f32 = 0.25;
const WARMUP_BLOCKS: usize = 24;
const SETTLE_BLOCKS: usize = 40;
const MAX_STEP: f32 = 0.01;
const EXACT: f32 = 1.0e-6;

fn spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"))
}

fn processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"),
        max_block_frames: NonZeroU32::new(128).expect("non-zero block"),
    };
    (
        PlayerNodeProcessor::new(inputs, shape, &PcmPool::default()),
        control,
    )
}

fn track(src: &str, level: f32) -> Box<PlayerResource> {
    Box::new(PlayerResource::new(
        Resource::from_reader(TestPcmReader::with_value(spec(), TRACK_SECS, level), None),
        Arc::from(src),
        &PcmPool::default(),
    ))
}

fn load(control: &mut SlotControl, src: &str, level: f32) {
    push(
        control,
        PlayerCmd::LoadTrack {
            resource: track(src, level),
            item_id: None,
        },
    );
}

fn push(control: &mut SlotControl, cmd: PlayerCmd) {
    control.cmd_tx.try_push(cmd).ok();
}

fn start(processor: &mut PlayerNodeProcessor, src: &str) {
    if let Some(track) = processor.track_mut(&Arc::from(src)) {
        track.play();
    }
}

fn block(processor: &mut PlayerNodeProcessor) -> (Vec<f32>, bool) {
    let mut out_l = vec![0.0f32; BLOCK_FRAMES];
    let mut out_r = vec![0.0f32; BLOCK_FRAMES];
    processor.drain_commands();
    processor.cleanup_finished_tracks();
    let is_playing = processor.playback().playing.load(Ordering::SeqCst);
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let (read, _) = processor.render_audio(&mut buffers, BLOCK_FRAMES, is_playing);
    (out_l, read)
}

fn pump(processor: &mut PlayerNodeProcessor, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES);
    for _ in 0..blocks {
        let (out_l, _) = block(processor);
        rendered.extend_from_slice(&out_l);
    }
    rendered
}

fn last(rendered: &[f32]) -> f32 {
    rendered.last().copied().expect("blocks were rendered")
}

fn max_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .fold(0.0f32, |worst, pair| worst.max((pair[1] - pair[0]).abs()))
}

fn across(before: &[f32], after: &[f32]) -> Vec<f32> {
    let mut stream = vec![last(before)];
    stream.extend_from_slice(after);
    stream
}

#[kithara::test]
fn pausing_fades_the_output_out() {
    let (mut processor, mut control) = processor();
    load(&mut control, "a.mp3", TEST_PCM_DEFAULT_VALUE);
    push(&mut control, PlayerCmd::SetPaused(false));
    processor.drain_commands();
    start(&mut processor, "a.mp3");

    let playing = pump(&mut processor, WARMUP_BLOCKS);
    assert!(
        (last(&playing) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the track plays at full level before the pause ({})",
        last(&playing)
    );

    push(&mut control, PlayerCmd::SetPaused(true));
    let paused = pump(&mut processor, SETTLE_BLOCKS);

    let step = max_step(&across(&playing, &paused));
    assert!(
        step <= MAX_STEP,
        "pause must fade the output out, not cut the block (step {step})"
    );
    assert!(
        paused[paused.len() - BLOCK_FRAMES..]
            .iter()
            .all(|sample| *sample == 0.0),
        "a paused player still reaches silence"
    );
    let (_, still_reading) = block(&mut processor);
    assert!(
        !still_reading,
        "once the fade has run out the pause stops reading the tracks"
    );
}

#[kithara::test]
fn resuming_fades_the_output_in() {
    let (mut processor, mut control) = processor();
    load(&mut control, "a.mp3", TEST_PCM_DEFAULT_VALUE);
    push(&mut control, PlayerCmd::SetPaused(false));
    processor.drain_commands();
    start(&mut processor, "a.mp3");
    pump(&mut processor, WARMUP_BLOCKS);

    push(&mut control, PlayerCmd::SetPaused(true));
    let paused = pump(&mut processor, SETTLE_BLOCKS);
    assert!(last(&paused) == 0.0, "the pause settled at silence");

    push(&mut control, PlayerCmd::SetPaused(false));
    let resumed = pump(&mut processor, SETTLE_BLOCKS);

    let step = max_step(&across(&paused, &resumed));
    assert!(
        step <= MAX_STEP,
        "resume must fade the output in, not step into it (step {step})"
    );
    assert!(
        (last(&resumed) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "playback is back at full level ({})",
        last(&resumed)
    );
}

fn fading_in() -> (PlayerNodeProcessor, SlotControl, Vec<f32>) {
    let (mut processor, mut control) = processor();
    load(&mut control, "a.mp3", TEST_PCM_DEFAULT_VALUE);
    push(&mut control, PlayerCmd::SetFadeDuration(FADE_SECONDS));
    push(&mut control, PlayerCmd::SetPaused(false));
    push(
        &mut control,
        PlayerCmd::Transition(TrackTransition::FadeIn(Arc::from("a.mp3"))),
    );

    let fading = pump(&mut processor, WARMUP_BLOCKS);
    let level = last(&fading);
    assert!(
        level > 0.0 && level < TEST_PCM_DEFAULT_VALUE * 0.9,
        "the fade-in is still climbing ({level})"
    );

    (processor, control, fading)
}

#[kithara::test]
fn seeking_a_fading_track_does_not_snap_the_mix() {
    let (mut processor, mut control, fading) = fading_in();

    let seek_epoch = processor.playback().next_seek_epoch();
    push(
        &mut control,
        PlayerCmd::Seek {
            seconds: 5.0,
            seek_epoch,
        },
    );
    let sought = pump(&mut processor, WARMUP_BLOCKS);

    let step = max_step(&across(&fading, &sought));
    assert!(
        step <= MAX_STEP,
        "a seek must not jump the mix of a fading track (step {step})"
    );
}

#[kithara::test]
fn resending_the_crossfade_duration_does_not_snap_the_mix() {
    let (mut processor, mut control, fading) = fading_in();

    push(&mut control, PlayerCmd::SetFadeDuration(FADE_SECONDS));
    let resent = pump(&mut processor, WARMUP_BLOCKS);

    let step = max_step(&across(&fading, &resent));
    assert!(
        step <= MAX_STEP,
        "an unchanged crossfade duration must leave the fade alone (step {step})"
    );
}

#[kithara::test]
fn a_track_started_without_a_crossfade_is_instant() {
    let (mut processor, mut control) = processor();
    load(&mut control, "a.mp3", TEST_PCM_DEFAULT_VALUE);
    push(&mut control, PlayerCmd::SetFadeDuration(0.0));
    push(&mut control, PlayerCmd::SetPaused(false));
    processor.drain_commands();
    start(&mut processor, "a.mp3");

    let playing = pump(&mut processor, WARMUP_BLOCKS);
    assert!(
        (last(&playing) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the first track plays at full level ({})",
        last(&playing)
    );

    load(&mut control, "b.mp3", SECOND_LEVEL);
    processor.drain_commands();
    start(&mut processor, "b.mp3");
    let handover = pump(&mut processor, 1);

    assert!(
        (handover[0] - (TEST_PCM_DEFAULT_VALUE + SECOND_LEVEL)).abs() < EXACT,
        "the second track is at full level on its first frame ({})",
        handover[0]
    );
}
