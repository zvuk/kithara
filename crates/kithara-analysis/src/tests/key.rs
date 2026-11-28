use std::{
    f64::consts::PI,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ops::Range,
};

use kithara_key::{KeyEstimate, Mode, MusicalKey, PitchClass};
use kithara_platform::{CancelToken, time::Duration, tokio::sync::watch};
use kithara_resampler::NoResamplerBackend;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::{
    bufpool::{Pools, TestPools, pools, pools_with_budget, sample_buffer},
    kithara,
};
use kithara_worker::TickResult;
use num_traits::cast::ToPrimitive;

use super::fixtures::{self, FakeReader, idle_ingest, spec};
use crate::{
    AnalysisFile, AnalysisFileSpec, AnalysisProgress, TrackAnalysis,
    analyzer::{AnalysisDemand, AnalyzerBuilder, Extent, TrackAnalyzers},
    archive::tests::apply,
    worker::{AnalysisTask, Job},
};

fn rate() -> NonZeroU32 {
    NonZeroU32::new(fixtures::consts::FIXTURES_SR).expect("the fixture rate is non-zero")
}

/// Three seconds each of A minor, D minor, E minor and A minor, every note
/// with four partials: a progression whose key is A minor.
fn a_minor() -> Vec<f32> {
    const CHORDS: [[i32; 3]; 4] = [[57, 60, 64], [62, 65, 69], [64, 67, 71], [57, 60, 64]];
    const PARTIALS: [f64; 4] = [1.0, 0.5, 0.33, 0.25];
    const GAIN: f64 = 0.1;
    const CHORD_SECONDS: u32 = 3;

    let rate = f64::from(fixtures::consts::FIXTURES_SR);
    let chord_frames = fixtures::consts::FIXTURES_SR * CHORD_SECONDS;
    let mut out = Vec::new();
    let mut n = 0.0;
    for chord in CHORDS {
        for _ in 0..chord_frames {
            let mut sample = 0.0;
            for note in chord {
                let freq = 440.0 * 2.0f64.powf(f64::from(note - 69) / 12.0);
                for (harmonic, amplitude) in (1u8..).zip(PARTIALS) {
                    sample += amplitude * (2.0 * PI * freq * f64::from(harmonic) * n / rate).sin();
                }
            }
            out.push((GAIN * sample).to_f32().unwrap_or(0.0));
            n += 1.0;
        }
    }
    out
}

fn pass(pools: &Pools) -> TrackAnalyzers<NoResamplerBackend, TestPools> {
    AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
        .with_key()
        .build(rate(), "key-track".into(), 0, AnalysisDemand::ALL)
        .expect("the key estimator fits the test region")
}

fn offer(
    pools: &Pools,
    pass: &mut TrackAnalyzers<NoResamplerBackend, TestPools>,
    pcm: &[f32],
    range: Range<u64>,
) {
    let at = range.start.to_usize().unwrap_or(usize::MAX);
    let end = range.end.to_usize().unwrap_or(usize::MAX);
    let samples = &pcm[at..end];
    let chunk = AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec::new(1, rate()),
            frames: u32::try_from(samples.len()).unwrap_or(0),
            frame_offset: range.start,
            media_origin: Some(Duration::ZERO),
            visible_origin: Some(Duration::ZERO),
            ..Default::default()
        },
        sample_buffer(pools, samples),
    );
    pass.push(&chunk, &mut Extent::default(), None)
        .expect("key estimation succeeds");
}

fn chunks(frames: u64) -> Vec<Range<u64>> {
    (0..frames.div_ceil(fixtures::consts::CHUNK))
        .map(|index| {
            index * fixtures::consts::CHUNK..((index + 1) * fixtures::consts::CHUNK).min(frames)
        })
        .collect()
}

fn settled(pass: &mut TrackAnalyzers<NoResamplerBackend, TestPools>, frames: u64) -> TrackAnalysis {
    pass.settle();
    pass.snapshot(None, true, Some(frames))
        .expect("key estimation succeeds")
}

fn key_of(analysis: &TrackAnalysis) -> KeyEstimate {
    analysis.key().expect("a tonal track states a key")
}

