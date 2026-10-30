#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
#[cfg(feature = "spectrum")]
use kithara_dsp::spectrum::{Autocorrelation, Fft, FftLen, magnitude, phase};
use kithara_dsp::{
    fade::{self, FadeCurve},
    filter::{Biquad, Coefficients, Hertz, Type},
    interp::{InterpError, Interpolation, interpolate},
};
#[cfg(feature = "spectrum")]
use kithara_test_utils::bufpool::pools;
use kithara_test_utils::kithara;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const FRAMES: usize = 1024;
const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
const SIX: NonZeroUsize = NonZeroUsize::MIN.saturating_add(5);
const TWELVE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(11);
const PLANE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(FRAMES + 7);
#[cfg(feature = "spectrum")]
const LAGS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(511);

#[kithara::test(native)]
fn layout_and_sanitize_never_allocate() {
    let mut restored = vec![vec![0.25_f32; FRAMES]; SIX.get()];
    let stereo = vec![0.0_f32; TWO.get() * FRAMES];
    let mut six = vec![0.0_f32; SIX.get() * FRAMES];
    assert_no_alloc(|| {
        kithara_dsp::deinterleave_variable(&stereo, NonZeroUsize::MIN, &mut restored, 0..FRAMES);
        kithara_dsp::deinterleave_variable(&stereo, TWO, &mut restored, 0..FRAMES);
        kithara_dsp::deinterleave_variable(&six, SIX, &mut restored, 0..FRAMES);
        kithara_dsp::sanitize(&mut six);
    });
}

#[kithara::test(native)]
fn channel_major_layout_never_allocates_at_any_channel_count() {
    let mut planar = vec![0.25_f32; TWELVE.get() * PLANE.get()];
    let mut interleaved = vec![0.0_f32; TWELVE.get() * FRAMES];
    assert_no_alloc(|| {
        for channels in [NonZeroUsize::MIN, TWO, SIX, TWELVE] {
            kithara_dsp::interleave_channel_major(
                &planar,
                PLANE,
                3..3 + FRAMES,
                &mut interleaved,
                channels,
            );
            kithara_dsp::deinterleave_channel_major(
                &interleaved,
                channels,
                &mut planar,
                PLANE,
                3..3 + FRAMES,
            );
        }
    });
}

#[kithara::test(native)]
fn biquad_never_allocates_after_construction() {
    let four = NonZeroUsize::MIN.saturating_add(3);
    let mut filter = Biquad::new(TWO, four).expect("filter builds");
    let mut lookahead = Biquad::new(TWO, four).expect("filter builds");
    let low_pass = Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(48_000.0).expect("positive rate"),
        Hertz::from_hz(4_000.0).expect("positive cutoff"),
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .expect("valid low-pass");
    let mut planes = vec![vec![0.25_f32; FRAMES]; TWO.get()];
    assert_no_alloc(|| {
        for section in 0..four.get() {
            filter.retune(section, low_pass).expect("section in range");
            lookahead
                .retune(section, low_pass)
                .expect("section in range");
        }
        filter.settle(&[0.25, 0.25]).expect("one level per channel");
        filter
            .process(&mut planes, 0..FRAMES / 2)
            .expect("shape matches");
        lookahead.copy_state(&filter).expect("same shape");
        lookahead
            .process(&mut planes, FRAMES / 2..FRAMES)
            .expect("shape matches");
        for plane in &mut planes {
            plane.fill(0.0);
        }
        filter
            .process(&mut planes, 0..FRAMES)
            .expect("shape matches");
        for plane in &mut planes {
            plane.fill(0.0);
        }
        filter
            .process(&mut planes, 0..FRAMES)
            .expect("silence resets");
    });
}

#[kithara::test(native)]
fn interpolation_never_allocates() {
    let window = vec![0.25_f32; FRAMES + 4];
    let positions: Vec<f32> = std::iter::successors(Some(1.5_f32), |position| Some(position + 1.0))
        .take(FRAMES - 1)
        .collect();
    let mut output = vec![0.0_f32; FRAMES];
    assert_no_alloc(|| {
        for method in [
            Interpolation::Linear,
            Interpolation::Quadratic,
            Interpolation::Hermite,
            Interpolation::Watte,
        ] {
            assert_eq!(
                interpolate(method, &window, &positions, &mut output),
                Ok(FRAMES - 1)
            );
        }
        assert_eq!(
            interpolate(Interpolation::Linear, &window, &[f32::NAN], &mut output),
            Err(InterpError::OutOfWindow)
        );
    });
}

#[cfg(feature = "spectrum")]
#[kithara::test(native)]
fn spectrum_never_allocates_after_construction() {
    let fft =
        Fft::new(FftLen::new(FRAMES).expect("1024 is an FFT length")).expect("the FFT builds");
    let mut spectrum = fft.spectrum(&pools()).expect("the planes fit the region");
    let frame = vec![0.25_f32; FRAMES];
    let mut bins = vec![0.0_f32; FRAMES / 2 + 1];
    assert_no_alloc(|| {
        fft.forward(&frame, &mut spectrum).expect("the frame fits");
        fft.forward(&frame[..FRAMES / 2], &mut spectrum)
            .expect("a short frame fits");
        assert_eq!(
            magnitude(spectrum.re(), spectrum.im(), &mut bins),
            FRAMES / 2 + 1
        );
        assert_eq!(
            phase(spectrum.re(), spectrum.im(), &mut bins),
            FRAMES / 2 + 1
        );
    });
}

