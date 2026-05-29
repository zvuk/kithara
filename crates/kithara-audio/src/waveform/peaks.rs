use num_traits::cast::ToPrimitive;

use super::envelope::Envelope;

/// Peak-normalised mono envelope over interleaved f32 PCM: downmix to
/// mono, split into `buckets` ranges, take each range's max abs sample,
/// normalise so the loudest bucket is `1.0`. Empty when there are no
/// frames or `buckets`/`channels` is zero; all-zero for silent input.
#[must_use]
pub(crate) fn compute_peaks(pcm: &[f32], channels: usize, buckets: usize) -> Vec<f32> {
    if buckets == 0 || channels == 0 {
        return Vec::new();
    }
    let frames = pcm.len() / channels;
    if frames == 0 {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; buckets];
    let mut global = 0.0_f32;
    let inv_channels = 1.0_f32 / channels.to_f32().unwrap_or(1.0);

    for (b, slot) in out.iter_mut().enumerate() {
        let start = b * frames / buckets;
        let end = (b + 1) * frames / buckets;
        let mut peak = 0.0_f32;
        for f in start..end {
            let base = f * channels;
            let mut sum = 0.0_f32;
            for c in 0..channels {
                sum += pcm[base + c];
            }
            let mono = (sum * inv_channels).abs();
            if mono > peak {
                peak = mono;
            }
        }
        *slot = peak;
        if peak > global {
            global = peak;
        }
    }

    if global > 0.0 {
        let inv = 1.0_f32 / global;
        for v in &mut out {
            *v *= inv;
        }
    }
    out
}

/// Streaming peak accumulator: folds frames into a fixed `cap`-bin pool,
/// max-merging adjacent bins when full so memory stays `O(cap)` and the
/// envelope stays uniform regardless of total length.
pub struct PeakAccumulator {
    bins: Vec<f32>,
    cap: usize,
    frames_per_bin: u64,
    frames_in_last: u64,
}

