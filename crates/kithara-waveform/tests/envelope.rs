#![cfg(all(feature = "dsp", not(target_arch = "wasm32")))]
#![forbid(unsafe_code)]

use kithara_test_fixtures::analysis_fixtures::{
    analysis_silence, waveform_mix, waveform_peak_clicks, waveform_peak_tones,
};
use kithara_test_utils::{bufpool::pools, kithara};
use kithara_waveform::{
    AnalysisParams, Bucket, MAX_BUCKETS, Waveform, WaveformAnalyzer, WaveformResume,
};
use num_traits::ToPrimitive;

const RATE: u32 = 44_100;

fn analyse(pcm: &[f32], channels: usize, buckets: usize) -> Waveform {
    let pools = pools();
    let mut pass =
        WaveformAnalyzer::new(RATE, AnalysisParams::default(), &pools).expect("valid setup");
    pass.push(&pools, pcm, channels, 0).expect("finite source");
    pass.snapshot(
        buckets,
        Some(u64::try_from(pcm.len() / channels).expect("bounded fixture")),
    )
    .expect("finite windows")
}

fn height(bucket: Bucket) -> f32 {
    bucket
        .left()
        .into_iter()
        .chain(bucket.right())
        .fold(0.0, f32::max)
}

#[kithara::test]
fn clicks_have_no_early_columns_and_five_ms_clicks_stay_separate(waveform_peak_clicks: Vec<f32>) {
    let at = 88_211;
    let next = at + 221;
    let pcm = waveform_peak_clicks;
    let wave = analyse(&pcm, 1, MAX_BUCKETS);
    let heights: Vec<_> = wave.buckets().map(height).collect();
    assert!(heights[..at / 64].iter().all(|height| *height == 0.0));
    let first = heights[at / 64].max(heights[at / 64 + 1]);
    let second = heights[next / 64].max(heights[next / 64 + 1]);
    assert!(first > 0.0 && second > 0.0);
    assert!(
        heights[at / 64 + 2..next / 64]
            .iter()
            .any(|height| *height < first.min(second))
    );
    assert!(
        analyse(&pcm, 1, 10)
            .buckets()
            .map(height)
            .any(|height| height >= first.max(second))
    );
}

#[kithara::test]
fn burst_and_held_tone_have_equal_peaks(waveform_peak_tones: [Vec<f32>; 5]) {
    let held = &waveform_peak_tones[1];
    let burst = &held[..882];
    let peak = |wave: Waveform| {
        wave.buckets()
            .map(|bucket| bucket.left()[1])
            .fold(0.0, f32::max)
    };
    assert!(
        (peak(analyse(burst, 1, MAX_BUCKETS)) - peak(analyse(held, 1, MAX_BUCKETS))).abs()
            <= 1.0 / 255.0
    );
}

#[kithara::test]
fn heights_are_absolute(waveform_mix: Vec<f32>) {
    let quiet: Vec<_> = waveform_mix[..9_216]
        .iter()
        .map(|sample| sample * 0.25)
        .collect();
    let mut extended = quiet.clone();
    extended.extend(vec![1.0; 9_216]);
    let alone = analyse(&quiet, 1, MAX_BUCKETS);
    let later = analyse(&extended, 1, MAX_BUCKETS);
    assert!(alone.buckets().eq(later.buckets().take(alone.len())));
    let loud = analyse(&waveform_mix[..9_216], 1, MAX_BUCKETS);
    assert!(
        alone
            .buckets()
            .zip(loud.buckets())
            .all(|(quiet, loud)| height(quiet) < height(loud))
    );
}

