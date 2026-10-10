//! A click is a step in the waveform, so every test here renders PCM through the same calls
//! `process()` makes and measures the largest jump between neighbouring frames. The mock source is
//! constant DC: whatever step the render adds is the transport or the fade, never the material.
#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use firewheel::node::ProcBuffers;
use kithara_audio::mock::TEST_PCM_DEFAULT_VALUE;
use kithara_command::{Outcome, Receipt, ScopedReceipt, Seq, When};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, FaderValue, OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_fixtures::integration_fixtures::{constant_half, constant_quarter};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;

use super::{TestEnds, TestMixer, mixer_with_shape, send as send_batch};
use crate::{
    CrossfadeSettings,
    bridge::{DeckMixSettingsChange, DeckPart, DeckProtocol, Fade, FadeDir, Returned, Slot},
    rt::{
        DeckMixerConfig, StreamShape,
        track::{PcmConsumer, PlayerResource, PlayerTrack},
    },
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};
const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const SECOND_LEVEL: f32 = 0.25;
const FADE_SECONDS: f32 = 0.25;
const WARMUP_BLOCKS: usize = 24;
const SETTLE_BLOCKS: usize = 40;
const MAX_STEP: f32 = 0.01;
const EXACT: f32 = 1.0e-6;

fn crossfade(duration: f32) -> CrossfadeSettings {
    CrossfadeSettings {
        duration,
        ..CrossfadeSettings::default()
    }
}

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"))
}

struct ClickControl {
    ends: TestEnds,
    packets: Vec<PacketRing>,
}

fn processor() -> (TestMixer, ClickControl) {
    let shape = StreamShape::new(NonZeroU32::new(128).expect("block"), spec().sample_rate);
    let (mixer, ends) = mixer_with_shape(DeckMixerConfig::default(), 64, shape);
    (
        mixer,
        ClickControl {
            ends,
            packets: Vec::new(),
        },
    )
}

fn samples(input: &'static [u8], frames: usize) -> Vec<f32> {
    input
        .chunks_exact(4)
        .cycle()
        .take(frames * 2)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("sample bytes")))
        .collect()
}

fn load(control: &mut ClickControl, src: &str, input: &'static [u8]) -> Slot {
    let slot = Slot::new(u16::try_from(control.packets.len()).expect("fixture slot"));
    let mut packets = PacketRing::new(spec(), Duration::from_secs(60), 4);
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec(),
        SegmentId::FIRST,
        0,
        0,
        &samples(input, 160_000),
    ))));
    let pcm = Box::new(
        PlayerResource::new(
            PcmConsumer::new(packets.receiver.take().expect("receiver")),
            Arc::from(src),
            &pools(),
        )
        .expect("resource"),
    );
    control.packets.push(packets);
    push(
        control,
        DeckPart::Attach {
            slot,
            pcm,
            segment: SegmentId::FIRST,
        },
    );
    slot
}

fn push(control: &mut ClickControl, part: DeckPart) {
    send_batch(&mut control.ends, When::Next, vec![part]);
}

fn send_at(control: &mut ClickControl, at: SessionFrame, part: DeckPart) -> Seq {
    send_batch(&mut control.ends, When::At(at), vec![part])
}

fn answer(control: &mut ClickControl, seq: Seq) -> Receipt<DeckProtocol> {
    std::iter::from_fn(|| control.ends.ring.receipt())
        .find_map(|receipt| match receipt {
            ScopedReceipt::Scope(_, receipt) if receipt.seq() == seq => Some(receipt),
            _ => None,
        })
        .expect("the command is answered in the block it applies in")
}

fn start(processor: &mut TestMixer, slot: Slot) {
    processor
        .mixer
        .deck
        .tracks
        .at_mut(slot)
        .expect("loaded track")
        .start(Fade::Crossfade(crossfade(0.0)));
}

fn block_from(processor: &mut TestMixer, start: SessionFrame) -> (Vec<f32>, bool) {
    let mut out_l = vec![0.0f32; BLOCK_FRAMES];
    let mut out_r = vec![0.0f32; BLOCK_FRAMES];
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    processor.inbox.0.drain();
    let level = processor
        .inbox
        .0
        .scope(processor.mixer.scope)
        .expect("scope");
    let context = RenderContext::new_linear(
        OutputContext::new(
            start..SessionFrame::new(i64::from(start) + 128),
            spec().sample_rate,
            SessionEpoch::new(0),
            None,
        )
        .expect("output"),
        None,
    )
    .expect("context");
    let read = processor.mixer.render_block(
        Some(level),
        Some(&context),
        start,
        &mut buffers,
        BLOCK_FRAMES,
    );
    (out_l, read)
}

