use std::num::NonZeroU32;

use kithara_audio::{TrackFailureKind, mock::TEST_PCM_DEFAULT_VALUE};
use kithara_command::Outcome;
use kithara_platform::time::Duration;
use kithara_signal::{AudioSpec, SegmentId};
use kithara_test_fixtures::integration_fixtures::constant_half;
use kithara_test_utils::kithara;

use super::{
    TestMixer,
    legacy_fixture::{self, Control, Resource, attach, push, render},
};
use crate::{
    CrossfadeSettings,
    bridge::{DeckPart, Fade, RtMetricsSnapshot, Slot},
    rt::DeckMixerConfig,
    worker::{PcmPacket, packet_tests::PacketRing},
};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: u32 = 128;
const CROSSFADE_SECONDS: f32 = 0.5;
const CROSSFADE_BLOCKS: usize = 8;
const AUDIBLE_FRACTION: f32 = 0.5;

fn crossfade(duration: f32) -> CrossfadeSettings {
    CrossfadeSettings {
        duration,
        ..Default::default()
    }
}

fn block_len() -> usize {
    usize::try_from(BLOCK_FRAMES).expect("block frames fit usize")
}

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"))
}

fn processor() -> (TestMixer, Control) {
    legacy_fixture::processor(DeckMixerConfig::default(), BLOCK_FRAMES, spec().sample_rate)
}

fn faulty_track(src: &str, failed: bool) -> Resource {
    let mut packets = PacketRing::new(spec(), Duration::from_secs(60), 8);
    if failed {
        packets.push(PcmPacket::Failed {
            segment: SegmentId::FIRST,
            failure: TrackFailureKind::Decode {
                kind: kithara_audio::DecodeErrorKind::InvalidData,
            },
        });
    }
    legacy_fixture::from_packets(src, packets)
}

fn healthy_track(input: &'static [u8], src: &str) -> Resource {
    legacy_fixture::resource(input, src, 60.0, spec())
}

fn pump(processor: &mut TestMixer, blocks: usize) -> Vec<f32> {
    let mut left = Vec::new();
    for _ in 0..blocks {
        left = render(processor, block_len()).1;
    }
    left
}

fn peak(rendered: &[f32]) -> f32 {
    rendered
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

fn render_loaded_blocks(resource: Resource, blocks: usize) -> (TestMixer, Control, Vec<f32>) {
    let (mut processor, mut control) = processor();
    let slot = Slot::new(0);
    attach(&mut control, slot, resource, false);
    push(
        &mut control,
        DeckPart::Start {
            slot,
            fade: Fade::Crossfade(crossfade(0.0)),
        },
    );
    pump(&mut processor, 1);
    let rendered = pump(&mut processor, blocks);
    (processor, control, rendered)
}

fn metrics(processor: &TestMixer) -> RtMetricsSnapshot {
    processor.mixer.deck.metrics.snapshot()
}

#[kithara::test]
fn decode_error_is_counted_not_logged() {
    let (processor, _control, _) = render_loaded_blocks(faulty_track("broken.mp3", true), 1);
    assert!(
        metrics(&processor).decode_errors() > 0,
        "a decode error inside process() must land in the counters"
    );
}

#[kithara::test]
fn source_with_nothing_ready_renders_silence_and_counts_an_underrun() {
    let (processor, _control, rendered) =
        render_loaded_blocks(faulty_track("stalled.mp3", false), 4);
    assert!(
        metrics(&processor).underruns() > 0,
        "a zero-filled block short of EOF is an underrun"
    );
    let peak = peak(&rendered);
    assert!(
        peak == 0.0,
        "an underrun must render silence, not stale scratch (peak {peak})"
    );
}

#[kithara::test]
fn a_crossfade_into_a_stalled_track_underruns_instead_of_waiting(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let outgoing = Slot::new(0);
    attach(
        &mut control,
        outgoing,
        healthy_track(constant_half, "outgoing.mp3"),
        false,
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: outgoing,
            fade: Fade::Crossfade(crossfade(0.0)),
        },
    );
    pump(&mut processor, 1);
    let before = peak(&pump(&mut processor, CROSSFADE_BLOCKS));
    assert!(
        (before - TEST_PCM_DEFAULT_VALUE).abs() < f32::EPSILON,
        "the outgoing track plays at full level before the crossfade ({before})"
    );
    let incoming = Slot::new(1);
    attach(
        &mut control,
        incoming,
        faulty_track("incoming.mp3", false),
        false,
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: incoming,
            fade: Fade::Crossfade(crossfade(CROSSFADE_SECONDS)),
        },
    );
    pump(&mut processor, 1);
    let during = peak(&pump(&mut processor, CROSSFADE_BLOCKS));
    assert!(
        metrics(&processor).underruns() > 0,
        "a crossfade into a source with nothing ready must count an underrun"
    );
    assert!(
        during >= before * AUDIBLE_FRACTION,
        "the outgoing track must keep carrying the mix while the incoming one underruns (before {before}, during {during})"
    );
}