#[kithara::test]
fn channels_stay_apart_including_antiphase_and_multichannel(waveform_peak_tones: [Vec<f32>; 5]) {
    let mono = &waveform_peak_tones[1][..12_000];
    let left: Vec<_> = mono.iter().flat_map(|sample| [*sample, 0.0]).collect();
    let opposed: Vec<_> = mono.iter().flat_map(|sample| [*sample, -*sample]).collect();
    let multi: Vec<_> = mono
        .iter()
        .flat_map(|sample| [*sample, -*sample, 0.0, 1.0])
        .collect();
    let mono_wave = analyse(mono, 1, MAX_BUCKETS);
    assert!(
        mono_wave
            .buckets()
            .all(|bucket| bucket.left() == bucket.right())
    );
    assert!(
        analyse(&left, 2, MAX_BUCKETS)
            .buckets()
            .all(|bucket| bucket.right() == [0.0; 4])
    );
    let stereo = analyse(&opposed, 2, MAX_BUCKETS);
    assert!(stereo.buckets().eq(mono_wave.buckets()));
    assert_eq!(analyse(&multi, 4, MAX_BUCKETS), stereo);
}

#[kithara::test]
fn range_order_resume_and_partial_columns_are_stable(waveform_peak_tones: [Vec<f32>; 5]) {
    let pcm = &waveform_peak_tones[0][..44_100];
    let expected = analyse(pcm, 1, MAX_BUCKETS);
    let pools = pools();
    let mut shuffled =
        WaveformAnalyzer::new(RATE, AnalysisParams::default(), &pools).expect("setup");
    let blocks: Vec<_> = pcm.chunks(1_500).enumerate().collect();
    for &(index, block) in blocks.iter().rev() {
        shuffled
            .push(
                &pools,
                block,
                1,
                u64::try_from(index * 1_500).expect("bounded fixture"),
            )
            .expect("finite block");
    }
    assert_eq!(
        shuffled
            .snapshot(MAX_BUCKETS, Some(44_100))
            .expect("snapshot"),
        expected
    );
    let mut interrupted =
        WaveformAnalyzer::new(RATE, AnalysisParams::default(), &pools).expect("setup");
    interrupted
        .push(&pools, &pcm[..22_050], 1, 0)
        .expect("head");
    let early = interrupted
        .snapshot(MAX_BUCKETS, Some(44_100))
        .expect("early snapshot");
    let mut bytes = Vec::new();
    interrupted.write_resume(&mut bytes);
    let mut reader = kithara_blob::Reader::new(&bytes);
    let resume = WaveformResume::decode(&mut reader).expect("own record");
    reader.finish().expect("record consumed");
    let mut restored =
        WaveformAnalyzer::new(RATE, AnalysisParams::default(), &pools).expect("setup");
    restored.restore(&pools, resume).expect("restore");
    restored
        .push(&pools, &pcm[22_050..], 1, 22_050)
        .expect("tail");
    let late = restored
        .snapshot(MAX_BUCKETS, Some(44_100))
        .expect("late snapshot");
    assert_eq!(late, expected);
    let mut growing =
        WaveformAnalyzer::new(RATE, AnalysisParams::default(), &pools).expect("setup");
    growing.push(&pools, &pcm[..22_050], 1, 0).expect("head");
    growing
        .snapshot(MAX_BUCKETS, Some(22_050))
        .expect("temporary extent");
    growing
        .push(&pools, &pcm[22_050..], 1, 22_050)
        .expect("tail");
    assert_eq!(
        growing
            .snapshot(MAX_BUCKETS, Some(44_100))
            .expect("grown extent"),
        expected
    );

    for (early, late) in early
        .buckets()
        .zip(late.buckets())
        .filter(|(early, _)| height(*early) > 0.0)
    {
        assert_eq!(early, late);
    }
}

#[kithara::test]
#[case(60.0, 0)]
#[case(401.6597700493015, 1)]
#[case(2688.842847934298, 2)]
#[case(18_000.0, 3)]
fn eq_centres_reject_other_bands_by_thirty_db(
    #[case] hz: f64,
    #[case] dominant: usize,
    waveform_peak_tones: [Vec<f32>; 5],
) {
    let pcm = &waveform_peak_tones[[0, 2, 3, 4][dominant]][..44_100];
    let wave = analyse(pcm, 1, MAX_BUCKETS);
    // Companding halves the dB distance; compare decoded peak amplitudes.
    let mut peaks = [0.0_f32; 4];
    for bucket in wave.buckets().skip(48) {
        for (peak, height) in peaks.iter_mut().zip(bucket.left()) {
            *peak = peak.max(height * height);
        }
    }
    assert!(peaks[dominant] > 0.1);
    for (band, peak) in peaks
        .iter()
        .enumerate()
        .filter(|(band, _)| *band != dominant)
    {
        assert!(
            *peak <= peaks[dominant] * 10.0_f32.powf(-30.0 / 20.0),
            "{hz} Hz: band {band}: {peaks:?}"
        );
    }
}

