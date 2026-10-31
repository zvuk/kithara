<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-dsp.svg)](https://crates.io/crates/kithara-dsp)
[![docs.rs](https://docs.rs/kithara-dsp/badge.svg)](https://docs.rs/kithara-dsp)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-dsp

Vector DSP kernels over planar `f32` slices. The layout functions match
`fast_interleave` for `f32`: Apple builds run the stereo pair on Accelerate,
every other target runs it on `fearless_simd` at the best SIMD level the CPU
reports, and other channel counts take a bit-exact strided copy. `filter`
runs multichannel biquad cascades, `interp` reads a window at fractional
positions and `spectrum` takes the real FFT of a Hann-windowed frame on the
same backends; `downmix` folds interleaved frames to mono and `sum_squares`
reduces a slice. The FFT, the bin kernels and the autocorrelation build only
with the `spectrum` feature, off by default, so a build that runs no analysis
compiles no FFT; `spectrum::FftLen` and `SpectrumError` are always
available. No kernel panics; only `filter::Biquad::new`,
`spectrum::Fft::new` and, off Apple, `Fft::spectrum` allocate.
`Fft::spectrum` takes its sample planes, and `spectrum::Autocorrelation::new`
its padding, from the caller's `kithara-bufpool` region; off Apple the complex
planes of the transform are the FFT library's own.

## Usage

```rust
use std::num::NonZeroUsize;

// Two channels of three frames each, one after the other.
let planar = [1.0_f32, 2.0, 3.0, -1.0, -2.0, -3.0];
let stride = NonZeroUsize::new(3).expect("three frames per plane");
let stereo = NonZeroUsize::new(2).expect("two channels");
let mut interleaved = [0.0_f32; 6];
kithara_dsp::interleave_channel_major(&planar, stride, 0..3, &mut interleaved, stereo);
assert_eq!(
    interleaved.map(f32::to_bits),
    [1.0_f32, -1.0, 2.0, -2.0, 3.0, -3.0].map(f32::to_bits)
);

let mut samples = [f32::NAN, 0.5];
kithara_dsp::sanitize(&mut samples);
assert_eq!(samples, [0.0, 0.5]);
```

## Key Types

<table>

<tr><th>Item</th><th>Role</th></tr>

<tr><td><code>deinterleave_variable</code></td><td>One interleaved slice into planar channels; <code>fast_interleave</code>'s signature for <code>f32</code></td></tr>

<tr><td><code>interleave_channel_major</code></td><td>Every plane of one strided channel-major slice into one interleaved slice, at any channel count without a slice of plane references</td></tr>

<tr><td><code>deinterleave_channel_major</code></td><td>One interleaved slice into every plane of one strided channel-major slice</td></tr>

<tr><td><code>sanitize</code></td><td>Zeroes NaN, ±infinity, subnormals and −0.0 in place</td></tr>

<tr><td><code>downmix</code></td><td>Averages the channels of each whole interleaved frame into one mono sample; two channels run the pair kernel, more channels sum the frame and scale by <code>1/N</code>, bit-identical to that scalar formula</td></tr>

<tr><td><code>sum_squares</code></td><td>Sum of the squares of a slice; an empty slice is <code>+0.0</code></td></tr>

<tr><td><code>filter::Biquad</code></td><td>Biquad sections in cascade over planar channels; <code>new</code> allocates, <code>retune</code>, <code>process</code>, <code>settle</code> and <code>copy_state</code> do not, and silence resets the state</td></tr>

<tr><td><code>filter::Coefficients</code></td><td>The <code>biquad</code> crate's RBJ cookbook designs (with <code>Type</code>, <code>Hertz</code> and <code>Errors</code>), re-exported as the one import path</td></tr>

<tr><td><code>interp::interpolate</code></td><td>Reads a window at fractional positions with an <code>interp::Interpolation</code> method (linear, quadratic, Hermite, Watte); a position outside the window is an error and leaves the output untouched</td></tr>

<tr><td><code>interp::RateRamp</code></td><td>Read rate moving linearly to a target; places a block of positions in closed form and lands on the target exactly</td></tr>

<tr><td><code>spectrum::FftLen</code></td><td>An FFT length <code>f·2ⁿ</code>, <code>f</code> in {1, 3, 5, 15}, <code>n ≥ 4</code>: the lengths vDSP's real DFT runs, held on every backend</td></tr>

<tr><td><code>spectrum::Fft</code></td><td>Real FFT of a Hann-windowed frame into a <code>Spectrum</code> of <code>N/2 + 1</code> unscaled bins; a short frame is zero-padded; <code>new</code> allocates, <code>spectrum</code> takes its sample planes from the caller's pool region and, off Apple, allocates the complex ones, <code>forward</code> does neither</td></tr>

<tr><td><code>spectrum::magnitude</code> / <code>spectrum::phase</code></td><td>Magnitude and phase (<code>atan2</code>) of each bin from its real and imaginary parts, through one <code>fearless_simd</code> kernel on every target</td></tr>

<tr><td><code>spectrum::Autocorrelation</code></td><td>Unbiased autocorrelation of a frame over lags <code>0..N</code>; <code>new</code> takes its zero padding from the caller's pool region, <code>process</code> does not allocate</td></tr>

<tr><td><code>fade::FadeCurve</code></td><td>firewheel's fade curve, re-exported as the one import path</td></tr>

<tr><td><code>curve::CurveShape</code></td><td>Clock-free Hold, Linear and bounded Cubic progression in f64</td></tr>

<tr><td><code>param::*</code></td><td>firewheel's parameter smoother, smoothing filter and A/B mix, re-exported as the one import path</td></tr>

</table>

## Integration

`kithara-signal` interleaves and deinterleaves its buffers through the layout
functions; its pooled planar buffer goes through the channel-major ones, so
no channel count allocates. `kithara-resampler`'s Glide backend filters
through `filter::Biquad`, interpolates through `interp::interpolate` and places
its positions with `interp::RateRamp`. `kithara-decode` sanitizes resampled
planes. `kithara-beat`'s spectral detector reads novelty from `spectrum::Fft`,
`magnitude` and `phase` and its tempo period from `spectrum::Autocorrelation`;
`kithara-waveform` sums band energy with `sum_squares` over an `Fft`, and it,
`kithara-analysis`'s producer and its beat analyzer downmix through `downmix`.
The build target picks the backend at compile time; on x86 the SIMD level is
detected once per process, so a call costs one load before the kernel runs.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-dsp) for detailed contracts, invariants, and internals.