/// Renders a block on a clock that stands still: every part `push` sends applies on the next
/// block, whatever frame it starts on.
fn block(processor: &mut TestMixer) -> (Vec<f32>, bool) {
    block_from(processor, SessionFrame::default())
}

fn pump(processor: &mut TestMixer, blocks: usize) -> Vec<f32> {
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
fn pausing_fades_the_output_out(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, item_id);

    let playing = pump(&mut processor, WARMUP_BLOCKS);
    assert!(
        (last(&playing) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the track plays at full level before the pause ({})",
        last(&playing)
    );

    push(
        &mut control,
        DeckPart::Stop {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
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
fn resuming_fades_the_output_in(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, item_id);
    pump(&mut processor, WARMUP_BLOCKS);

    push(
        &mut control,
        DeckPart::Stop {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    let paused = pump(&mut processor, SETTLE_BLOCKS);
    assert!(last(&paused) == 0.0, "the pause settled at silence");

    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
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

#[kithara::test]
fn a_start_inside_a_block_sounds_from_its_frame(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    block(&mut processor);
    assert!(
        pump(&mut processor, 2).iter().all(|sample| *sample == 0.0),
        "a stopped deck stays silent"
    );

    let origin = i64::try_from(BLOCK_FRAMES).expect("a block fits the clock");
    let offset = BLOCK_FRAMES / 2;
    let at = SessionFrame::new(origin + i64::try_from(offset).expect("an offset fits the clock"));
    let seq = send_at(
        &mut control,
        at,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    let (started, _) = block_from(&mut processor, SessionFrame::new(origin));

    assert!(
        started[..offset].iter().all(|sample| *sample == 0.0),
        "the deck is silent before the frame it starts on"
    );
    assert!(
        started[offset..].iter().any(|sample| *sample > 0.0),
        "the deck sounds from the frame it starts on"
    );
    let answer = answer(&mut control, seq);
    assert!(
        matches!(answer.outcome(), Outcome::Applied { at: applied, .. } if *applied == at),
        "the start is applied at its frame"
    );
}

/// Two media positions read off one clock agree to well under a frame.
const SAME_POSITION: f64 = 1.0e-9;

fn seconds(frames: usize) -> f64 {
    f64::from(u32::try_from(frames).expect("a frame count fits the clock")) / f64::from(SAMPLE_RATE)
}

fn position(processor: &TestMixer, item_id: Slot) -> f64 {
    processor
        .track(item_id)
        .map(PlayerTrack::position)
        .expect("the deck holds the track")
}

#[kithara::test]
fn stopping_one_track_inside_a_block_holds_it_from_its_frame(
    constant_half: &'static [u8],
    constant_quarter: &'static [u8],
) {
    let (mut processor, mut control) = processor();
    let first = load(&mut control, "a.mp3", constant_half);
    let second = load(&mut control, "b.mp3", constant_quarter);
    push(
        &mut control,
        DeckPart::Start {
            slot: first,
            fade: Fade::Declick,
        },
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: second,
            fade: Fade::Declick,
        },
    );
    let both = pump(&mut processor, WARMUP_BLOCKS);
    let mixed = TEST_PCM_DEFAULT_VALUE + SECOND_LEVEL;
    assert!(
        (last(&both) - mixed).abs() < EXACT,
        "both started tracks sound ({})",
        last(&both)
    );
    let before = position(&processor, first);

    let origin = i64::try_from(BLOCK_FRAMES).expect("a block fits the clock");
    let offset = BLOCK_FRAMES / 2;
    let at = SessionFrame::new(origin + i64::try_from(offset).expect("an offset fits the clock"));
    let seq = send_at(
        &mut control,
        at,
        DeckPart::Stop {
            slot: first,
            fade: Fade::Declick,
        },
    );
    let (stopping, _) = block_from(&mut processor, SessionFrame::new(origin));

    assert!(
        stopping[..offset]
            .iter()
            .all(|sample| (*sample - mixed).abs() < EXACT),
        "both tracks sound up to the frame the stop applies on"
    );
    assert!(
        stopping[offset] < mixed,
        "the stopped track's share falls from the frame the stop applies on"
    );
    let stopped = pump(&mut processor, SETTLE_BLOCKS);
    let answer = answer(&mut control, seq);
    let declick = DeckMixerConfig::default()
        .declick_frames(spec().sample_rate)
        .get();
    let expected = before + seconds(offset + declick);
    assert!(
        matches!(answer.outcome(), Outcome::Applied { at: applied, data: () } if *applied == at)
            && answer.batch().commands.iter().any(|part| matches!(part,
                DeckPart::Returned(Returned::Stopped { slot, resume })
                if *slot == first && (resume.position.as_secs_f64() - expected).abs() < SAME_POSITION)),
        "the stop is applied at its frame and reports where the fade stopped reading \
         ({expected}): {:?}",
        answer.outcome()
    );

    let ramp = [stopping, stopped.clone()].concat();
    let step = max_step(&across(&both, &ramp));
    assert!(
        step <= MAX_STEP,
        "a stop ramps its track out, it does not cut it (step {step})"
    );
    assert!(
        ramp.iter().all(|sample| *sample >= SECOND_LEVEL - EXACT),
        "the other track keeps sounding through the stop"
    );
    assert!(
        (last(&stopped) - SECOND_LEVEL).abs() < EXACT,
        "the other track sounds alone once the ramp has run out ({})",
        last(&stopped)
    );
    let held = position(&processor, first);
    pump(&mut processor, SETTLE_BLOCKS);
    assert!(
        (position(&processor, first) - held).abs() < SAME_POSITION,
        "a stopped track is not read once its ramp has run out"
    );

    push(
        &mut control,
        DeckPart::Start {
            slot: first,
            fade: Fade::Declick,
        },
    );
    let resumed = pump(&mut processor, SETTLE_BLOCKS);
    let step = max_step(&across(&stopped, &resumed));
    assert!(
        step <= MAX_STEP,
        "a start ramps its track in, it does not step it (step {step})"
    );
    assert!(
        (last(&resumed) - mixed).abs() < EXACT,
        "the restarted track sounds with the other again ({})",
        last(&resumed)
    );
    assert!(
        (position(&processor, first) - (held + seconds(SETTLE_BLOCKS * BLOCK_FRAMES))).abs()
            < SAME_POSITION,
        "the restarted track plays on from where it held"
    );
}

/// Half the fader, a quarter of the amplitude: the deck sounds at the square of its volume.
const HALF_FADER_GAIN: f32 = 0.25;
/// Half the fader at half the mix level: the level scales what the volume leaves.
const HALF_FADER_HALF_LEVEL_GAIN: f32 = 0.125;

fn mix(change: DeckMixSettingsChange) -> DeckPart {
    DeckPart::Mix(change)
}

fn half_volume() -> DeckPart {
    mix(DeckMixSettingsChange::Volume(FaderValue::from(0.5)))
}

#[kithara::test]
fn a_volume_change_inside_a_block_moves_the_gain_from_its_frame(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, item_id);
    let unity = pump(&mut processor, WARMUP_BLOCKS);
    assert!(
        (last(&unity) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the deck plays at unity before its volume changes ({})",
        last(&unity)
    );

    let origin = i64::try_from(BLOCK_FRAMES).expect("a block fits the clock");
    let offset = BLOCK_FRAMES / 2;
    let at = SessionFrame::new(origin + i64::try_from(offset).expect("an offset fits the clock"));
    let seq = send_at(&mut control, at, half_volume());
    let (changed, _) = block_from(&mut processor, SessionFrame::new(origin));

    assert!(
        changed[..offset]
            .iter()
            .all(|sample| (*sample - TEST_PCM_DEFAULT_VALUE).abs() < EXACT),
        "the deck keeps its gain before the frame the change applies on"
    );
    assert!(
        changed[offset] < TEST_PCM_DEFAULT_VALUE,
        "the gain moves from the frame the change applies on"
    );
    let answer = answer(&mut control, seq);
    assert!(
        matches!(answer.outcome(), Outcome::Applied { at: applied, .. } if *applied == at),
        "the change is applied at its frame"
    );

    let settled = pump(&mut processor, SETTLE_BLOCKS);
    let step = max_step(&across(&unity, &[changed, settled.clone()].concat()));
    assert!(
        step <= MAX_STEP,
        "a volume change ramps the gain, it does not step it (step {step})"
    );
    assert!(
        (last(&settled) - TEST_PCM_DEFAULT_VALUE * HALF_FADER_GAIN).abs() < EXACT,
        "the deck settles at the square of its volume ({})",
        last(&settled)
    );
}

#[kithara::test]
fn a_level_change_inside_a_block_scales_the_volume_from_its_frame(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, item_id);
    pump(&mut processor, WARMUP_BLOCKS);
    push(&mut control, half_volume());
    let half = pump(&mut processor, SETTLE_BLOCKS);
    let quarter = TEST_PCM_DEFAULT_VALUE * HALF_FADER_GAIN;
    assert!(
        (last(&half) - quarter).abs() < EXACT,
        "the deck plays at the square of its volume before its level changes ({})",
        last(&half)
    );

    let origin = i64::try_from(BLOCK_FRAMES).expect("a block fits the clock");
    let offset = BLOCK_FRAMES / 2;
    let at = SessionFrame::new(origin + i64::try_from(offset).expect("an offset fits the clock"));
    let seq = send_at(&mut control, at, mix(DeckMixSettingsChange::Level(0.5)));
    let (changed, _) = block_from(&mut processor, SessionFrame::new(origin));

    assert!(
        changed[..offset]
            .iter()
            .all(|sample| (*sample - quarter).abs() < EXACT),
        "the deck keeps its gain before the frame the level applies on"
    );
    assert!(
        changed[offset] < quarter,
        "the gain moves from the frame the level applies on"
    );
    let answer = answer(&mut control, seq);
    assert!(
        matches!(answer.outcome(), Outcome::Applied { at: applied, .. } if *applied == at),
        "the level is applied at its frame"
    );

    let settled = pump(&mut processor, SETTLE_BLOCKS);
    let step = max_step(&across(&half, &[changed, settled.clone()].concat()));
    assert!(
        step <= MAX_STEP,
        "a level change ramps the gain, it does not step it (step {step})"
    );
    assert!(
        (last(&settled) - TEST_PCM_DEFAULT_VALUE * HALF_FADER_HALF_LEVEL_GAIN).abs() < EXACT,
        "the deck settles at the square of its volume times its level ({})",
        last(&settled)
    );
}

#[kithara::test]
fn a_muted_deck_is_silent_at_any_volume_and_unmutes_to_it(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, item_id);
    let unity = pump(&mut processor, WARMUP_BLOCKS);

    push(&mut control, mix(DeckMixSettingsChange::Muted(true)));
    let muted = pump(&mut processor, SETTLE_BLOCKS);
    let step = max_step(&across(&unity, &muted));
    assert!(
        step <= MAX_STEP,
        "muting ramps the deck down, it does not cut it (step {step})"
    );
    assert!(last(&muted) == 0.0, "a muted deck reaches silence");

    push(&mut control, half_volume());
    assert!(
        pump(&mut processor, SETTLE_BLOCKS)
            .iter()
            .all(|sample| *sample == 0.0),
        "a volume change leaves a muted deck silent"
    );

    push(&mut control, mix(DeckMixSettingsChange::Muted(false)));
    let unmuted = pump(&mut processor, SETTLE_BLOCKS);
    assert!(
        (last(&unmuted) - TEST_PCM_DEFAULT_VALUE * HALF_FADER_GAIN).abs() < EXACT,
        "unmuting brings the deck back at the volume it was given while muted ({})",
        last(&unmuted)
    );
}

fn fading_in(constant_half: &'static [u8]) -> (TestMixer, ClickControl, Vec<f32>, Slot) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Declick,
        },
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Crossfade(crossfade(FADE_SECONDS)),
        },
    );

    let fading = pump(&mut processor, WARMUP_BLOCKS);
    let level = last(&fading);
    assert!(
        level > 0.0 && level < TEST_PCM_DEFAULT_VALUE * 0.9,
        "the fade-in is still climbing ({level})"
    );

    (processor, control, fading, item_id)
}

#[kithara::test]
fn reversing_a_fade_in_continues_from_the_gain_it_reached(constant_half: &'static [u8]) {
    let (mut processor, mut control, fading, item_id) = fading_in(constant_half);

    push(
        &mut control,
        DeckPart::Fade {
            slot: item_id,
            settings: crossfade(FADE_SECONDS),
            dir: FadeDir::Out,
        },
    );
    let reversed = pump(&mut processor, SETTLE_BLOCKS * 3);

    let step = max_step(&across(&fading, &reversed));
    assert!(
        step <= MAX_STEP,
        "a cancelled fade-in fades out from the gain it reached, not from full level (step {step})"
    );
    assert!(
        last(&reversed) == 0.0,
        "the reversed fade still reaches silence ({})",
        last(&reversed)
    );
}

#[kithara::test]
fn reversing_a_fade_out_continues_from_the_gain_it_reached(constant_half: &'static [u8]) {
    let (mut processor, mut control, _, item_id) = fading_in(constant_half);
    pump(&mut processor, SETTLE_BLOCKS * 20);

    push(
        &mut control,
        DeckPart::Fade {
            slot: item_id,
            settings: crossfade(FADE_SECONDS),
            dir: FadeDir::Out,
        },
    );
    let fading_out = pump(&mut processor, WARMUP_BLOCKS);
    let level = last(&fading_out);
    assert!(
        level > TEST_PCM_DEFAULT_VALUE * 0.1 && level < TEST_PCM_DEFAULT_VALUE,
        "the fade-out is still falling ({level})"
    );

    let reached = position(&processor, item_id);
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Crossfade(crossfade(FADE_SECONDS)),
        },
    );
    let reversed = pump(&mut processor, SETTLE_BLOCKS * 3);

    let step = max_step(&across(&fading_out, &reversed));
    assert!(
        step <= MAX_STEP,
        "a cancelled fade-out fades back in from the gain it reached, not from silence \
         (step {step})"
    );
    assert!(
        (position(&processor, item_id) - (reached + seconds(reversed.len()))).abs() < SAME_POSITION,
        "a fade-in does not seek: the track plays on from {reached} s, now at {} s",
        position(&processor, item_id)
    );
    assert!(
        (last(&reversed) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the reversed fade reaches full level ({})",
        last(&reversed)
    );
}

