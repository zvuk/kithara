use std::f64::consts::TAU;

use num_traits::cast;

/// Period of the saw-tooth in frames: one full 16-bit sweep.
pub const SAW_PERIOD: usize = 65_536;

/// The tone every generated WAV fixture carries unless it says otherwise.
pub const TONE: Wave = Wave::sine(440.0);

/// How a [`Wave::Sweep`] interpolates between its two frequencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SweepMode {
    /// Frequency rises by a constant number of Hz per second.
    Linear,
    /// Frequency rises by a constant factor per second.
    Log,
}

/// Waveform a fixture carries, sampled per frame.
///
/// Every sample is deterministic in `(frame, sample_rate)`: the same pair
/// always yields the same value, which is what lets one body be rendered in
/// pieces, at an offset, or twice.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Wave {
    /// Tone bursts at `hz` every `period_frames`, each fading out over
    /// `burst_frames`.
    #[non_exhaustive]
    Clicks {
        hz: f64,
        period_frames: usize,
        burst_frames: usize,
    },
    /// Ascending saw-tooth: frame 0 is `i16::MIN`, frame 65535 is `i16::MAX`.
    Sawtooth,
    /// Descending saw-tooth: frame 0 is `i16::MAX`, frame 65535 is `i16::MIN`.
    SawtoothDescending,
    /// [`Self::Sawtooth`] offset by half a period.
    SawtoothShifted,
    /// Digital silence.
    Silence,
    /// Sine at `hz`, peaking at `peak`.
    Sine { hz: f64, peak: i16 },
    /// Phase-continuous chirp from `start_hz` to `end_hz` over `total_frames`,
    /// silent past its end. Build it with [`Wave::sweep`].
    #[non_exhaustive]
    Sweep {
        start_hz: f64,
        end_hz: f64,
        total_frames: usize,
        mode: SweepMode,
    },
}

impl Wave {
    /// A repeating tone burst: `hz` fading to silence across `burst_frames`,
    /// once every `period_frames`.
    ///
    /// # Panics
    /// Panics unless `hz` is finite and positive and the burst fits one period.
    #[must_use]
    pub fn clicks(hz: f64, period_frames: usize, burst_frames: usize) -> Self {
        assert!(
            hz.is_finite() && hz > 0.0,
            "a click burst carries a finite positive frequency, not {hz}"
        );
        assert!(burst_frames > 0, "a click burst spans at least one frame");
        assert!(
            burst_frames <= period_frames,
            "a burst of {burst_frames} frames does not fit a period of {period_frames}"
        );

        Self::Clicks {
            hz,
            period_frames,
            burst_frames,
        }
    }

    /// One 16-bit sample of this waveform.
    #[must_use]
    pub fn sample(self, frame: usize, sample_rate: u32) -> i16 {
        match self {
            Self::Clicks {
                hz,
                period_frames,
                burst_frames,
            } => {
                let into = frame % period_frames;
                if into >= burst_frames {
                    return 0;
                }
                let at = seconds(into, sample_rate);
                let fade = 1.0 - at / seconds(burst_frames, sample_rate);
                quantize(fade * f64::sin(TAU * hz * at), i16::MAX)
            }
            Self::Sawtooth => saw(frame),
            Self::SawtoothDescending => saw(SAW_PERIOD - 1 - frame % SAW_PERIOD),
            Self::SawtoothShifted => saw(frame + SAW_PERIOD / 2),
            Self::Silence => 0,
            Self::Sine { hz, peak } => {
                quantize(f64::sin(TAU * hz * seconds(frame, sample_rate)), peak)
            }
            Self::Sweep {
                start_hz,
                end_hz,
                total_frames,
                mode,
            } => {
                if frame >= total_frames {
                    return 0;
                }
                let phase = sweep_phase(
                    seconds(frame, sample_rate),
                    seconds(total_frames, sample_rate),
                    start_hz,
                    end_hz,
                    mode,
                );
                quantize(f64::sin(phase), i16::MAX)
            }
        }
    }

    /// A chirp between two frequencies.
    ///
    /// # Panics
    ///
    /// Panics unless both frequencies are finite and positive and the span is
    /// non-empty. A logarithmic sweep additionally needs two distinct
    /// frequencies: its phase divides by the ratio's logarithm.
    /// A full-scale sine at `hz`.
    #[must_use]
    pub const fn sine(hz: f64) -> Self {
        Self::Sine { hz, peak: i16::MAX }
    }

