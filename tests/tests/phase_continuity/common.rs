use std::{
    f64::consts::{PI, TAU},
    fmt,
    time::Duration,
};

use kithara::{
    audio::{Audio, ReadOutcome},
    events::EventBus,
    stream::{Stream, StreamType},
};
use kithara_integration_tests::Xorshift64;
use tracing::{info, warn};

pub(crate) const SAMPLE_RATE: u32 = 44_100;
pub(crate) const CHANNELS: u16 = 2;
pub(crate) const FREQ_HZ: f64 = 440.0;
pub(crate) const STREAM_FRAMES: u64 = (SAMPLE_RATE as u64) * 60;
pub(crate) const TOLERANCE_SAMPLES: f64 = 0.5;
/// Minimum fitted amplitude for a scan window to carry a meaningful phase.
/// The test sine is full-scale (amp ≈ 1.0, ≥ 0.5 even through lossy AAC);
/// a window over codec priming/leading silence fits amp ≈ 1e-4, where the
/// phase is pure fit noise. Anchoring continuity on such a window compares
/// real signal against noise and false-positives at random — so windows
/// below this floor are skipped entirely (no anchor, no comparison).
pub(crate) const MIN_SIGNAL_AMP: f64 = 0.1;
/// Phase fit window. Needs ≥ one full period of the test sine
/// (≈100 samples @ 440 Hz / 44.1 kHz) so DFT correlation leakage
/// (`Σ cos(2δk+φ)` term) cancels to <1e-3 sample of bias. 128 samples
/// = 2.9 ms post-seek read budget — well within decoder warm-up.
pub(crate) const READ_FRAMES_AFTER_SEEK: usize = 128;
pub(crate) const READ_PENDING_RETRIES: usize = 4096;
pub(crate) const E2E_SCAN_INTERVAL_FRAMES: u64 = SAMPLE_RATE as u64 / 8;
pub(crate) const SAFETY_END_MARGIN_FRAMES: u64 = 4096;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SinePhaseSpec {
    pub(crate) freq_hz: f64,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
}

impl SinePhaseSpec {
    pub(crate) const fn default_440() -> Self {
        Self {
            freq_hz: FREQ_HZ,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        }
    }

    pub(crate) fn delta_rad_per_sample(&self) -> f64 {
        TAU * self.freq_hz / f64::from(self.sample_rate)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PhaseDrift {
    pub(crate) at_frame: u64,
    pub(crate) expected_rad: f64,
    pub(crate) measured_rad: f64,
    pub(crate) jump_samples: f64,
    pub(crate) amplitude: f64,
}

impl fmt::Display for PhaseDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "phase drift at frame {}: expected={:.4}rad measured={:.4}rad jump={:.3} samples (amp={:.3})",
            self.at_frame, self.expected_rad, self.measured_rad, self.jump_samples, self.amplitude,
        )
    }
}

pub(crate) fn wrap_pi(x: f64) -> f64 {
    let mut v = x % TAU;
    if v > PI {
        v -= TAU;
    } else if v < -PI {
        v += TAU;
    }
    v
}

/// Estimate sine phase + amplitude via least-squares fit of model
/// `s[k] = a · sin(δk) + b · cos(δk)` over the window. Returns
/// `(phase_at_first_sample_rad, amplitude)` where the signal is
/// reconstructed as `s[k] = A · sin(δk + φ)`, A = √(a²+b²), φ = atan2(b, a).
///
/// LS via the closed-form 2×2 normal equations:
///
/// ```text
///   [Σsin²  Σsincos] [a]   [Σs·sin]
///   [Σsincos Σcos² ] [b] = [Σs·cos]
/// ```
///
/// Unlike single-bin DFT correlation (which assumes `Σsin² ≈ N/2`),
/// the explicit matrix inverse cancels leakage even when the window
/// covers a non-integer number of periods (e.g. N=128, freq=440Hz,
/// sr=44.1k → 1.28 periods). Bias for a clean sine is < 1e-9 samples
/// at f64 precision. Robust to ~5–10% amplitude quantization noise
/// (lossy AAC/MP3).
pub(crate) fn measure_phase_rad_window(mono: &[f64], delta_rad: f64) -> (f64, f64) {
    assert!(mono.len() >= 2, "phase window needs ≥2 samples");
    let mut ss = 0.0_f64;
    let mut cc = 0.0_f64;
    let mut sc = 0.0_f64;
    let mut ps = 0.0_f64;
    let mut pc = 0.0_f64;
    for (k, &s) in mono.iter().enumerate() {
        let angle = delta_rad * k as f64;
        let sin_k = angle.sin();
        let cos_k = angle.cos();
        ss += sin_k * sin_k;
        cc += cos_k * cos_k;
        sc += sin_k * cos_k;
        ps += s * sin_k;
        pc += s * cos_k;
    }
    let det = ss * cc - sc * sc;
    let a = (cc * ps - sc * pc) / det;
    let b = (-sc * ps + ss * pc) / det;
    let phase = b.atan2(a);
    let amp = (a * a + b * b).sqrt();
    (phase, amp)
}