#[kithara::test]
fn lane_peaks_match_a_double_precision_causal_reference(waveform_peak_tones: [Vec<f32>; 5]) {
    use kithara_dsp::filter::{Coefficients, Hertz, Type};
    let edges = AnalysisParams::default().band_edges_hz();
    let source = &waveform_peak_tones[1][..12_000];
    let actual = analyse(source, 1, MAX_BUCKETS);
    let mut expected = Vec::new();
    for body in 0..4 {
        let start = if body == 0 { 0 } else { body * 3_072 - 1_024 };
        let offset = if body == 0 { 0 } else { 1_024 };
        let mut columns = [[0.0_f32; 4]; 48];
        for band in 0..4 {
            let mut samples: Vec<f64> = (start..start + 4_096)
                .map(|frame| source.get(frame).copied().map_or(0.0, f64::from))
                .collect();
            let stages = match band {
                0 => vec![(Type::LowPass, edges[0])],
                1 => vec![(Type::HighPass, edges[0]), (Type::LowPass, edges[1])],
                2 => vec![(Type::HighPass, edges[1]), (Type::LowPass, edges[2])],
                _ => vec![(Type::HighPass, edges[2])],
            };
            for (kind, cutoff) in stages {
                let coefficients = Coefficients::from_params(
                    kind,
                    Hertz::from_hz(f64::from(RATE)).expect("rate"),
                    Hertz::from_hz(cutoff).expect("cutoff"),
                    std::f64::consts::FRAC_1_SQRT_2,
                )
                .expect("design");
                for _ in 0..2 {
                    let Coefficients { b0, b1, b2, a1, a2 } = coefficients;
                    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
                    for sample in &mut samples {
                        let input = *sample;
                        let output = b0 * input + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
                        (x2, x1, y2, y1) = (x1, input, y1, output);
                        *sample = output;
                    }
                }
            }
            for (column, samples) in columns.iter_mut().zip(
                samples[offset.min(source.len().saturating_sub(start))
                    ..source.len().saturating_sub(start).min(4_096)]
                    .chunks(64),
            ) {
                let peak = samples
                    .iter()
                    .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
                column[band] = (255.0 * peak.min(1.0).sqrt())
                    .round()
                    .to_f32()
                    .expect("byte height fits f32")
                    / 255.0;
            }
        }
        expected.extend(columns);
    }
    for (index, (actual, reference)) in actual.buckets().zip(expected).enumerate() {
        for (band, (actual, reference)) in actual.left().into_iter().zip(reference).enumerate() {
            assert!(
                (actual - reference).abs() <= 1.0 / 255.0,
                "column {index} band {band}: {actual} versus {reference}"
            );
        }
    }
}

#[kithara::test]
#[case(22_050, 32)]
#[case(44_100, 64)]
#[case(48_000, 64)]
#[case(96_000, 128)]
#[case(192_000, 256)]
fn source_rate_scales_columns_without_exceeding_two_ms(
    #[case] rate: u32,
    #[case] frames_per_column: usize,
    analysis_silence: Vec<f32>,
) {
    let pools = pools();
    let mut pass =
        WaveformAnalyzer::new(rate, AnalysisParams::default(), &pools).expect("supported rate");
    pass.push(&pools, &analysis_silence[..10_000], 1, 0)
        .expect("silence");
    let wave = pass.snapshot(MAX_BUCKETS, Some(10_000)).expect("snapshot");
    assert_eq!(wave.len(), 10_000_usize.div_ceil(frames_per_column));
    assert!(f64::from(rate) * 0.002 >= frames_per_column.to_f64().expect("bounded column length"));
}
