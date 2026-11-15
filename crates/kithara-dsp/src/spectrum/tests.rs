use std::{f32::consts::TAU, num::NonZeroUsize};

use kithara_test_utils::{
    bufpool::{pools, pools_with_budget},
    kithara,
};
use num_traits::ToPrimitive;

use super::{Autocorrelation, Fft, FftLen, Spectrum, SpectrumError, fft::hann, oracle};

const VALID: [usize; 9] = [16, 32, 48, 64, 80, 240, 1024, 3072, 4096];
const INVALID: [usize; 10] = [0, 1, 2, 8, 12, 17, 24, 40, 112, 144];
const LENGTHS: [usize; 7] = [16, 48, 80, 240, 1024, 3072, 4096];
const FRAME: usize = 1024;
const SHORT: usize = 700;
const UNWRITTEN: f32 = -1.0;
/// The coefficient of the window novelty tabulated for itself.
const NOVELTY_A0: f32 = 0.5;
/// The periodicity window of beat's period stage.
const LAGS: usize = 512;
const SHORT_LAGS: usize = 300;

fn fft(len: usize) -> Fft {
    Fft::new(FftLen::new(len).expect("the test lengths are FFT lengths"))
        .expect("every FFT length builds")
}

/// A spectrum for `fft` from a region with room for it.
fn spectrum(fft: &Fft) -> Spectrum {
    fft.spectrum(&pools()).expect("the planes fit the region")
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn windowed(frame: &[f32], len: FftLen) -> Vec<f32> {
    frame
        .iter()
        .zip(hann(len, 1.0).iter())
        .map(|(x, w)| x * w)
        .collect()
}

#[kithara::test]
fn fft_lengths_are_the_real_dft_lengths_vdsp_builds() {
    for len in VALID {
        let fft_len = FftLen::new(len).expect("a valid FFT length");
        assert_eq!(fft_len.get(), len);
        assert_eq!(fft_len.bins(), (len / 2).saturating_add(1));
        assert!(Fft::new(fft_len).is_ok(), "{len} builds");
    }
    for len in INVALID {
        assert_eq!(FftLen::new(len), Err(SpectrumError::Length), "{len}");
    }
}

#[kithara::test]
fn the_window_is_the_hann_window_novelty_tabulated() {
    let len = FftLen::new(FRAME).expect("a valid FFT length");
    let step = TAU / 1023.0_f32;
    let expected: Vec<u32> = (0..1024_u16)
        .map(|n| {
            let phase = step * f32::from(n);
            NOVELTY_A0.mul_add(-phase.cos(), NOVELTY_A0).to_bits()
        })
        .collect();
    assert_eq!(bits(&hann(len, 1.0)), expected);
}

#[kithara::test]
fn the_spectrum_tracks_a_direct_dft() {
    for len in LENGTHS {
        let fft = fft(len);
        let mut spectrum = spectrum(&fft);
        let frame = oracle::mix(len);
        fft.forward(&frame, &mut spectrum).expect("the frame fits");
        let want = oracle::dft(&windowed(&frame, fft.size()));
        let snr = oracle::snr([spectrum.re(), spectrum.im()], &want);
        assert!(snr >= oracle::DFT_SNR_DB, "{len}: {snr} dB");
    }
}

#[kithara::test]
fn dc_and_nyquist_bins_are_real() {
    let fft = fft(FRAME);
    let constant = vec![1.0_f32; FRAME];
    let alternating: Vec<f32> = (0..FRAME)
        .map(|n| if n.is_multiple_of(2) { 1.0 } else { -1.0 })
        .collect();
    for (name, frame) in [("dc", constant), ("nyquist", alternating)] {
        let mut spectrum = spectrum(&fft);
        fft.forward(&frame, &mut spectrum).expect("the frame fits");
        let im = spectrum.im();
        assert_eq!(im.len(), fft.size().bins(), "{name}: bin count");
        assert_eq!(
            im.first().map(|value| value.to_bits()),
            Some(0),
            "{name}: bin 0 is real"
        );
        assert_eq!(
            im.last().map(|value| value.to_bits()),
            Some(0),
            "{name}: bin N/2 is real"
        );
        let want = oracle::dft(&windowed(&frame, fft.size()));
        let snr = oracle::snr([spectrum.re(), im], &want);
        assert!(snr >= oracle::DFT_SNR_DB, "{name}: {snr} dB");
    }
}

#[kithara::test]
fn a_short_frame_is_the_zero_padded_frame() {
    let fft = fft(FRAME);
    let short = oracle::mix(SHORT);
    let padded: Vec<f32> = short
        .iter()
        .copied()
        .chain(std::iter::repeat(0.0))
        .take(FRAME)
        .collect();
    let mut reused = spectrum(&fft);
    fft.forward(&oracle::mix(FRAME), &mut reused)
        .expect("a whole frame fits");
    fft.forward(&short, &mut reused)
        .expect("a short frame fits");
    let mut fresh = spectrum(&fft);
    fft.forward(&padded, &mut fresh)
        .expect("the padded frame fits");
    assert_eq!(bits(reused.re()), bits(fresh.re()));
    assert_eq!(bits(reused.im()), bits(fresh.im()));
}

#[kithara::test]
fn a_long_frame_or_a_foreign_spectrum_is_a_shape_error() {
    let (fft, other) = (fft(16), fft(32));
    let mut foreign = spectrum(&other);
    assert_eq!(
        fft.forward(&[0.0_f32; 16], &mut foreign),
        Err(SpectrumError::Shape)
    );
    let mut own = spectrum(&fft);
    assert_eq!(
        fft.forward(&[0.0_f32; 17], &mut own),
        Err(SpectrumError::Shape)
    );
    assert_eq!(fft.forward(&[0.0_f32; 16], &mut own), Ok(()));
}

#[kithara::test]
fn silence_has_a_zero_spectrum() {
    let fft = fft(FRAME);
    let mut spectrum = spectrum(&fft);
    fft.forward(&[0.0_f32; FRAME], &mut spectrum)
        .expect("the frame fits");
    let (re, im) = (spectrum.re(), spectrum.im());
    assert!(
        re.iter().chain(im).all(|value| *value == 0.0),
        "silence has no spectrum"
    );
}

#[kithara::test]
fn a_spectrum_takes_its_planes_from_the_callers_region() {
    let fft = fft(FRAME);
    let planes = FRAME
        .saturating_add(fft.size().bins().saturating_mul(2))
        .saturating_mul(size_of::<f32>());
    let region = pools();
    let before = region.stats().allocated_bytes;
    let taken = fft.spectrum(&region).expect("the planes fit the region");
    assert!(
        region.stats().allocated_bytes >= before.saturating_add(planes),
        "the region accounts for the planes"
    );
    drop(taken);
    assert!(
        fft.spectrum(&pools_with_budget(planes / 2)).is_err(),
        "a region without room for the planes refuses the spectrum"
    );
}

#[kithara::test]
fn an_fft_moves_and_shares_across_threads() {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<Fft>();
    send_and_sync::<Spectrum>();
}

fn autocorrelation() -> Autocorrelation {
    Autocorrelation::new(
        NonZeroUsize::new(LAGS).expect("a positive length"),
        &pools(),
    )
    .expect("the padding fits the region")
}

/// Lag `k` is `Σ x[k + j]·x[j] / (N − k)` within the reduction bound
/// `(N + 1)·ε·Σ|terms| / (N − k)`; a short frame is zero-extended, a long
/// one cut to `N`, and a reused instance forgets the frame before.
#[kithara::test]
fn autocorrelation_tracks_the_unbiased_f64_estimate() {
    let mut acf = autocorrelation();
    let long = oracle::mix(LAGS.saturating_mul(2));
    let frames = [
        ("whole", oracle::mix(LAGS)),
        ("short", oracle::mix(SHORT_LAGS)),
        ("long", long.clone()),
    ];
    let reach = LAGS.saturating_add(1).to_f64().unwrap_or(f64::NAN) * f64::from(f32::EPSILON);
    for (name, frame) in frames {
        let seen: Vec<f32> = frame.iter().copied().take(LAGS).collect();
        let mut output = vec![UNWRITTEN; LAGS.saturating_add(1)];
        assert_eq!(acf.process(&frame, &mut output), LAGS, "{name}: lag count");
        for (lag, got) in output.iter().take(LAGS).enumerate() {
            let (sum, size) =
                seen.iter()
                    .skip(lag)
                    .zip(&seen)
                    .fold((0.0_f64, 0.0_f64), |(sum, size), (x, y)| {
                        let term = f64::from(*x) * f64::from(*y);
                        (sum + term, size + term.abs())
                    });
            let count = LAGS.saturating_sub(lag).to_f64().unwrap_or(f64::NAN);
            assert!(
                (f64::from(*got) - sum / count).abs() <= reach * size / count,
                "{name}: lag {lag}: {got}, f64 {}",
                sum / count
            );
        }
        assert_eq!(
            output.last().map(|value| value.to_bits()),
            Some(UNWRITTEN.to_bits()),
            "{name}: wrote past {LAGS} lags"
        );
    }
    let mut fewer = [UNWRITTEN; 3];
    assert_eq!(
        acf.process(&long, &mut fewer),
        3,
        "as many lags as the output holds"
    );
}

#[kithara::test]
fn an_autocorrelation_takes_its_padding_from_the_callers_region() {
    let len = NonZeroUsize::new(LAGS).expect("a positive length");
    let padding = LAGS
        .saturating_mul(2)
        .saturating_sub(1)
        .saturating_mul(size_of::<f32>());
    let region = pools();
    let before = region.stats().allocated_bytes;
    let taken = Autocorrelation::new(len, &region).expect("the padding fits the region");
    assert!(
        region.stats().allocated_bytes >= before.saturating_add(padding),
        "the region accounts for the padding"
    );
    drop(taken);
    assert!(
        Autocorrelation::new(len, &pools_with_budget(padding / 2)).is_err(),
        "a region without room for the padding refuses the autocorrelation"
    );
}