fn read_block<T>(audio: &mut Audio<Stream<T>>, buf: &mut [f32], label: &str) -> Option<usize>
where
    T: StreamType<Events = EventBus>,
{
    let mut retries = 0usize;
    loop {
        match audio.read(buf) {
            Ok(ReadOutcome::Frames { count, .. }) => return Some(count.get()),
            Ok(ReadOutcome::Pending { .. }) => {
                retries += 1;
                assert!(
                    retries < READ_PENDING_RETRIES,
                    "{label}: pending exceeded {READ_PENDING_RETRIES} retries (decoder starved)",
                );
            }
            Ok(ReadOutcome::Eof { .. }) => return None,
            Err(e) => panic!("{label}: read error: {e}"),
        }
    }
}

/// Check phase continuity vs the **previous** scan, not a fixed anchor.
///
/// "Continuity" = no sudden glitch (sample drop, fragment skip, decoder
/// restart). A lossy codec (AAC/MP3) has a small sub-sample MDCT phase
/// wobble (~±0.1 sample per scan window) that drifts as a random walk
/// vs the original signal — measuring against a fixed anchor would
/// false-positive on that wobble. Measuring against the **last** scan
/// captures only the inter-scan glitch, which is what we audibly care
/// about. The last scan slides forward after every check.
fn check_against_previous(
    last: &mut Option<(u64, f64)>,
    consumed: u64,
    buf: &[f32],
    chan: usize,
    sine: SinePhaseSpec,
    label: &str,
) -> Option<PhaseDrift> {
    let frames_in_buf = buf.len() / chan;
    if frames_in_buf < 2 {
        return None;
    }
    let mono: Vec<f64> = (0..frames_in_buf)
        .map(|f| f64::from(buf[f * chan]))
        .collect();
    let delta = sine.delta_rad_per_sample();
    let (measured, amp) = measure_phase_rad_window(&mono, delta);
    if amp < MIN_SIGNAL_AMP {
        info!(consumed, amp, "{label} skipped (no signal)");
        return None;
    }
    let result = match *last {
        None => {
            info!(consumed, measured, amp, "{label} first scan");
            None
        }
        Some((prev_frame, prev_phase)) => {
            let frame_diff = consumed as i64 - prev_frame as i64;
            let expected = wrap_pi(prev_phase + delta * frame_diff as f64);
            let phase_diff = wrap_pi(measured - expected);
            let jump_samples = phase_diff / delta;
            if jump_samples.abs() > TOLERANCE_SAMPLES {
                let drift = PhaseDrift {
                    at_frame: consumed,
                    expected_rad: expected,
                    measured_rad: measured,
                    jump_samples,
                    amplitude: amp,
                };
                warn!(consumed, %drift, "{label} drift");
                Some(drift)
            } else {
                info!(consumed, jump_samples, amp, "{label} aligned");
                None
            }
        }
    };
    *last = Some((consumed, measured));
    result
}