#[cfg(feature = "spectrum")]
#[kithara::test(native)]
fn autocorrelation_never_allocates_after_construction() {
    let mut acf = Autocorrelation::new(LAGS, &pools()).expect("the padding fits the region");
    let frame = vec![0.25_f32; LAGS.get()];
    let mut output = vec![0.0_f32; LAGS.get()];
    assert_no_alloc(|| {
        assert_eq!(acf.process(&frame, &mut output), LAGS.get());
        assert_eq!(acf.process(&frame[..100], &mut output), LAGS.get());
    });
}

#[kithara::test(native)]
fn sum_squares_never_allocates() {
    let samples = vec![0.25_f32; FRAMES];
    assert_no_alloc(|| {
        assert_eq!(
            kithara_dsp::sum_squares(&samples).to_bits(),
            64.0_f32.to_bits()
        );
    });
}

#[kithara::test(native)]
fn elementwise_kernels_and_fade_gains_never_allocate() {
    let input = vec![0.25_f32; FRAMES];
    let mut acc = vec![0.0_f32; FRAMES];
    let mut positions = vec![0.0_f32; FRAMES];
    let mut outgoing = vec![0.0_f32; FRAMES];
    let mut incoming = vec![0.0_f32; FRAMES];
    assert_no_alloc(|| {
        assert_eq!(kithara_dsp::ramp(&mut positions, 0.0, 1.0), FRAMES);
        assert_eq!(kithara_dsp::div(&mut positions, 1_023.0), FRAMES);
        assert_eq!(kithara_dsp::affine(&mut acc, 0.5, 0.25), FRAMES);
        assert_eq!(kithara_dsp::sub(&mut acc, &input), FRAMES);
        assert_eq!(kithara_dsp::scale_add(&input, 0.5, &mut acc), FRAMES);
        assert_eq!(kithara_dsp::mul_add(&input, &input, &mut acc), FRAMES);
        assert_eq!(kithara_dsp::add(&input, &mut acc), FRAMES);
        assert_eq!(kithara_dsp::multiply_in_place(&mut acc, &input), FRAMES);
        for curve in [
            FadeCurve::EqualPower3dB,
            FadeCurve::EqualPower6dB,
            FadeCurve::SquareRoot,
            FadeCurve::Linear,
        ] {
            assert_eq!(
                fade::gains(curve, &positions, &mut outgoing, &mut incoming),
                FRAMES
            );
            for frame in [0, FRAMES / 2, FRAMES - 1] {
                let (a, b) = fade::gains_at(curve, positions[frame]);
                assert_eq!(a.to_bits(), outgoing[frame].to_bits());
                assert_eq!(b.to_bits(), incoming[frame].to_bits());
            }
        }
    });
}

#[kithara::test(native)]
fn downmix_never_allocates_at_any_channel_count() {
    let interleaved = vec![0.25_f32; TWELVE.get() * FRAMES];
    let mut mono = vec![0.0_f32; FRAMES];
    assert_no_alloc(|| {
        for channels in [NonZeroUsize::MIN, TWO, SIX, TWELVE] {
            assert_eq!(
                kithara_dsp::downmix(&interleaved, channels, &mut mono),
                FRAMES
            );
        }
    });
}

#[kithara::test(native)]
fn fir_history_peak_linking_and_envelope_never_allocate() {
    let mut detector = kithara_dsp::filter::Fir::<17, 7, 2, 9>::new([[0.125; 17]; 7]);
    let mut envelope = kithara_dsp::envelope::AttackRelease::new(0.999);
    let mut left = [2.0; 600];
    let mut right = [-2.0; 607];
    let mut peaks = [0.0; 600];
    let mut linked = peaks;
    assert_no_alloc(|| {
        for frames in [600, 1, 0, 7, 255, 256, 257, 1, 0, 600] {
            detector.begin_block(&[&mut left[..frames], &mut right[..frames + 7]], 0.98);
            linked.fill(0.0);
            assert_eq!(
                detector.process(0, &left[..frames], &left[..frames], &mut peaks[..frames]),
                frames
            );
            assert_eq!(
                kithara_dsp::envelope::PeakHold::link(&peaks[..frames], &mut linked[..frames]),
                frames
            );
            assert_eq!(
                detector.process(
                    1,
                    &right[..frames + 7],
                    &right[..frames],
                    &mut peaks[..frames]
                ),
                frames
            );
            assert_eq!(
                kithara_dsp::envelope::PeakHold::link(&peaks[..frames], &mut linked[..frames]),
                frames
            );
            for target in &mut linked[..frames] {
                *target = if *target > 0.98 { 0.98 / *target } else { 1.0 };
            }
            assert_eq!(envelope.process(&mut linked[..frames]), frames);
            kithara_dsp::multiply_in_place(&mut left[..frames], &linked[..frames]);
            kithara_dsp::multiply_in_place(&mut right[..frames], &linked[..frames]);
        }
        detector.reset();
        envelope.reset();
    });
}