impl PeakAccumulator {
    /// `cap` is the raw bin pool size; pick well above the final bucket
    /// count for headroom (it is rounded up to an even number, min 2).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(2);
        let cap = cap + (cap % 2);
        Self {
            bins: Vec::with_capacity(cap),
            cap,
            frames_per_bin: 1,
            frames_in_last: 0,
        }
    }

    /// Fold one interleaved chunk into the pool.
    pub fn push_interleaved(&mut self, pcm: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let inv_channels = 1.0_f32 / channels.to_f32().unwrap_or(1.0);
        let frames = pcm.len() / channels;
        for f in 0..frames {
            let base = f * channels;
            let mut sum = 0.0_f32;
            for c in 0..channels {
                sum += pcm[base + c];
            }
            self.push_frame((sum * inv_channels).abs());
        }
    }

    fn push_frame(&mut self, amp: f32) {
        if self.frames_in_last == 0 {
            if self.bins.len() == self.cap {
                self.compress();
            }
            self.bins.push(amp);
        } else if let Some(last) = self.bins.last_mut() {
            *last = last.max(amp);
        }
        self.frames_in_last += 1;
        if self.frames_in_last == self.frames_per_bin {
            self.frames_in_last = 0;
        }
    }

    fn compress(&mut self) {
        self.bins = self
            .bins
            .chunks_exact(2)
            .map(|pair| pair[0].max(pair[1]))
            .collect();
        self.frames_per_bin *= 2;
    }

    /// Downsample + peak-normalise to an [`Envelope`] of `buckets` values.
    #[must_use]
    pub fn finalize(&self, buckets: usize) -> Envelope {
        Envelope::from_peaks(compute_peaks(&self.bins, 1, buckets))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{PeakAccumulator, compute_peaks};

    const EPS: f32 = 1e-6;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() <= EPS
    }

    #[kithara::test]
    fn buckets_zero_returns_empty() {
        assert!(compute_peaks(&[0.1, 0.2, 0.3], 1, 0).is_empty());
    }

    #[kithara::test]
    fn channels_zero_returns_empty() {
        assert!(compute_peaks(&[0.1, 0.2], 0, 4).is_empty());
    }

    #[kithara::test]
    fn empty_pcm_returns_empty() {
        assert!(compute_peaks(&[], 2, 8).is_empty());
    }

    #[kithara::test]
    fn full_scale_mono_normalises_to_one() {
        let pcm: Vec<f32> = (0..1000)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let env = compute_peaks(&pcm, 1, 10);
        assert_eq!(env.len(), 10, "one value per bucket");
        for v in env {
            assert!(
                approx(v, 1.0),
                "full-scale signal must normalise to 1.0, got {v}"
            );
        }
    }

    #[kithara::test]
    fn silence_yields_zero_buckets_not_empty() {
        let env = compute_peaks(&[0.0; 800], 1, 8);
        assert_eq!(
            env.len(),
            8,
            "silent track still has frames, so buckets stay"
        );
        assert!(
            env.iter().all(|&v| approx(v, 0.0)),
            "silent input must yield all-zero buckets, not NaN/empty"
        );
    }

    #[kithara::test]
    fn stereo_downmix_is_channel_mean() {
        // First 400 frames: L=R=1.0 -> mono mean 1.0.
        // Last 400 frames: L=1.0, R=-1.0 -> mono mean 0.0.
        let mut pcm = Vec::with_capacity(800 * 2);
        for _ in 0..400 {
            pcm.push(1.0);
            pcm.push(1.0);
        }
        for _ in 0..400 {
            pcm.push(1.0);
            pcm.push(-1.0);
        }
        let env = compute_peaks(&pcm, 2, 2);
        assert_eq!(env.len(), 2);
        assert!(approx(env[0], 1.0), "loud half -> 1.0, got {}", env[0]);
        assert!(
            approx(env[1], 0.0),
            "cancelling stereo half must downmix to 0.0, got {}",
            env[1]
        );
    }

    #[kithara::test]
    fn peak_normalisation_preserves_relative_shape() {
        let mut pcm = vec![0.25_f32; 500];
        pcm.extend(std::iter::repeat_n(1.0_f32, 500));
        let env = compute_peaks(&pcm, 1, 2);
        assert_eq!(env.len(), 2);
        assert!(
            approx(env[0], 0.25),
            "quiet bucket must keep its 0.25 ratio after peak-normalisation, got {}",
            env[0]
        );
        assert!(approx(env[1], 1.0), "loud bucket -> 1.0, got {}", env[1]);
    }

    #[kithara::test]
    fn fewer_frames_than_buckets_does_not_panic() {
        let env = compute_peaks(&[0.5, -0.5, 0.25], 1, 5);
        assert_eq!(env.len(), 5);
        assert!(
            env.iter()
                .all(|&v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "all buckets must stay finite and within [0, 1]: {env:?}"
        );
    }

    #[kithara::test]
    fn deterministic_for_same_input() {
        let pcm: Vec<f32> = (0..2048_u16)
            .map(|i| (f32::from(i) * 0.001).sin())
            .collect();
        assert_eq!(
            compute_peaks(&pcm, 2, 256),
            compute_peaks(&pcm, 2, 256),
            "same PCM must produce the same envelope"
        );
    }

    #[kithara::test]
    fn accumulator_no_frames_finalizes_empty() {
        let acc = PeakAccumulator::new(64);
        assert!(acc.finalize(8).is_empty());
    }

    #[kithara::test]
    fn accumulator_uniform_signal_is_flat() {
        let mut acc = PeakAccumulator::new(256);
        acc.push_interleaved(&[0.5_f32; 10_000], 1);
        let env = acc.finalize(10);
        assert_eq!(env.len(), 10);
        assert!(
            env.iter().all(|&v| approx(v, 1.0)),
            "uniform signal must be flat at 1.0 after peak-normalisation: {env:?}"
        );
    }

    #[kithara::test]
    fn accumulator_compression_preserves_loud_transient() {
        // Tiny cap forces repeated max-merges; the single loud frame must
        // survive every compression.
        let mut acc = PeakAccumulator::new(8);
        let mut pcm = vec![0.1_f32; 5000];
        pcm[2500] = 1.0;
        acc.push_interleaved(&pcm, 1);
        let env = acc.finalize(4);
        assert_eq!(env.len(), 4);
        let max = env.iter().copied().fold(0.0_f32, f32::max);
        assert!(
            approx(max, 1.0),
            "loud transient must survive merges, got max {max}"
        );
        assert!(
            env.iter().any(|&v| v < 0.9),
            "quiet regions must stay below the transient: {env:?}"
        );
    }

    #[kithara::test]
    fn accumulator_matches_compute_peaks_without_compression() {
        let pcm: Vec<f32> = (0..4096_u16).map(|i| (f32::from(i) * 0.01).sin()).collect();
        let mut acc = PeakAccumulator::new(1 << 14);
        acc.push_interleaved(&pcm, 2);
        assert_eq!(
            &acc.finalize(128)[..],
            compute_peaks(&pcm, 2, 128).as_slice(),
            "with no compression the accumulator must match the pure path"
        );
    }
}
