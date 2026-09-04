use kithara_resampler::rubato::RubatoBackend;
use kithara_test_utils::kithara;

use super::{
    super::analyzer::AnalyzerBuilder,
    fixtures::{Artifacts, CH, SR, artifacts, assert_agrees, beat_detector, chunk, sine, spec},
};
use crate::{analyzer::Extent, beat::GridParams, test_pools::pools};

const BUCKETS: usize = 64;

fn analyse(samples: &[f32], blocks: &[(u64, usize, usize)]) -> Artifacts {
    let pools = pools();
    let mut builder = AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
        .with_waveform(BUCKETS)
        .with_beat_detector(beat_detector(), GridParams::default());
    let mut beat = builder.take_detector();
    let mut extent = Extent::default();
    let mut analyzers = builder
        .build(spec().sample_rate, "order-harness".into(), 0)
        .expect("analysis buffers fit the test region");

    for (at, from, to) in blocks {
        let part = samples.get(*from..*to).unwrap_or_default();
        analyzers.push(&chunk(&pools, part, *at), &mut extent, beat.as_mut());
    }

    let frames = u64::try_from(samples.len() / usize::from(CH)).unwrap_or(0);
    artifacts(&analyzers.snapshot(beat.as_mut(), true, Some(frames)))
}

fn blocks(frames: usize, count: usize) -> Vec<(u64, usize, usize)> {
    let per = frames / count;
    (0..count)
        .map(|index| {
            let start = index * per;
            let end = if index + 1 == count {
                frames
            } else {
                start + per
            };
            let at = u64::try_from(start).unwrap_or(0);
            (at, start * usize::from(CH), end * usize::from(CH))
        })
        .collect()
}

#[kithara::test]
fn arrival_order_does_not_change_the_artifacts() {
    let frames = 12 * usize::try_from(SR).unwrap_or(1);
    let samples = sine(frames);
    let ascending = blocks(frames, 12);
    let want = analyse(&samples, &ascending);
    assert!(!want.1.is_empty(), "the harness must find markers at all");

    let shuffled: Vec<_> = [7usize, 0, 11, 3, 9, 1, 5, 10, 2, 8, 4, 6]
        .iter()
        .filter_map(|index| ascending.get(*index).copied())
        .collect();
    assert_agrees(&want, &analyse(&samples, &shuffled), "shuffled");

    let mut duplicated = ascending.clone();
    duplicated.extend(ascending.iter().take(4).copied());
    duplicated.extend(ascending.iter().skip(8).copied());
    assert_agrees(&want, &analyse(&samples, &duplicated), "duplicated");

    // Half-block strides, so every block overlaps its neighbour.
    let per = frames / 12;
    let overlapped: Vec<(u64, usize, usize)> = (0..23)
        .map(|index| {
            let start = (index * per / 2).min(frames);
            let end = (start + per).min(frames);
            (
                u64::try_from(start).unwrap_or(0),
                start * usize::from(CH),
                end * usize::from(CH),
            )
        })
        .filter(|(_, from, to)| to > from)
        .collect();
    assert_agrees(&want, &analyse(&samples, &overlapped), "overlapping");
}