#[kithara::test(native, flash(false))]
fn out_of_order_coverage_settles_on_the_in_order_key() {
    let pcm = a_minor();
    let frames = u64::try_from(pcm.len()).unwrap_or(0);
    let ranges = chunks(frames);
    let pools = pools();

    let mut in_order = pass(&pools);
    for range in &ranges {
        offer(&pools, &mut in_order, &pcm, range.clone());
    }
    let in_order = key_of(&settled(&mut in_order, frames));
    assert_eq!(
        in_order.key(),
        MusicalKey {
            tonic: PitchClass::A,
            mode: Mode::Minor,
        }
    );

    let mut shuffled = pass(&pools);
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&index| (index * 7919) % ranges.len());
    let (first, rest) = order.split_at(order.len() / 2);
    for &index in first.iter().filter(|&&index| index % 5 != 2) {
        offer(&pools, &mut shuffled, &pcm, ranges[index].clone());
    }
    let mid = shuffled
        .snapshot(None, false, Some(frames))
        .expect("key estimation succeeds");
    assert!(!mid.is_settled(), "half the track is not the whole of it");
    assert!(mid.key().is_some(), "the covered half states a key");

    for &index in first.iter().filter(|&&index| index % 5 == 2).chain(rest) {
        offer(&pools, &mut shuffled, &pcm, ranges[index].clone());
    }
    let straddle = fixtures::consts::CHUNK / 2..fixtures::consts::CHUNK * 5 / 2;
    offer(&pools, &mut shuffled, &pcm, straddle);
    offer(&pools, &mut shuffled, &pcm, ranges[3].clone());
    let shuffled = key_of(&settled(&mut shuffled, frames));

    assert_eq!(shuffled.key(), in_order.key());
    assert!(
        (shuffled.strength() - in_order.strength()).abs() <= 1e-6,
        "order does not move the strength: {} vs {}",
        shuffled.strength(),
        in_order.strength()
    );
}

fn store(progress: &AnalysisProgress, chunk_frames: NonZeroU64) -> Vec<u8> {
    let spec = AnalysisFileSpec::for_analysis(progress.analysis(), chunk_frames)
        .expect("the pass knows its extent");
    let update = AnalysisFile::create(&spec, progress).expect("the progress is stored");
    let mut bytes = Vec::new();
    apply(&mut bytes, &update);
    bytes
}

#[kithara::test(native, flash(false))]
fn a_stored_pass_resumes_to_the_uninterrupted_key() {
    let pcm = a_minor();
    let frames = u64::try_from(pcm.len()).unwrap_or(0);
    let ranges = chunks(frames);
    let (head, tail) = ranges.split_at(ranges.len() / 2);
    let chunk_frames = NonZeroU64::new(fixtures::consts::CHUNK).expect("the chunk is non-zero");
    let pools = pools();
    let builder = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone()).with_key();

    let mut whole = pass(&pools);
    for range in &ranges {
        offer(&pools, &mut whole, &pcm, range.clone());
    }
    let whole = settled(&mut whole, frames);

    let mut first = pass(&pools);
    for range in head {
        offer(&pools, &mut first, &pcm, range.clone());
    }
    let partial = first
        .progress(None, false, chunk_frames, Some(frames))
        .expect("key estimation succeeds");
    assert!(partial.analysis().key().is_some(), "the head states a key");
    let stored = AnalysisFile::parse(&store(&partial, chunk_frames), &builder.fingerprint())
        .expect("the stored pass reads back under the active fingerprint");
    let mut resumed = builder
        .restore(stored.latest(), chunk_frames)
        .expect("the stored pass resumes");
    drop(first);
    for range in tail {
        offer(&pools, &mut resumed, &pcm, range.clone());
    }
    let resumed = settled(&mut resumed, frames);
    assert_eq!(resumed.key(), Some(key_of(&whole)));

    let done = AnalysisProgress::try_from(resumed.clone()).expect("a settled pass is final");
    let read_back = AnalysisFile::parse(&store(&done, chunk_frames), &builder.fingerprint())
        .expect("the settled pass reads back");
    assert_eq!(read_back.latest().analysis().key(), resumed.key());
}

#[kithara::test(native, flash(false))]
fn a_pass_whose_key_estimator_fails_ends_unsettled_without_publishing() {
    /// Room for the analyzers, short of the buffer the key estimator
    /// folds one long stereo chunk into.
    const BUDGET: usize = 1 << 20;
    const FRAMES: usize = 1 << 20;
    const TICK_LIMIT: u32 = 64;

    let builder =
        AnalyzerBuilder::<NoResamplerBackend, _>::new(pools_with_budget(BUDGET)).with_key();
    let silence = vec![0.0; usize::from(fixtures::consts::CH) * FRAMES];
    let (tx, results) = watch::channel::<Option<AnalysisProgress>>(None);
    let job = Job {
        tx,
        demand: AnalysisDemand::ALL,
        token: "key-track".into(),
        reader: Box::new(FakeReader::chunked(&pools(), &silence, 1)),
        cancel: CancelToken::root(),
        rate: spec().sample_rate,
        resume: None,
        report: None,
        ingest: idle_ingest(),
        revision: 0,
    };
    let mut task = AnalysisTask::new(
        job,
        &builder,
        NonZeroU32::MIN,
        NonZeroUsize::MIN,
        NonZeroU32::MIN,
    )
    .expect("a fresh pass needs no resume");

    let mut ticks = 0;
    while task.tick(&builder, None) != TickResult::Done {
        ticks += 1;
        assert!(ticks < TICK_LIMIT, "the pass ends");
    }
    assert!(
        results.borrow().is_none(),
        "a pass whose key estimator failed publishes nothing: {:?}",
        results
            .borrow()
            .as_ref()
            .map(|progress| progress.analysis().is_settled())
    );
}
