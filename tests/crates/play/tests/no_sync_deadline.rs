#![cfg(not(target_arch = "wasm32"))]

use std::{hint::black_box, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::node::ProcBuffers;
use kithara::{
    events::TrackId,
    platform::{
        sync::Arc,
        time::{Duration, Instant},
    },
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
    signal::AudioSpec,
};
use kithara_integration_tests::{audio_mock::TestPcmReader, offline::peak};
use kithara_test_fixtures::integration_fixtures::deadline_tracks;
use ringbuf::traits::Producer;

use crate::bufpool_ext::pools;

struct Consts;

impl Consts {
    const BLOCK_FRAMES: [u32; 4] = [128, 256, 512, 1_024];
    const CHANNELS: u16 = 2;
    /// The slack over a perfectly proportional mix. Measured growth is below
    /// `1.0` at every cell, because more tracks amortise the per-callback work;
    /// a mix that rescans what it already mixed lands at `2.5` and above.
    const MAX_PER_TRACK_GROWTH: f64 = 1.25;
    const MEASURED_BLOCKS: usize = 4_096;
    const MIXED_TRACK_COUNTS: [usize; 2] = [2, 4];
    /// How many adjacent `alone`/`mixed` pairs the ratio is taken as the best
    /// of. The runner runs this code in two regimes about 1.6x apart -- across
    /// 1 148 stress samples `alone` at 1 024 frames held 3.84 us through p75
    /// and 6.04 us by p95 -- and a whole 4 096-block measurement sits inside
    /// one of them. A single pair straddling that edge reads 1.6 or 0.6
    /// whatever the mix costs, which breached 0.96 % of cells and so 7.4 % of
    /// runs; three pairs put that at one in a million.
    const PAIRED_SAMPLES: usize = 3;
    const SAMPLE_RATE: u32 = 48_000;
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
) -> Duration {
    let frames = out_l.len();
    let is_playing = control.playback.playing.load(Ordering::SeqCst);
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [out_l, out_r];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };

    let start = Instant::now();
    processor.drain_commands();
    processor.cleanup_finished_tracks();
    let outcome = processor.render_audio(&mut buffers, frames, is_playing);
    let elapsed = start.elapsed();

    black_box(outcome);
    elapsed
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

fn measure(block_frames: u32, tracks: usize, deadline_tracks: [&'static [u8]; 4]) -> Duration {
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
        black_box(render_block(
            &mut processor,
            &control,
            &mut out_l,
            &mut out_r,
        ));
    }
    let warm_peak = peak(&out_l).max(peak(&out_r));
    assert!(
        warm_peak > 0.0,
        "deadline cell must reach audible PCM before timing ({block_frames} frames, {tracks} track(s))"
    );
    assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
    assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);

    let mut cheapest = Duration::MAX;
    for _ in 0..Consts::MEASURED_BLOCKS {
        cheapest = cheapest.min(render_block(
            &mut processor,
            &control,
            &mut out_l,
            &mut out_r,
        ));
        assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
        assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);
        black_box((&out_l, &out_r));
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
        "measured callbacks must finish on audible PCM, not silence"
    );
    cheapest
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn per_track_growth(mixed: Duration, alone: Duration, tracks: usize) -> f64 {
    let count = u32::try_from(tracks).expect("track count fits u32");
    mixed.as_secs_f64() / (alone.as_secs_f64() * f64::from(count))
}

/// Mixing a track costs the same however many tracks play.
///
/// The hot path scans the active tracks again inside its per-track loop, so a
/// stray per-frame step there turns the mix quadratic and the audio deadline
/// stops holding as the queue fills. A clock cannot say so by itself here: a
/// whole callback costs 0.03-0.30 % of its period, and judging its tail read the
/// runner's queue instead -- p99 was 24-38 us idle, 1 695 us under 8x
/// oversubscription and 14 062 us on the stress runner, all on this code. What a
/// block costs per track is the code talking, so the cheapest of 4 096 blocks
/// carries the verdict, read as the best of [`Consts::PAIRED_SAMPLES`] adjacent
/// pairs. Absolute block cost belongs to the `rt_block_budget` bench, which
/// times this processor and asks the clock for no verdict.
#[kithara::test(native, serial, flash(false))]
fn mixing_a_track_costs_the_same_however_many_tracks_play(deadline_tracks: [&'static [u8]; 4]) {
    for block_frames in Consts::BLOCK_FRAMES {
        let mut best: [Option<(Duration, Duration)>; Consts::MIXED_TRACK_COUNTS.len()] =
            [None; Consts::MIXED_TRACK_COUNTS.len()];

        for _ in 0..Consts::PAIRED_SAMPLES {
            let alone = measure(block_frames, 1, deadline_tracks);
            for (slot, tracks) in best.iter_mut().zip(Consts::MIXED_TRACK_COUNTS) {
                let mixed = measure(block_frames, tracks, deadline_tracks);
                let improves = slot.is_none_or(|(kept_alone, kept_mixed)| {
                    per_track_growth(mixed, alone, tracks)
                        < per_track_growth(kept_mixed, kept_alone, tracks)
                });
                if improves {
                    *slot = Some((alone, mixed));
                }
            }
        }

        for (slot, tracks) in best.into_iter().zip(Consts::MIXED_TRACK_COUNTS) {
            let (alone, mixed) = slot.expect("every mixed cell is measured at least once");
            let count = u32::try_from(tracks).expect("track count fits u32");
            let growth = per_track_growth(mixed, alone, tracks);

            println!(
                "no-SYNC mix cost: frames={block_frames:>4} tracks={tracks} \
                 alone={:>8.2} us mixed={:>8.2} us per track={:>8.2} us growth={growth:>5.3}",
                micros(alone),
                micros(mixed),
                micros(mixed / count),
            );
            assert!(
                growth <= Consts::MAX_PER_TRACK_GROWTH,
                "mixing {tracks} tracks at {block_frames} frames costs {:.2} us, \
                 {:.2} us per track against {:.2} us for a single track; a mix that \
                 stays proportional to its tracks grows at most {:.2}x per track, \
                 this one grows {growth:.2}x",
                micros(mixed),
                micros(mixed / count),
                micros(alone),
                Consts::MAX_PER_TRACK_GROWTH,
            );
        }
    }
}