pub(crate) fn e2e_phase_scan<T>(
    audio: &mut Audio<Stream<T>>,
    sine: SinePhaseSpec,
    total_frames_truth: u64,
) -> Vec<PhaseDrift>
where
    T: StreamType<Events = EventBus>,
{
    audio
        .seek(Duration::from_secs_f64(0.0))
        .expect("rewind for e2e");
    let chan = sine.channels as usize;
    let total_frames = total_frames_truth.saturating_sub(SAFETY_END_MARGIN_FRAMES);
    let want_samples = READ_FRAMES_AFTER_SEEK * chan;
    let mut buf = vec![0.0_f32; want_samples];
    let mut consumed: u64 = 0;
    let mut next_scan_at: u64 = E2E_SCAN_INTERVAL_FRAMES;
    let mut anchor: Option<(u64, f64)> = None;
    let mut drifts: Vec<PhaseDrift> = Vec::new();

    while consumed < total_frames {
        let Some(n) = read_block(audio, &mut buf, &format!("e2e@frame{consumed}")) else {
            break;
        };
        let frames_this_read = (n / chan) as u64;
        if consumed >= next_scan_at
            && let Some(drift) =
                check_against_previous(&mut anchor, consumed, &buf[..n], chan, sine, "e2e phase")
        {
            drifts.push(drift);
        }
        if consumed >= next_scan_at {
            next_scan_at = next_scan_at.saturating_add(E2E_SCAN_INTERVAL_FRAMES);
        }
        consumed += frames_this_read;
    }
    drifts
}

