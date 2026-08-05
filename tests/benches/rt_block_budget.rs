#![forbid(unsafe_code)]

use std::{hint::black_box, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::node::ProcBuffers;
use kithara::{
    bufpool::PcmPool,
    decode::PcmSpec,
    platform::{
        sync::Arc,
        time::{Duration, Instant},
    },
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
};
use kithara_integration_tests::audio_mock::TestPcmReader;
use ringbuf::traits::Producer;

struct Consts;

impl Consts {
    const BLOCK_FRAMES: u32 = 128;
    const CHANNELS: u16 = 2;
    const MEASURED_BLOCKS: usize = 20_000;
    const SAMPLE_RATE: u32 = 48_000;
    /// One track, a crossfade pair, and a full arena (`MAX_TRACKS`).
    const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    /// Longer than `MEASURED_BLOCKS + WARMUP_BLOCKS` of audio, so no track
    /// reaches EOF while it is being measured.
    const TRACK_SECONDS: f64 = 600.0;
    const WARMUP_BLOCKS: usize = 2_000;
}

/// One measured lane: sorted per-block durations plus the evidence that the
/// blocks carried a real mix.
struct Measurement {
    durations: Vec<Duration>,
    peak: f32,
    tracks: usize,
}

fn non_zero(value: u32, label: &str) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("bench {label} must be non-zero"))
}

fn block_frames() -> usize {
    usize::try_from(Consts::BLOCK_FRAMES)
        .unwrap_or_else(|_| panic!("bench block frames exceed usize"))
}

fn block_budget() -> Duration {
    Duration::from_secs_f64(f64::from(Consts::BLOCK_FRAMES) / f64::from(Consts::SAMPLE_RATE))
}

fn spec() -> PcmSpec {
    PcmSpec::new(
        Consts::CHANNELS,
        non_zero(Consts::SAMPLE_RATE, "sample rate"),
    )
}

/// Build the processor the way the production stream does: `slot_channels`
/// for the ring pair, `StreamShape` for the rate and block the host declared.
fn processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: non_zero(Consts::SAMPLE_RATE, "sample rate"),
        max_block_frames: non_zero(Consts::BLOCK_FRAMES, "block frames"),
    };
    (
        PlayerNodeProcessor::new(inputs, shape, &PcmPool::default()),
        control,
    )
}

fn send(control: &mut SlotControl, cmd: PlayerCmd) {
    if control.cmd_tx.try_push(cmd).is_err() {
        panic!("bench command ring full");
    }
}

/// Ship `count` ready sources across and start them, so every measured block
/// mixes `count` tracks.
fn load_tracks(processor: &mut PlayerNodeProcessor, control: &mut SlotControl, count: usize) {
    let pool = PcmPool::default();
    let sources: Vec<Arc<str>> = (0..count)
        .map(|idx| Arc::from(format!("bench-track-{idx}").as_str()))
        .collect();

    for src in &sources {
        let resource = Resource::from_reader(
            TestPcmReader::new(spec(), Consts::TRACK_SECONDS),
            Some(Arc::clone(src)),
        );
        send(
            control,
            PlayerCmd::LoadTrack {
                resource: Box::new(PlayerResource::new(resource, Arc::clone(src), &pool)),
                item_id: None,
            },
        );
    }
    send(control, PlayerCmd::SetPaused(false));
    processor.drain_commands();

    for src in &sources {
        match processor.track_mut(src) {
            Some(track) => track.play(),
            None => panic!("bench track {src} did not reach the arena"),
        }
    }
}

/// Time the three calls `process()` makes around the block. The block counter
/// it bumps and the playing flag it reads stay outside the timer.
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

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
}

fn measure(tracks: usize) -> Measurement {
    let (mut processor, mut control) = processor();
    load_tracks(&mut processor, &mut control, tracks);

    let frames = block_frames();
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];

    for _ in 0..Consts::WARMUP_BLOCKS {
        render_block(&mut processor, &control, &mut out_l, &mut out_r);
    }

    let before = control.playback.metrics().snapshot();
    let mut durations = Vec::with_capacity(Consts::MEASURED_BLOCKS);
    let mut peak = 0.0_f32;
    for _ in 0..Consts::MEASURED_BLOCKS {
        durations.push(render_block(
            &mut processor,
            &control,
            &mut out_l,
            &mut out_r,
        ));
        peak = peak.max(peak_of(&out_l));
        black_box(&out_l);
    }
    let after = control.playback.metrics().snapshot();

    let live = processor.track_count();
    assert_eq!(
        live, tracks,
        "bench measured {live} track(s), not the {tracks} it loaded"
    );
    assert_eq!(
        after.underruns(),
        before.underruns(),
        "an underrun renders zero-fill, so the timing would be of silence"
    );
    assert_eq!(
        after.decode_errors(),
        before.decode_errors(),
        "a decode error skips the mix, so the timing would not be of a mix"
    );
    assert!(peak > 0.0, "measured blocks must carry audio, not silence");

    durations.sort_unstable();
    Measurement {
        durations,
        peak,
        tracks,
    }
}

impl Measurement {
    fn percentile(&self, pct: usize) -> Duration {
        let rank = (self.durations.len() * pct).div_ceil(100);
        let idx = rank.saturating_sub(1).min(self.durations.len() - 1);
        self.durations[idx]
    }

    fn max(&self) -> Duration {
        self.percentile(100)
    }
}

fn cell(elapsed: Duration, budget: Duration) -> String {
    let micros = elapsed.as_secs_f64() * 1e6;
    let share = elapsed.as_secs_f64() / budget.as_secs_f64() * 1e2;
    format!("{micros:>7.2} us / {share:>5.2}%")
}

fn main() {
    let budget = block_budget();
    println!(
        "PlayerNodeProcessor block budget: {:.3} ms ({} frames @ {} Hz, {} ch)",
        budget.as_secs_f64() * 1e3,
        Consts::BLOCK_FRAMES,
        Consts::SAMPLE_RATE,
        Consts::CHANNELS,
    );
    println!(
        "{} blocks measured per lane after {} warm-up blocks",
        Consts::MEASURED_BLOCKS,
        Consts::WARMUP_BLOCKS,
    );
    println!(
        "{:>7}  {:>22}  {:>22}  {:>22}",
        "tracks", "p50", "p99", "max"
    );

    for count in Consts::TRACK_COUNTS {
        let measurement = measure(count);
        println!(
            "{:>7}  {:>22}  {:>22}  {:>22}",
            measurement.tracks,
            cell(measurement.percentile(50), budget),
            cell(measurement.percentile(99), budget),
            cell(measurement.max(), budget),
        );
        black_box(measurement.peak);
    }
}
