use std::sync::Arc;

use num_traits::cast::ToPrimitive;
use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

use super::{
    bucket::{Bucket, Waveform},
    bucketize::bucketize,
};

/// FFT / band-split / reduction tunables. One home for the constants.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct AnalysisParams {
    /// FFT window length (real input); band bins span `0..=fft_size/2`.
    pub fft_size: usize,
    /// Low/mid crossover in Hz.
    pub low_mid_hz: f32,
    /// Mid/high crossover in Hz.
    pub mid_high_hz: f32,
    /// Per-window RMS gate; windows below it contribute no band energy.
    pub energy_floor: f32,
    /// Per-band perceptual gain (`[low, mid, high]`) applied to magnitudes
    /// before shared normalization. Music tilts energy toward the low end, so
    /// without lifting mid/high the upper bands render as invisible slivers.
    /// This is the balance knob, not a color: low stays the dominant hull.
    pub band_gain: [f32; 3],
}

impl Default for AnalysisParams {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            low_mid_hz: 250.0,
            mid_high_hz: 2500.0,
            energy_floor: 1e-4,
            band_gain: [1.0, 2.5, 12.0],
        }
    }
}

/// Synchronous streaming waveform analyzer. Downmixes to mono once (channel
/// mean) and reduces the spectrum to an overlapping-window (75% Hann overlap)
/// raw band-energy series. `finalize` folds it into per-bucket band heights:
/// three independent envelopes (low/mid/high) the deck paints concentrically.
pub struct WaveformAnalyzer {
    params: AnalysisParams,
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    low_mid_bin: usize,
    mid_high_bin: usize,
    band_bin_inv: [f32; 3],
    window_hop: usize,
    fill: Vec<f32>,
    fft_input: Vec<f32>,
    fft_output: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    raw_bands: Vec<[f32; 3]>,
}

impl WaveformAnalyzer {
    #[must_use]
    pub fn new(sample_rate: u32, params: AnalysisParams) -> Self {
        let fft_size = params.fft_size.max(2);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let fft_input = fft.make_input_vec();
        let fft_output = fft.make_output_vec();
        let fft_scratch = fft.make_scratch_vec();

        let hann = hann_window(fft_size);
        let bins = fft_output.len();
        let rate = sample_rate.to_f32().unwrap_or(0.0);
        let size_f = fft_size.to_f32().unwrap_or(1.0);
        let bin_hz = if size_f > 0.0 { rate / size_f } else { 0.0 };
        let low_mid_bin = crossover_bin(params.low_mid_hz, bin_hz, bins);
        let mid_high_bin = crossover_bin(params.mid_high_hz, bin_hz, bins).max(low_mid_bin);
        // Per-band inverse bin count: divide summed energy by bandwidth so a
        // wide band (mid/high) doesn't outweigh a narrow one (low) by sheer bin
        // count. This makes each band an energy density (RMS-like), so bass can
        // be the hull instead of mid.
        let inv = |count: usize| 1.0 / count.max(1).to_f32().unwrap_or(1.0);
        let band_bin_inv = [
            inv(low_mid_bin.saturating_sub(1)),
            inv(mid_high_bin.saturating_sub(low_mid_bin)),
            inv(bins.saturating_sub(mid_high_bin)),
        ];

        Self {
            params,
            fft,
            hann,
            low_mid_bin,
            mid_high_bin,
            band_bin_inv,
            window_hop: (fft_size / 4).max(1),
            fill: Vec::with_capacity(fft_size),
            fft_input,
            fft_output,
            fft_scratch,
            raw_bands: Vec::new(),
        }
    }

    /// Fold one interleaved chunk: downmix to mono (channel mean), fill FFT
    /// windows and push completed-window band energies.
    pub fn push_interleaved(&mut self, pcm: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let inv_channels = 1.0 / channels.to_f32().unwrap_or(1.0);

        for frame in pcm.chunks_exact(channels) {
            let mono: f32 = frame.iter().sum::<f32>() * inv_channels;
            self.fill.push(mono);
            if self.fill.len() == self.fft_input.len() {
                self.run_window();
            }
        }
    }

    fn run_window(&mut self) {
        for ((dst, &sample), &w) in self
            .fft_input
            .iter_mut()
            .zip(self.fill.iter())
            .zip(self.hann.iter())
        {
            *dst = sample * w;
        }
        // Slide by one hop; the retained tail overlaps the next window.
        self.fill.drain(0..self.window_hop.min(self.fill.len()));
        self.push_window_bands();
    }

    /// Run the FFT on the current `fft_input` and push its band energies.
    fn push_window_bands(&mut self) {
        let bands = if self
            .fft
            .process_with_scratch(
                &mut self.fft_input,
                &mut self.fft_output,
                &mut self.fft_scratch,
            )
            .is_ok()
        {
            self.window_bands()
        } else {
            [0.0; 3]
        };
        self.raw_bands.push(bands);
    }