    /// A chirp from `start_hz` to `end_hz` across `total_frames`.
    ///
    /// # Panics
    ///
    /// Panics when a frequency is not finite and positive, when the sweep spans
    /// no frames, or when a logarithmic sweep is given one frequency twice.
    #[must_use]
    pub fn sweep(start_hz: f64, end_hz: f64, total_frames: usize, mode: SweepMode) -> Self {
        assert!(
            start_hz.is_finite() && start_hz > 0.0,
            "a sweep starts at a finite positive frequency, not {start_hz}"
        );
        assert!(
            end_hz.is_finite() && end_hz > 0.0,
            "a sweep ends at a finite positive frequency, not {end_hz}"
        );
        assert!(total_frames > 0, "a sweep spans at least one frame");
        assert!(
            mode != SweepMode::Log || start_hz != end_hz,
            "a logarithmic sweep needs two distinct frequencies, both are {start_hz}"
        );

        Self::Sweep {
            start_hz,
            end_hz,
            total_frames,
            mode,
        }
    }
}

/// Position of `frame` in seconds.
fn seconds(frame: usize, sample_rate: u32) -> f64 {
    let frame = f64::from(u32::try_from(frame).expect("invariant: a fixture is under 2^32 frames"));
    frame / f64::from(sample_rate)
}

/// Scales a unit-range value to `peak` and rounds it into 16-bit.
fn quantize(unit: f64, peak: i16) -> i16 {
    let scaled = (unit * f64::from(peak))
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX));
    cast(scaled).unwrap_or(0)
}

/// One frame of an ascending saw-tooth.
fn saw(frame: usize) -> i16 {
    let step = i32::try_from(frame % SAW_PERIOD).expect("invariant: a saw period fits i32");
    let value = step + i32::from(i16::MIN);
    cast(value).expect("invariant: one saw period spans exactly the 16-bit range")
}

