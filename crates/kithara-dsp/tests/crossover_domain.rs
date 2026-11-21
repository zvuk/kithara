use kithara_dsp::filter::{Crossover, Hertz};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

fn hz(value: f64) -> Hertz<f64> {
    Hertz::from_hz(value).expect("positive domain point")
}

fn impulse(filter: &mut Crossover, bands: usize) -> [f32; 3] {
    let mut output = [[0.0_f32; 1]; 3];
    let [low, middle, high] = &mut output;
    let mut planes = [
        [low.as_mut_slice()],
        [middle.as_mut_slice()],
        [high.as_mut_slice()],
    ];
    assert_eq!(filter.split(&[&[1.0][..]], &mut planes[..bands]), 1);
    output.map(|plane| plane[0])
}

#[kithara::test]
#[case(44_100)]
#[case(48_000)]
#[case(96_000)]
#[case(192_000)]
#[case(352_800)]
#[case(384_000)]
#[case(705_600)]
#[case(768_000)]
fn lr4_ap2_domain_constructs_and_retunes_without_response_holes(#[case] rate: u32) {
    const INTERVALS: u32 = 65_536;
    let mut minimum = f32::INFINITY;
    for point in 0..=INTERVALS {
        let cutoff = match point {
            0 => 20.0,
            INTERVALS => 20_000.0,
            _ => 20.0 * (1_000.0_f64.ln() * f64::from(point) / f64::from(INTERVALS)).exp(),
        };
        let frequencies = if point == 0 {
            vec![hz(20.0)]
        } else {
            vec![hz(20.0), hz(cutoff)]
        };
        let mut fresh =
            Crossover::<1>::new(&frequencies, hz(f64::from(rate))).expect("fresh domain design");
        let mut retuned =
            Crossover::<1>::new(&frequencies, hz(44_100.0)).expect("original domain design");
        retuned
            .update_sample_rate(hz(f64::from(rate)))
            .expect("domain retune");
        let actual = impulse(&mut fresh, frequencies.len() + 1);
        let updated = impulse(&mut retuned, frequencies.len() + 1);
        assert!(actual.iter().all(|sample| sample.is_finite()));
        assert!(
            actual[0] > 0.0,
            "rate{rate} cutoff{cutoff}: low band collapsed"
        );
        assert_eq!(actual.map(f32::to_bits), updated.map(f32::to_bits));
        minimum = minimum.min(actual[0]);
    }
    assert!(Crossover::<1>::new(&[hz(f64::from(rate) / 2.0)], hz(f64::from(rate))).is_err());
    println!(
        "LR4_AP2_DOMAIN,rate={rate},points={},refused=0,collapsed=0,minimum_low={minimum:e}",
        INTERVALS + 1
    );
}

fn divisor(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn response(frequencies: &[f64], rate: u32, tone: u32) {
    const TOLERANCE: f64 = 1e-5;
    let period = usize::try_from(rate).expect("test rate fits usize")
        / divisor(
            usize::try_from(rate).expect("test rate fits usize"),
            usize::try_from(tone).expect("test tone fits usize"),
        );
    let warmup = (8.0 * f64::from(rate) / frequencies[0])
        .ceil()
        .to_usize()
        .expect("positive bounded warmup");
    let measured = 4 * period;
    let frames = warmup + measured;
    let phase = |frame: usize| {
        std::f64::consts::TAU * f64::from(tone) * frame.to_f64().expect("test frame fits f64")
            / f64::from(rate)
    };
    let mut output: Vec<f32> = (0..frames)
        .map(|frame| phase(frame).sin().to_f32().expect("bounded sine"))
        .collect();
    let gains = vec![vec![1.0_f32; frames]; frequencies.len() + 1];
    let cutoffs: Vec<_> = frequencies.iter().copied().map(hz).collect();
    let mut filter = Crossover::<1>::new(&cutoffs, hz(f64::from(rate))).expect("response design");
    let mut bands = vec![[vec![0.0; frames]]; gains.len()];
    assert_eq!(filter.split(&[output.as_slice()], &mut bands), frames);
    output.fill(0.0);
    for (band, gain) in bands.iter().zip(&gains) {
        kithara_dsp::mul_add(&band[0], gain, &mut output);
    }
    assert!(output.iter().all(|sample| sample.is_finite()));
    let (mut real, mut imaginary) = (0.0, 0.0);
    for (frame, value) in output.iter().enumerate().skip(warmup) {
        let (sine, cosine) = phase(frame).sin_cos();
        real += f64::from(*value) * sine;
        imaginary += f64::from(*value) * cosine;
    }
    let scale = 2.0 / measured.to_f64().expect("test window fits f64");
    let (real, imaginary) = (real * scale, imaginary * scale);
    let mut expected = (1.0, 0.0);
    for cutoff in frequencies {
        let ratio = (std::f64::consts::PI * f64::from(tone) / f64::from(rate)).tan()
            / (std::f64::consts::PI * cutoff / f64::from(rate)).tan();
        let a = 1.0 - ratio * ratio;
        let b = ratio / std::f64::consts::FRAC_1_SQRT_2;
        let denominator = a * a + b * b;
        let next = ((a * a - b * b) / denominator, -2.0 * a * b / denominator);
        expected = (
            expected.0 * next.0 - expected.1 * next.1,
            expected.0 * next.1 + expected.1 * next.0,
        );
    }
    assert!(
        (real.hypot(imaginary) - 1.0).abs() <= TOLERANCE,
        "magnitude rate{rate} tone{tone} cutoffs{frequencies:?}: ({real},{imaginary})"
    );
    assert!(
        (real - expected.0).abs() <= TOLERANCE && (imaginary - expected.1).abs() <= TOLERANCE,
        "phase response rate{rate} tone{tone} cutoffs{frequencies:?}: ({real},{imaginary}) expected{expected:?}"
    );
}

#[kithara::test]
#[case(44_100)]
#[case(48_000)]
#[case(96_000)]
#[case(192_000)]
#[case(352_800)]
#[case(384_000)]
#[case(705_600)]
#[case(768_000)]
fn lr4_ap2_unity_sum_has_the_analytical_allpass_response(#[case] rate: u32) {
    for cutoff in [
        20.0, 40.0, 100.0, 250.0, 1_000.0, 4_325.0, 10_000.0, 20_000.0,
    ] {
        let frequencies = if cutoff == 20.0 {
            vec![20.0]
        } else {
            vec![20.0, cutoff]
        };
        for tone in [20, 1_000, 18_000, 20_000] {
            response(&frequencies, rate, tone);
        }
    }
    for frequencies in [
        &[
            f64::from(f32::from_bits(1132049691)),
            f64::from(f32::from_bits(1166485625)),
        ][..],
        &[
            f64::from(f32::from_bits(1118092657)),
            f64::from(f32::from_bits(1125858699)),
            f64::from(f32::from_bits(1133660640)),
            f64::from(f32::from_bits(1141496414)),
            f64::from(f32::from_bits(1149364066)),
            f64::from(f32::from_bits(1156895608)),
            f64::from(f32::from_bits(1164358997)),
            f64::from(f32::from_bits(1171875738)),
            f64::from(f32::from_bits(1179442756)),
        ][..],
    ] {
        for tone in [20, 1_000, 18_000, 20_000] {
            response(frequencies, rate, tone);
        }
    }
    println!("LR4_AP2_RESPONSE,rate={rate},layouts=10,tones=4,magnitude_phase_tolerance=1e-5");
}
