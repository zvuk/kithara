#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara_effects::filter::{Cutoff, FilterEffect, FilterSettings, Kind};
use kithara_signal::AudioSpec;
use kithara_test_utils::kithara;

const FRAMES: usize = 8192;
const ORIGIN: usize = 4096;
const RATE: u32 = 44_100;
const FREQUENCIES: [f64; 7] = [40.0, 200.0, 400.0, 800.0, 1000.0, 2000.0, 10_000.0];

/// Knob range whose log map spans `start..end` Hz on the side of `kind`.
fn sweep_settings(kind: Kind, start: f64, end: f64) -> (FilterSettings, f64, f64) {
    let cutoff = |hz| Cutoff::checked(hz).expect("sweep cutoff");
    let (edge, extreme, sign) = match kind {
        Kind::LowPass => (1_600.0, 100.0, -1.0),
        Kind::HighPass => (20.0, 2_000.0, 1.0),
    };
    let low = kind == Kind::LowPass;
    let settings = FilterSettings::builder()
        .maybe_low_edge(low.then(|| cutoff(edge)))
        .maybe_low_extreme(low.then(|| cutoff(extreme)))
        .maybe_high_edge((!low).then(|| cutoff(edge)))
        .maybe_high_extreme((!low).then(|| cutoff(extreme)))
        .build()
        .expect("sweep settings");
    let knob = |hz: f64| sign * (hz / edge).ln() / (extreme / edge).ln();
    (settings, knob(start), knob(end))
}

fn sweep_impulse(kind: Kind, start: f64, end: f64) -> Vec<f32> {
    let rate = NonZeroU32::new(RATE).expect("nonzero rate");
    let (settings, from, to) = sweep_settings(kind, start, end);
    let mut filter = FilterEffect::new(AudioSpec::new(2, rate), settings).expect("filter");
    let knobs: Vec<_> = (0..FRAMES)
        .map(|frame| {
            let progress = f64::from(u32::try_from(frame).expect("bounded frame"))
                / f64::from(u32::try_from(FRAMES - 1).expect("bounded duration"));
            if frame == FRAMES - 1 {
                to
            } else {
                from + (to - from) * progress
            }
        })
        .collect();
    let mut planes = [vec![0.0; FRAMES], vec![0.0; FRAMES]];
    planes[0][ORIGIN] = 0.25;
    planes[1][ORIGIN] = -0.125;
    filter
        .process_program(&mut planes, 0..FRAMES, &knobs)
        .expect("sweep program");
    assert!(planes.iter().flatten().all(|sample| sample.is_finite()));
    for (left, right) in planes[0].iter().zip(&planes[1]) {
        assert_eq!(-0.5 * left, *right);
    }
    assert!(planes[0][..ORIGIN].iter().all(|sample| *sample == 0.0));
    planes.into_iter().next().expect("left plane")
}

fn spectrum(samples: &[f32]) -> [f64; 7] {
    FREQUENCIES.map(|frequency| {
        let (real, imaginary) =
            samples
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(real, imaginary), (frame, sample)| {
                    let offset = f64::from(u32::try_from(frame).expect("bounded frame"))
                        - f64::from(u32::try_from(ORIGIN).expect("bounded origin"));
                    let angle = -std::f64::consts::TAU * frequency * offset / f64::from(RATE);
                    let (sine, cosine) = angle.sin_cos();
                    let value = f64::from(*sample) / 0.25;
                    (real + value * cosine, imaginary + value * sine)
                });
        20.0 * real.hypot(imaginary).log10()
    })
}

fn transient(samples: &[f32]) -> (usize, f64) {
    let response = &samples[ORIGIN..];
    let peak = response
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(frame, _)| frame)
        .expect("impulse response");
    let (energy, moment) =
        response
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(energy, moment), (frame, sample)| {
                let square = f64::from(*sample).powi(2);
                (
                    energy + square,
                    moment + square * f64::from(u32::try_from(frame).expect("bounded frame")),
                )
            });
    assert!(energy > 0.0);
    (peak, moment / energy)
}

// Mixline c5ba7f965d21810bc39289ebe274d8ba6d1c4c78: _blend_sweep blends
// two fixed Pedalboard endpoint filters, including _pb_process warmup.
// Pedalboard 0.9.25 / numpy 2.5.3; target values use an independent f64 RBJ
// moving-coefficient recurrence. Zero-phase band-RMS analysis is not this renderer.
#[kithara::test]
#[case(Kind::LowPass, 200.0, 800.0,
    [-0.03803016519091928, -0.31243755661981026, -3.0085296596782913, -12.300977704316743, -16.039312948646973, -28.072866590148898, -59.25390793766275],
    [-0.13939988314240614, -1.840902541980701, -3.8608760187858646, -6.704284122147302, -7.913987553168983, -12.661464126395778, -27.702016598032053],
    (19, 24.730840059560922), (1, 7.285687669534216))]
#[case(Kind::HighPass, 30.0, 1000.0,
    [-25.973537092599763, -2.239042769789979, -0.1880179706610568, -0.019072525937533596, -0.010065486338602598, -0.0017720828869520282, -1.1212147234307591e-05],
    [-7.876631178088366, -5.443576747233919, -4.39350342745561, -2.5886555369291213, -1.991518843136265, -0.6840565872053139, -0.021634577445410745],
    (0, 0.4798639504267108), (0, 0.1394051881275308))]
fn sweep_response_reports_pinned_reference_differences(
    #[case] kind: Kind,
    #[case] start: f64,
    #[case] end: f64,
    #[case] causal_db: [f64; 7],
    #[case] mixline_db: [f64; 7],
    #[case] causal_transient: (usize, f64),
    #[case] mixline_transient: (usize, f64),
) {
    let (causal_peak, causal_centroid) = causal_transient;
    let (mixline_peak, mixline_centroid) = mixline_transient;
    let output = sweep_impulse(kind, start, end);
    let measured_db = spectrum(&output);
    for ((measured, expected), frequency) in measured_db.iter().zip(causal_db).zip(FREQUENCIES) {
        assert!(
            (measured - expected).abs() <= 0.5,
            "{kind:?} at {frequency} Hz: {measured} dB, causal {expected} dB"
        );
    }
    let (peak, centroid) = transient(&output);
    assert_eq!(peak, causal_peak);
    assert!(
        (centroid - causal_centroid).abs() <= 0.1,
        "{kind:?}: energy centroid {centroid}, causal {causal_centroid}"
    );
    let spectral_difference: [f64; 7] = std::array::from_fn(|i| measured_db[i] - mixline_db[i]);
    eprintln!(
        "{kind:?}: product_db={measured_db:?}; product_minus_mixline_db={spectral_difference:?}; peak={peak}, mixline_peak={mixline_peak}; centroid={centroid}, mixline_centroid={mixline_centroid}"
    );
}