/// Accumulated phase of a chirp, integrated analytically so that reading a
/// frame in isolation gives the same value as reading it in sequence.
fn sweep_phase(at: f64, span: f64, start_hz: f64, end_hz: f64, mode: SweepMode) -> f64 {
    match mode {
        SweepMode::Linear => {
            let slope = (end_hz - start_hz) / (2.0 * span);
            TAU * (slope * at).mul_add(at, start_hz * at)
        }
        SweepMode::Log => {
            let rate = f64::ln(end_hz / start_hz) / span;
            TAU * start_hz * (f64::exp(rate * at) - 1.0) / rate
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use kithara_test_utils::kithara;

    use super::*;

    mod consts {
        pub(super) const SAMPLE_RATE: u32 = 48_000;
    }

    fn render(wave: Wave, frames: usize) -> Vec<i16> {
        (0..frames)
            .map(|frame| wave.sample(frame, consts::SAMPLE_RATE))
            .collect()
    }

    fn zero_crossings(samples: &[i16]) -> usize {
        let mut crossings = 0usize;
        let mut previous = 0i8;
        for &sample in samples {
            let sign = match sample {
                0 => continue,
                _ if sample > 0 => 1,
                _ => -1,
            };
            if previous != 0 && sign != previous {
                crossings += 1;
            }
            previous = sign;
        }
        crossings
    }

    fn frequency(samples: &[i16]) -> f64 {
        let seconds = samples.len() as f64 / f64::from(consts::SAMPLE_RATE);
        zero_crossings(samples) as f64 / (2.0 * seconds)
    }

    fn window(samples: &[i16], range: Range<usize>) -> &[i16] {
        &samples[range]
    }

    fn peak(samples: &[i16]) -> i32 {
        samples
            .iter()
            .map(|&s| i32::from(s).abs())
            .max()
            .unwrap_or(0)
    }

    #[kithara::test(native, flash(false))]
    fn a_saw_climbs_the_whole_16_bit_range() {
        assert_eq!(Wave::Sawtooth.sample(0, consts::SAMPLE_RATE), i16::MIN);
        assert_eq!(Wave::Sawtooth.sample(1, consts::SAMPLE_RATE), i16::MIN + 1);
        assert_eq!(
            Wave::Sawtooth.sample(SAW_PERIOD - 1, consts::SAMPLE_RATE),
            i16::MAX
        );
        assert_eq!(
            Wave::Sawtooth.sample(SAW_PERIOD, consts::SAMPLE_RATE),
            i16::MIN
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_descending_saw_mirrors_the_ascending_one() {
        for frame in [0, 1, 12_345, SAW_PERIOD - 1] {
            let up = i32::from(Wave::Sawtooth.sample(frame, consts::SAMPLE_RATE));
            let down = i32::from(Wave::SawtoothDescending.sample(frame, consts::SAMPLE_RATE));

            assert_eq!(up + down, -1, "frame {frame}");
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_shifted_saw_leads_by_half_a_period() {
        for frame in [0, 1, 12_345] {
            assert_eq!(
                Wave::SawtoothShifted.sample(frame, consts::SAMPLE_RATE),
                Wave::Sawtooth.sample(frame + SAW_PERIOD / 2, consts::SAMPLE_RATE),
                "frame {frame}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_sine_starts_at_zero_and_reaches_its_peak() {
        let sine = Wave::sine(f64::from(consts::SAMPLE_RATE) / 4.0);

        assert_eq!(sine.sample(0, consts::SAMPLE_RATE), 0);
        assert_eq!(sine.sample(1, consts::SAMPLE_RATE), i16::MAX);
    }

    #[kithara::test(native, flash(false))]
    fn a_sine_honours_its_peak() {
        let quiet = Wave::Sine {
            hz: f64::from(consts::SAMPLE_RATE) / 4.0,
            peak: 1_000,
        };

        assert_eq!(quiet.sample(1, consts::SAMPLE_RATE), 1_000);
    }

    #[kithara::test(native, flash(false))]
    fn clicks_fade_out_and_leave_the_rest_of_the_period_silent() {
        let period = consts::SAMPLE_RATE as usize / 2;
        let burst = consts::SAMPLE_RATE as usize / 100;
        let clicks = Wave::clicks(1_000.0, period, burst);
        let samples = render(clicks, period * 2);

        assert!(samples[burst..period].iter().all(|&s| s == 0));
        let head = peak(window(&samples, 0..burst / 4));
        let tail = peak(window(&samples, burst * 3 / 4..burst));
        assert!(head > tail, "{head} > {tail}");
        assert_eq!(
            peak(window(&samples, 0..burst)),
            peak(window(&samples, period..period + burst)),
            "every period carries the same burst"
        );
    }

    #[kithara::test(native, flash(false))]
    fn silence_is_silent() {
        assert!(render(Wave::Silence, 512).iter().all(|&s| s == 0));
    }

    #[kithara::test(native, flash(false))]
    fn a_sweep_starts_at_zero_and_ends_after_its_span() {
        let frames = consts::SAMPLE_RATE as usize;
        let sweep = Wave::sweep(100.0, 8_000.0, frames, SweepMode::Linear);

        assert_eq!(sweep.sample(0, consts::SAMPLE_RATE), 0);
        assert_eq!(sweep.sample(frames, consts::SAMPLE_RATE), 0);
    }

    #[kithara::test(native, flash(false))]
    fn a_sweep_gets_denser_over_time() {
        let frames = (consts::SAMPLE_RATE * 2) as usize;
        let samples = render(
            Wave::sweep(100.0, 6_400.0, frames, SweepMode::Linear),
            frames,
        );
        let size = 4_096;
        let early = zero_crossings(window(&samples, 2_048..2_048 + size));
        let middle = zero_crossings(window(&samples, 32_768..32_768 + size));
        let late = zero_crossings(window(&samples, 72_000..72_000 + size));

        assert!(early < middle, "{early} < {middle}");
        assert!(middle < late, "{middle} < {late}");
    }

    #[kithara::test(native, flash(false))]
    fn a_sweep_reaches_its_target_frequency() {
        let frames = consts::SAMPLE_RATE as usize;
        let samples = render(
            Wave::sweep(100.0, 4_000.0, frames, SweepMode::Linear),
            frames,
        );
        let tail = frequency(window(&samples, frames - 1_024..frames));

        assert!((tail - 4_000.0).abs() < 220.0, "tail is {tail} Hz");
    }

    #[kithara::test(native, flash(false))]
    fn a_log_sweep_passes_the_geometric_mean_at_its_midpoint() {
        let frames = (consts::SAMPLE_RATE * 2) as usize;
        let samples = render(Wave::sweep(100.0, 1_000.0, frames, SweepMode::Log), frames);
        let midpoint = frames / 2;
        let estimate = frequency(window(&samples, midpoint - 2_048..midpoint + 2_048));
        let expected = 100.0 * f64::sqrt(10.0);

        assert!(
            (estimate - expected).abs() < 25.0,
            "midpoint is {estimate} Hz"
        );
    }
}
