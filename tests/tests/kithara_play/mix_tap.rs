#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    self,
    decode::PcmSpec,
    play::{Cmd, PlayError, Resource, SessionDispatcher, SessionError},
};
use kithara_integration_tests::offline::resource_from_reader;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const TRACK_SECS: f64 = 0.1;
/// 10_240 frames: past the budget the smoke test allows the decode worker for
/// the same 100 ms fixture, so the render reaches the silence after the track.
const BLOCKS: usize = 20;
const ROOMY_CAPACITY: usize = 65_536;

fn make_resource() -> Resource {
    let spec = PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate"));
    resource_from_reader(kithara_integration_tests::audio_mock::TestPcmReader::new(
        spec, TRACK_SECS,
    ))
}

/// A harness with one mock track playing, rendered far enough that the
/// session graph (and its limiter) is live.
fn playing_harness() -> OfflinePlayerHarness {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    );
    harness.player().insert(make_resource(), None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
    harness.render(BLOCK_FRAMES);
    let _ = harness.tick_and_drain();
    harness
}

fn render_blocks(harness: &OfflinePlayerHarness, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * 2);
    for _ in 0..blocks {
        rendered.extend_from_slice(&harness.render(BLOCK_FRAMES));
        let _ = harness.tick_and_drain();
    }
    rendered
}

/// Topology anchor: the tap is a stereo-in / zero-out sink hanging off the
/// limiter *beside* `graph_out`. Firewheel must schedule it every block.
#[kithara::test]
fn zero_output_sink_is_processed_beside_graph_out() {
    let harness = playing_harness();
    let mut tap = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("enable mix tap");

    let rendered = render_blocks(&harness, BLOCKS);
    let tapped = tap.drain();

    assert_eq!(
        rendered.len(),
        BLOCKS * BLOCK_FRAMES * 2,
        "graph_out keeps producing while the sink is attached"
    );
    assert_eq!(
        tapped.len(),
        rendered.len(),
        "the zero-output sink must process every block"
    );
    assert_eq!(tap.drops(), 0);
}

#[kithara::test]
fn mix_tap_matches_graph_out_bit_exactly() {
    let harness = playing_harness();
    let mut tap = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("enable mix tap");

    let rendered = render_blocks(&harness, BLOCKS);
    let tapped = tap.drain();

    assert!(
        rendered.iter().any(|sample| sample.abs() > 0.0),
        "the fixture must render audible signal"
    );
    assert!(
        rendered.iter().rev().take(BLOCK_FRAMES).all(|s| *s == 0.0),
        "the render must reach past the track into silence"
    );
    assert_eq!(tapped, rendered, "mix tap must be bit-exact with graph_out");
}

#[kithara::test]
fn a_tap_armed_before_playback_reaches_the_graph_it_waits_for() {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    );
    let mut tap = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("arm the mix tap before a session output exists");
    assert!(tap.drain().is_empty(), "an idle session feeds nothing");

    harness.player().insert(make_resource(), None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");

    let rendered = render_blocks(&harness, BLOCKS);
    assert_eq!(
        tap.drain(),
        rendered,
        "the waiting tap catches the mix from the first block the graph renders"
    );
}

#[kithara::test]
fn the_tap_keeps_feeding_across_a_device_route_restart() {
    let harness = playing_harness();
    let mut tap = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("enable mix tap");
    render_blocks(&harness, 2);
    assert!(!tap.drain().is_empty(), "the feed runs before the restart");

    harness
        .session()
        .exec_ok(Cmd::InvalidateAudioRoute {
            reason: String::from("mix tap route restart"),
        })
        .expect("invalidate the audio route");

    let rendered = render_blocks(&harness, 4);
    assert!(
        tap.writer_alive(),
        "a stream restart at the same rate keeps the producer"
    );
    assert_eq!(
        tap.drain(),
        rendered,
        "the same producer carries the mix rendered after the restart"
    );
}

#[kithara::test]
fn mix_tap_overflow_drops_exactly_the_capacity_deficit() {
    const TIGHT_CAPACITY: usize = 2_048;

    let harness = playing_harness();
    let mut tap = harness
        .session()
        .enable_mix_tap(TIGHT_CAPACITY)
        .expect("enable mix tap");

    let rendered = render_blocks(&harness, BLOCKS);
    let tapped = tap.drain();

    assert_eq!(tapped.len(), TIGHT_CAPACITY, "the ring fills to capacity");
    assert_eq!(
        tapped,
        rendered[..TIGHT_CAPACITY],
        "what the ring holds is a contiguous prefix of the mix"
    );
    assert_eq!(
        tap.drops(),
        (rendered.len() - TIGHT_CAPACITY) as u64,
        "every sample the ring had no room for is counted"
    );
}

#[kithara::test]
fn second_enable_is_rejected_and_disable_releases_the_writer() {
    let harness = playing_harness();
    let tap = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("enable mix tap");

    match harness.session().enable_mix_tap(ROOMY_CAPACITY).map(|_| ()) {
        Err(PlayError::Session(SessionError::MixTapActive)) => {}
        other => panic!("a second consumer must be rejected, got {other:?}"),
    }
    assert!(tap.writer_alive());

    harness
        .session()
        .exec_ok(Cmd::DisableMixTap)
        .expect("disable mix tap");
    render_blocks(&harness, 2);
    assert!(
        !tap.writer_alive(),
        "disabling the tap must drop the producer so the consumer sees the feed end"
    );

    let re_enabled = harness
        .session()
        .enable_mix_tap(ROOMY_CAPACITY)
        .expect("re-enable mix tap after disable");
    assert!(re_enabled.writer_alive());
}