    /// Window the leftover `fill` (zero-padded) into one final band frame. Only
    /// needed for tracks shorter than one FFT window, which never trip the
    /// full-window path and would otherwise analyse to nothing.
    fn flush_partial(&mut self) {
        if self.fill.is_empty() {
            return;
        }
        let n = self.fill.len().min(self.fft_input.len());
        for (i, dst) in self.fft_input.iter_mut().enumerate() {
            *dst = if i < n {
                self.fill[i] * self.hann[i]
            } else {
                0.0
            };
        }
        self.push_window_bands();
    }

    fn window_bands(&self) -> [f32; 3] {
        // Zero the DC bin so a constant offset never colors the low band.
        let bins = &self.fft_output[1..];
        let total: f32 = bins.iter().map(Complex::norm_sqr).sum();
        let rms = (total / self.fft_input.len().to_f32().unwrap_or(1.0)).sqrt();
        if rms < self.params.energy_floor {
            return [0.0; 3];
        }

        let mut band = [0.0_f32; 3];
        for (i, c) in self.fft_output.iter().enumerate().skip(1) {
            let energy = c.norm_sqr();
            if i < self.low_mid_bin {
                band[0] += energy;
            } else if i < self.mid_high_bin {
                band[1] += energy;
            } else {
                band[2] += energy;
            }
        }
        [
            band[0] * self.band_bin_inv[0],
            band[1] * self.band_bin_inv[1],
            band[2] * self.band_bin_inv[2],
        ]
    }

