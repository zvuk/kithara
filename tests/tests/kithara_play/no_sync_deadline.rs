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
use ringbuf::traits::Producer;

use crate::bufpool_ext::pools;

struct Consts;

impl Consts {
    const BLOCK_FRAMES: [u32; 4] = [128, 256, 512, 1_024];
    const CHANNELS: u16 = 2;
    const MEASURED_BLOCKS: usize = 4_096;
    const SAMPLE_RATE: u32 = 48_000;
    const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    const TRACK_SECONDS: f64 = 300.0;
    const WARMUP_BLOCKS: usize = 512;
}

#[derive(Debug)]
struct CellTiming {
    block_frames: u32,
    max: Duration,
    p50: Duration,
    p99: Duration,
    period: Duration,
    tracks: usize,
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
    (PlayerNodeProcessor::new(inputs, shape, &pools()), control)
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
            TestPcmReader::with_value(spec(), Consts::TRACK_SECONDS, value),
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

fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let rank = (sorted.len() * pct).div_ceil(100);
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn measure(block_frames: u32, tracks: usize) -> CellTiming {
    let (mut processor, mut control) = processor(block_frames);
    let expected_sample = load_tracks(&mut processor, &mut control, tracks);
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

    let mut durations = Vec::with_capacity(Consts::MEASURED_BLOCKS);
    for _ in 0..Consts::MEASURED_BLOCKS {
        durations.push(render_block(
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
    durations.sort_unstable();
    CellTiming {
        block_frames,
        max: percentile(&durations, 100),
        p50: percentile(&durations, 50),
        p99: percentile(&durations, 99),
        period: Duration::from_secs_f64(f64::from(block_frames) / f64::from(Consts::SAMPLE_RATE)),
        tracks,
    }
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn period_share(duration: Duration, period: Duration) -> f64 {
    duration.as_secs_f64() / period.as_secs_f64() * 100.0
}

#[kithara::test(native, serial, flash(false))]
fn no_sync_player_render_hot_path_p99_stays_below_half_period() {
    let mut timings = Vec::with_capacity(Consts::BLOCK_FRAMES.len() * Consts::TRACK_COUNTS.len());

    for block_frames in Consts::BLOCK_FRAMES {
        for tracks in Consts::TRACK_COUNTS {
            timings.push(measure(block_frames, tracks));
        }
    }

    for timing in &timings {
        println!(
            "no-SYNC render hot path: frames={:>4} tracks={} p50={:>8.2} us ({:>6.2}%) \
             p99={:>8.2} us ({:>6.2}%) max={:>8.2} us ({:>6.2}%)",
            timing.block_frames,
            timing.tracks,
            micros(timing.p50),
            period_share(timing.p50, timing.period),
            micros(timing.p99),
            period_share(timing.p99, timing.period),
            micros(timing.max),
            period_share(timing.max, timing.period),
        );
    }

    for timing in timings {
        let half_period = timing.period / 2;
        assert!(
            timing.p99 < half_period,
            "no-SYNC player render p99 must stay below 50% of its period: frames={}, tracks={}, \
             p99={:.2} us ({:.2}%), half-period={:.2} us; max={:.2} us is diagnostic only",
            timing.block_frames,
            timing.tracks,
            micros(timing.p99),
            period_share(timing.p99, timing.period),
            micros(half_period),
            micros(timing.max),
        );
    }
}