pub(crate) fn seek_phase_scan<T, F>(
    audio: &mut Audio<Stream<T>>,
    sine: SinePhaseSpec,
    total_secs: f64,
    seek_count: usize,
    rng_seed: u64,
    mut on_seek: F,
) -> Vec<PhaseDrift>
where
    T: StreamType<Events = EventBus>,
    F: FnMut(usize),
{
    let chan = sine.channels as usize;
    let max_seek_secs = total_secs - 1.0;
    let want_samples = READ_FRAMES_AFTER_SEEK * chan;
    let mut buf = vec![0.0_f32; want_samples];
    let mut rng = Xorshift64::new(rng_seed);
    let mut anchor: Option<(u64, f64)> = None;
    let mut drifts: Vec<PhaseDrift> = Vec::new();

    for i in 0..seek_count {
        let pos_secs = rng.range_f64(0.5, max_seek_secs);
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|e| panic!("seek #{i} to {pos_secs:.3}s failed: {e}"));
        on_seek(i);
        let label = format!("seek#{i}@{pos_secs:.3}s");
        let n = read_block(audio, &mut buf, &label).unwrap_or_else(|| {
            panic!("{label}: unexpected EOF immediately after seek");
        });
        let consumed = (pos_secs * f64::from(SAMPLE_RATE)).round() as u64;
        if let Some(drift) =
            check_against_previous(&mut anchor, consumed, &buf[..n], chan, sine, &label)
        {
            drifts.push(drift);
        }
    }
    drifts
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn synth(spec: SinePhaseSpec, start_frame: u64, frames: usize, amp: f32) -> Vec<f32> {
        let chan = spec.channels as usize;
        let mut out = vec![0.0_f32; frames * chan];
        let d = spec.delta_rad_per_sample();
        for f in 0..frames {
            let phase = d * (start_frame + f as u64) as f64;
            let v = amp * phase.sin() as f32;
            for c in 0..chan {
                out[f * chan + c] = v;
            }
        }
        out
    }

    #[kithara::test]
    fn measure_matches_predicted_for_clean_sine() {
        let spec = SinePhaseSpec::default_440();
        let pcm = synth(spec, 12_345, READ_FRAMES_AFTER_SEEK, 0.8);
        let chan = spec.channels as usize;
        let mono: Vec<f64> = (0..pcm.len() / chan)
            .map(|f| f64::from(pcm[f * chan]))
            .collect();
        let delta = spec.delta_rad_per_sample();
        let (measured, amp) = measure_phase_rad_window(&mono, delta);
        let predicted = wrap_pi(delta * 12_345.0);
        let diff_samples = wrap_pi(measured - predicted) / delta;
        assert!(diff_samples.abs() < 1e-3, "diff {diff_samples}");
        assert!((amp - 0.8).abs() < 0.01, "amp {amp}");
    }

    fn xorshift_unit(state: &mut u64) -> f64 {
        let mut s = *state;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *state = s;
        (s as f64 / u64::MAX as f64) * 2.0 - 1.0
    }

    /// Returns Gaussian noise with the requested standard deviation,
    /// via Box-Muller from two uniform draws.
    fn gauss_noise(state: &mut u64, sigma: f64) -> f64 {
        let u1 = (xorshift_unit(state) + 1.0) * 0.5;
        let u2 = (xorshift_unit(state) + 1.0) * 0.5;
        let u1 = u1.max(1e-12);
        sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }

    /// Verifies LS fit error matches theory σ_s ≈ √(2/N)·(σ_n/A)/δ
    /// for additive white Gaussian noise. If this passes, the LS fit is
    /// correct; any larger error from real AAC must come from non-white
    /// noise structure, not a bug in the estimator.
    #[kithara::test]
    fn measure_error_matches_theory_for_white_noise() {
        let spec = SinePhaseSpec::default_440();
        let chan = spec.channels as usize;
        let delta = spec.delta_rad_per_sample();
        let sigma_n = 0.05_f64;
        let amplitude = 1.0_f64;
        let n = READ_FRAMES_AFTER_SEEK;
        let theory_sigma_samples = (2.0_f64 / n as f64).sqrt() * (sigma_n / amplitude) / delta;
        let trials = 400;
        let mut state: u64 = 0xC0FFEE_BADD_F00D_u64;
        let mut sum_sq = 0.0_f64;
        for trial in 0..trials {
            let start = 1000 + trial as u64 * 137;
            let pcm = synth(spec, start, n, amplitude as f32);
            let mono: Vec<f64> = (0..pcm.len() / chan)
                .map(|f| f64::from(pcm[f * chan]) + gauss_noise(&mut state, sigma_n))
                .collect();
            let (measured, _amp) = measure_phase_rad_window(&mono, delta);
            let predicted = wrap_pi(delta * start as f64);
            let diff_samples = wrap_pi(measured - predicted) / delta;
            sum_sq += diff_samples * diff_samples;
        }
        let observed_rms = (sum_sq / trials as f64).sqrt();
        let ratio = observed_rms / theory_sigma_samples;
        assert!(
            (0.85..1.15).contains(&ratio),
            "LS fit precision off vs theory: theory σ={theory_sigma_samples:.4} obs RMS={observed_rms:.4} ratio={ratio:.2}",
        );
    }

    #[kithara::test]
    fn window_robust_to_amplitude_noise() {
        let spec = SinePhaseSpec::default_440();
        let chan = spec.channels as usize;
        let pcm = synth(spec, 50_000, READ_FRAMES_AFTER_SEEK, 1.0);
        let delta = spec.delta_rad_per_sample();
        let mono: Vec<f64> = (0..pcm.len() / chan)
            .map(|f| {
                let s = f64::from(pcm[f * chan]);
                let jitter = if f % 2 == 0 { 0.95 } else { 1.05 };
                s * jitter
            })
            .collect();
        let (measured, _amp) = measure_phase_rad_window(&mono, delta);
        let predicted = wrap_pi(delta * 50_000.0);
        let diff_samples = wrap_pi(measured - predicted) / delta;
        assert!(
            diff_samples.abs() < 0.1,
            "5% amp noise should not cause >0.1 sample fit error, got {diff_samples}",
        );
    }

    #[kithara::test]
    fn anchor_detects_sample_drop_of_two() {
        let spec = SinePhaseSpec::default_440();
        let pcm0 = synth(spec, 1000, READ_FRAMES_AFTER_SEEK, 0.8);
        let pcm_dropped = synth(spec, 2000 + 2, READ_FRAMES_AFTER_SEEK, 0.8);
        let mut anchor: Option<(u64, f64)> = None;
        let chan = spec.channels as usize;
        assert!(check_against_previous(&mut anchor, 1000, &pcm0, chan, spec, "t").is_none());
        let drift = check_against_previous(&mut anchor, 2000, &pcm_dropped, chan, spec, "t")
            .expect("should detect drift");
        assert!(
            (drift.jump_samples - 2.0).abs() < 1e-3,
            "{}",
            drift.jump_samples
        );
    }
}