#[kithara::test]
fn seeking_a_fading_track_does_not_snap_the_mix(constant_half: &'static [u8]) {
    let (mut processor, mut control, fading, item_id) = fading_in(constant_half);

    let segment = SegmentId::FIRST.next();
    control.packets[usize::from(item_id.get())].push(PcmPacket::Chunk(Box::new(chunk(
        spec(),
        segment,
        0,
        5 * u64::from(SAMPLE_RATE),
        &samples(constant_half, 4096),
    ))));
    push(
        &mut control,
        DeckPart::Adopt {
            slot: item_id,
            segment,
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
fn each_fade_runs_under_its_own_settings(
    constant_half: &'static [u8],
    constant_quarter: &'static [u8],
) {
    let (mut processor, mut control, _, first) = fading_in(constant_half);
    let settled = pump(&mut processor, SETTLE_BLOCKS * 20);
    assert!(
        (last(&settled) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the first fade has settled under its own duration ({})",
        last(&settled)
    );

    let second_id = load(&mut control, "b.mp3", constant_quarter);
    push(
        &mut control,
        DeckPart::Fade {
            slot: first,
            settings: crossfade(FADE_SECONDS / 10.0),
            dir: FadeDir::Out,
        },
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: second_id,
            fade: Fade::Crossfade(crossfade(FADE_SECONDS / 10.0)),
        },
    );
    let handed_over = pump(&mut processor, SETTLE_BLOCKS * 3);
    assert!(
        (last(&handed_over) - SECOND_LEVEL).abs() < EXACT,
        "the next fade runs under its own duration: a tenth of the first settles within three \
         settle windows, the first would not ({})",
        last(&handed_over)
    );
}

#[kithara::test]
fn a_track_started_without_a_crossfade_is_instant(
    constant_quarter: &'static [u8],
    constant_half: &'static [u8],
) {
    let (mut processor, mut control) = processor();
    let first_id = load(&mut control, "a.mp3", constant_half);
    push(
        &mut control,
        DeckPart::Start {
            slot: first_id,
            fade: Fade::Declick,
        },
    );
    block(&mut processor);
    start(&mut processor, first_id);

    let playing = pump(&mut processor, WARMUP_BLOCKS);
    assert!(
        (last(&playing) - TEST_PCM_DEFAULT_VALUE).abs() < EXACT,
        "the first track plays at full level ({})",
        last(&playing)
    );

    let second_id = load(&mut control, "b.mp3", constant_quarter);
    block(&mut processor);
    start(&mut processor, second_id);
    let handover = pump(&mut processor, 1);

    assert!(
        (handover[0] - (TEST_PCM_DEFAULT_VALUE + SECOND_LEVEL)).abs() < EXACT,
        "the second track is at full level on its first frame ({})",
        handover[0]
    );
}