#[kithara::test]
fn a_healthy_track_reports_no_trouble(constant_half: &'static [u8]) {
    let (processor, _control, _) = render_loaded_blocks(healthy_track(constant_half, "ok.mp3"), 1);
    assert_eq!(metrics(&processor), RtMetricsSnapshot::default());
}

#[kithara::test]
fn a_seek_on_the_audio_thread_only_syncs_never_blocks() {
    let (mut processor, mut control) = processor();
    let slot = Slot::new(0);
    let segment = SegmentId::FIRST.next();
    let (pcm, mut worker, mut lane, counts) =
        legacy_fixture::prepared_seek(spec(), segment, Duration::from_secs(30));
    push(
        &mut control,
        DeckPart::Attach {
            slot,
            pcm,
            segment: SegmentId::FIRST,
        },
    );
    pump(&mut processor, 1);
    legacy_fixture::applied(&mut control);
    push(&mut control, DeckPart::Adopt { slot, segment });
    pump(&mut processor, 1);
    let receipts = legacy_fixture::applied(&mut control);
    use kithara_worker::Task;
    worker.tick();
    assert_eq!(
        counts.try_iter().count(),
        0,
        "the audio thread must not reach the blocking seek"
    );
    assert_eq!(receipts.iter().filter(|receipt| matches!(receipt.outcome(), Outcome::Applied { .. }))
        .flat_map(|receipt| receipt.batch().commands.iter())
        .filter(|part| matches!(part, DeckPart::Adopt { segment: adopted, .. } if *adopted == segment)).count(), 1,
        "it adopts the target that begin published");
    assert_eq!(
        lane.receipts().count(),
        0,
        "beginning belongs to the control thread, not to this call"
    );
    assert!(
        (processor.track(slot).expect("track loaded").position() - 30.0).abs() < 0.001,
        "the media clock still re-bases on the new position"
    );
}

#[kithara::test]
fn evicting_an_audible_track_is_counted(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    for index in 0..DeckMixerConfig::default().slots().get() {
        let slot = Slot::new(u16::try_from(index).expect("configured slot"));
        attach(
            &mut control,
            slot,
            healthy_track(constant_half, &format!("track-{index}.mp3")),
            false,
        );
        push(
            &mut control,
            DeckPart::Start {
                slot,
                fade: Fade::Crossfade(crossfade(0.0)),
            },
        );
        pump(&mut processor, 1);
    }
    attach(
        &mut control,
        Slot::new(0),
        healthy_track(constant_half, "newcomer.mp3"),
        true,
    );
    pump(&mut processor, 1);
    assert!(
        metrics(&processor).evicted_playing() > 0,
        "dropping an audible track to make room is a real defect and must stay visible"
    );
}

#[kithara::test]
fn a_block_larger_than_declared_is_clamped_not_grown(constant_half: &'static [u8]) {
    let (mut processor, mut control) = processor();
    let slot = Slot::new(0);
    attach(
        &mut control,
        slot,
        healthy_track(constant_half, "ok.mp3"),
        false,
    );
    push(
        &mut control,
        DeckPart::Start {
            slot,
            fade: Fade::Crossfade(crossfade(0.0)),
        },
    );
    pump(&mut processor, 1);
    let (rendered, out_l, _) = render(&mut processor, block_len() * 2);
    assert!(rendered, "the declared part of the block still renders");
    assert!(
        out_l[..block_len()].iter().all(|sample| sample.is_finite()),
        "frames up to max_block_frames are written"
    );
    assert!(
        out_l[block_len()..].iter().all(|sample| *sample == 0.0),
        "frames beyond the declared block are silence, since the host is told the whole block is written"
    );
}