    /// Bucketize the band-energy series (combine = component-wise max, so each
    /// bucket keeps its loudest window) and turn it into per-bucket band heights
    /// via [`normalize_bands`]. Empty when no frames were analysed.
    #[must_use]
    pub fn finalize(mut self, buckets: usize) -> Waveform {
        if self.raw_bands.is_empty() {
            self.flush_partial();
        }
        if buckets == 0 || self.raw_bands.is_empty() {
            return Waveform::from(Vec::new());
        }

        let max = |a: [f32; 3], b: [f32; 3]| [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
        let energy = bucketize(&self.raw_bands, buckets, [0.0; 3], max);
        let bands = normalize_bands(energy, self.params.band_gain);

        let out: Vec<Bucket> = bands
            .into_iter()
            .map(|b| Bucket {
                low: b[0],
                mid: b[1],
                high: b[2],
            })
            .collect();
        Waveform::from(out)
    }
}

fn hann_window(size: usize) -> Vec<f32> {
    if size <= 1 {
        return vec![1.0; size];
    }
    let denom = (size - 1).to_f32().unwrap_or(1.0);
    let scale = std::f32::consts::TAU / denom;
    (0..size)
        .map(|n| {
            let phase = scale * n.to_f32().unwrap_or(0.0);
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

fn crossover_bin(hz: f32, bin_hz: f32, bins: usize) -> usize {
    if bin_hz <= 0.0 {
        return bins;
    }
    let idx = (hz / bin_hz).to_usize().unwrap_or(bins);
    idx.min(bins)
}

/// Per-bucket band energies -> heights: `sqrt` to magnitude, apply per-band
/// gain, then divide all three by one shared global max so the relative band
/// sizes (the loudness tilt) survive while every value lands in `[0, 1]`.
fn normalize_bands(energy: Vec<[f32; 3]>, gain: [f32; 3]) -> Vec<[f32; 3]> {
    let mut mags: Vec<[f32; 3]> = energy
        .into_iter()
        .map(|e| {
            [
                e[0].sqrt() * gain[0],
                e[1].sqrt() * gain[1],
                e[2].sqrt() * gain[2],
            ]
        })
        .collect();

    let max = mags
        .iter()
        .flat_map(|m| m.iter().copied())
        .fold(0.0_f32, f32::max);
    if max > 0.0 {
        let inv = 1.0 / max;
        for m in &mut mags {
            m[0] *= inv;
            m[1] *= inv;
            m[2] *= inv;
        }
    }
    mags
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use num_traits::cast::ToPrimitive;

    use super::{AnalysisParams, WaveformAnalyzer};
    use crate::waveform::bucket::Bucket;

    struct Consts;

    impl Consts {
        const SR: u32 = 44_100;
        const EPS: f32 = 1e-6;
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() <= Consts::EPS
    }

    /// Tallest band of a bucket (the outer hull height).
    fn peak(b: &Bucket) -> f32 {
        b.low.max(b.mid).max(b.high)
    }

    fn analyzer(params: AnalysisParams) -> WaveformAnalyzer {
        WaveformAnalyzer::new(Consts::SR, params)
    }

    /// Unity gain so normalization/routing tests aren't coupled to the
    /// perceptual band balance.
    fn flat() -> AnalysisParams {
        AnalysisParams {
            band_gain: [1.0; 3],
            ..AnalysisParams::default()
        }
    }

    fn sine(freq: f32, samples: usize) -> Vec<f32> {
        let step = std::f32::consts::TAU * freq / Consts::SR.to_f32().unwrap_or(1.0);
        (0..samples)
            .map(|n| (step * n.to_f32().unwrap_or(0.0)).sin())
            .collect()
    }

    #[kithara::test]
    fn no_frames_finalizes_empty() {
        let wave = analyzer(AnalysisParams::default()).finalize(8);
        assert!(wave.is_empty());
    }

    #[kithara::test]
    fn zero_buckets_finalizes_empty() {
        let mut a = analyzer(AnalysisParams::default());
        a.push_interleaved(&[0.5; 8192], 1);
        assert!(a.finalize(0).is_empty());
    }

    #[kithara::test]
    fn loudest_band_normalises_to_one() {
        // Broadband square wave: after shared normalization the single loudest
        // band-bucket reaches exactly 1.0.
        let pcm: Vec<f32> = (0..16_384)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let mut a = analyzer(flat());
        a.push_interleaved(&pcm, 1);
        let wave = a.finalize(10);
        assert_eq!(wave.len(), 10);
        let max = wave.buckets().iter().map(peak).fold(0.0_f32, f32::max);
        assert!(
            approx(max, 1.0),
            "loudest band must normalise to 1.0, got {max}"
        );
    }

    #[kithara::test]
    fn silence_is_all_zero() {
        let mut a = analyzer(AnalysisParams::default());
        a.push_interleaved(&[0.0; 16_384], 1);
        let wave = a.finalize(8);
        assert_eq!(wave.len(), 8);
        for b in wave.buckets() {
            assert_eq!(*b, Bucket::default(), "silence -> all-zero bucket: {b:?}");
        }
    }

    #[kithara::test]
    fn fewer_frames_than_buckets_stays_finite() {
        let mut a = analyzer(AnalysisParams::default());
        a.push_interleaved(&[0.5, -0.5, 0.25], 1);
        let wave = a.finalize(5);
        assert_eq!(wave.len(), 5);
        for b in wave.buckets() {
            for v in [b.low, b.mid, b.high] {
                assert!(
                    v.is_finite() && (0.0..=1.0).contains(&v),
                    "band must stay finite in [0,1]: {b:?}"
                );
            }
        }
    }

    #[kithara::test]
    fn stereo_downmix_is_channel_mean() {
        // L=1, R=-1 cancels to mono 0 -> silence.
        let mut pcm = Vec::with_capacity(16_384 * 2);
        for _ in 0..16_384 {
            pcm.push(1.0);
            pcm.push(-1.0);
        }
        let mut a = analyzer(AnalysisParams::default());
        a.push_interleaved(&pcm, 2);
        let wave = a.finalize(4);
        for b in wave.buckets() {
            assert_eq!(*b, Bucket::default(), "cancelling stereo -> silence: {b:?}");
        }
    }

    #[kithara::test]
    fn deterministic_for_same_input() {
        let pcm = sine(440.0, 16_384);
        let run = || {
            let mut a = analyzer(AnalysisParams::default());
            a.push_interleaved(&pcm, 1);
            a.finalize(64).buckets().to_vec()
        };
        assert_eq!(run(), run(), "same PCM must produce the same waveform");
    }

    fn dominant(freq: f32) -> Bucket {
        // Floor disabled so routing isn't coupled to the gate; unity gain so it
        // isn't coupled to the perceptual balance.
        let params = AnalysisParams {
            energy_floor: 0.0,
            ..flat()
        };
        let pcm = sine(freq, 16_384);
        let mut a = analyzer(params);
        a.push_interleaved(&pcm, 1);
        a.finalize(4)
            .buckets()
            .iter()
            .copied()
            .find(|b| peak(b) > 0.0)
            .unwrap_or_default()
    }

    #[kithara::test]
    fn low_frequency_lands_in_low_band() {
        let b = dominant(80.0);
        assert!(
            b.low > b.mid && b.low > b.high,
            "80 Hz must be low-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn mid_frequency_lands_in_mid_band() {
        let b = dominant(1_000.0);
        assert!(
            b.mid > b.low && b.mid > b.high,
            "1 kHz must be mid-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn high_frequency_lands_in_high_band() {
        let b = dominant(10_000.0);
        assert!(
            b.high > b.low && b.high > b.mid,
            "10 kHz must be high-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn full_spectrum_track_has_no_color_gaps() {
        // Regression for a band series coarser than the bucket count, which left
        // columns with no bar. Every column of a full-spectrum track must carry
        // at least one nonzero band.
        let sr = Consts::SR.to_f32().unwrap_or(1.0);
        let frames = 45 * Consts::SR.to_usize().unwrap_or(0);
        let l = std::f32::consts::TAU * 80.0 / sr;
        let m = std::f32::consts::TAU * 1_000.0 / sr;
        let h = std::f32::consts::TAU * 10_000.0 / sr;
        let pcm: Vec<f32> = (0..frames)
            .map(|n| {
                let t = n.to_f32().unwrap_or(0.0);
                0.3 * ((l * t).sin() + (m * t).sin() + (h * t).sin())
            })
            .collect();
        let mut a = analyzer(AnalysisParams::default());
        a.push_interleaved(&pcm, 1);
        let wave = a.finalize(1500);
        let gaps = wave.buckets().iter().filter(|b| peak(b) <= 0.0).count();
        assert_eq!(gaps, 0, "every column must carry a bar");
    }
}
