//! The real detector over a real track, driven the way the app drives it.
//! Scaled fixtures agree on artifacts without reaching the regime where the
//! detector is the slow consumer, so this reads a whole track instead.
//!
//! `KITHARA_PROBE_PCM` names raw stereo f32 at `SR`; without it there is
//! nothing to read and the test stands down. No gate lane builds `beat-nn`,
//! so this runs when someone asks for it:
//!
//! ```text
//! ffmpeg -i assets/track.mp3 -f f32le -ac 2 -ar 44100 /tmp/track.f32le
//! KITHARA_PROBE_PCM=/tmp/track.f32le cargo nextest run -p kithara-analysis \
//!   --features analysis-beat,analysis-waveform,beat-nn --cargo-profile test-release \
//!   -E 'test(/a_real_track_reaches_its_end_whole/)'
//! ```

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_platform::{
    CancelToken,
    sync::mpsc,
    time::{Duration, Instant},
    tokio::sync::watch,
};
use kithara_resampler::rubato::RubatoBackend;
use kithara_test_utils::kithara;
use kithara_worker::TickResult;
use num_traits::cast::ToPrimitive;

use super::{
    super::{analyzer::AnalyzerBuilder, producer::ring, worker::Job},
    fixtures::{SR, spec},
    node::NodeHarness,
    track::Track,
};
use crate::AnalysisProgress;

const CHUNK_FRAMES: u64 = 8820;

#[kithara::test(native, flash(false))]
fn a_real_track_reaches_its_end_whole() {
    let Ok(path) = std::env::var("KITHARA_PROBE_PCM") else {
        return;
    };
    let pools = crate::test_pools::pools();
    let bytes = std::fs::read(&path).expect("probe pcm");
    let pcm: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let track = Track::new(pools.clone(), spec(), CHUNK_FRAMES, pcm);
    let frames = track.frames();
    let rate = spec().sample_rate;

    let (jobs, receiver) = mpsc::channel();
    let (tx, results) = watch::channel::<Option<AnalysisProgress>>(None);
    let (_writer, ingest) = ring::open_for(rate);
    jobs.send(Job {
        token: "probe".into(),
        reader: Box::new(track),
        tx,
        rate,
        ingest,
        cancel: CancelToken::root(),
        resume: None,
    })
    .expect("node accepts the job");

    let mut node = NodeHarness::with_settings(
        AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
            .with_waveform(64)
            .with_beat(),
        receiver,
        NonZeroU32::new(16).expect("nonzero"),
        NonZeroUsize::new(8).expect("nonzero"),
        NonZeroU32::new(5).expect("nonzero"),
    );

    let started = Instant::now();
    let mut ticks = 0u64;
    while results.has_changed().is_ok() {
        if node.tick() == TickResult::Backpressured {
            kithara_platform::thread::yield_now();
        }
        ticks += 1;
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "the pass did not finish in ten minutes after {ticks} ticks"
        );
    }
    let elapsed = started.elapsed();

    let progress = results.borrow().clone().expect("the pass publishes");
    let analysis = progress.analysis();
    let beat = analysis.beat().expect("the beat slot is filled");
    let lost: u64 = beat.unanalysed().iter().map(|range| range.frames()).sum();
    eprintln!(
        "{:.1}s track, wall {:.1}s, {ticks} ticks, covered {} of {frames}, lost {lost}, bpm {:.4}, beats {}",
        frames.to_f64().unwrap_or(0.0) / f64::from(SR),
        elapsed.as_secs_f64(),
        analysis.coverage().frames(),
        beat.artifact().bpm(),
        beat.artifact().beats().len(),
    );
    assert_eq!(lost, 0, "the real track lost {lost} frames");
}
