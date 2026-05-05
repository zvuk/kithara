//! Shared infrastructure for the ABR-switch contract tests
//! (`tests/tests/kithara_hls/abr_contract/T1..T8`).
//!
//! Every helper here is purpose-built for the contract spec at
//! `.docs/plans/2026-05-03-abr-switch-contract.md`. None of them
//! relax the contract: assertions are exact and there are no escape
//! hatches.
//!
//! Modules:
//! * [`phase_oracle`] — analytic 440 Hz phase reference and atan2
//!   quadrature measurement; reports the exact number of lost or
//!   repeated samples on a seam violation.
//! * [`params`] — production-shaped 4-variant wave fixture
//!   (3×AAC + 1×FLAC), network profiles, and audio-open boilerplate.
//! * [`counters`] — typed views over `emit_fetch_cmd`,
//!   `finish_request`, and disk inventory probes.
//! * [`assertions`] — equality predicates that print a precise diff
//!   on failure.

/// Single point of truth for every constant the contract suite
/// shares. Grouped on a uninhabited type so callers spell the
/// constant out as `Consts::SAMPLE_RATE` etc. — that satisfies the
/// `style.multiple-private-module-consts` lint and keeps the values
/// reachable from every T-test through one `use`.
pub(super) struct Consts;

impl Consts {
    pub(super) const CHANNELS: u16 = 2;
    /// Length of the post-seek observation window, in chunks. Long
    /// enough to expose any late `variant_from` chunk that would
    /// indicate a double-decode regression (T7).
    pub(super) const POST_SWITCH_CHUNKS_REQUIRED_LONG: usize = 12;
    /// Number of post-switch chunks the suite must observe before
    /// asserting on phase / fetch-count contracts. Four chunks span
    /// roughly one segment of audio at the test's sample rate.
    pub(super) const POST_SWITCH_CHUNKS_REQUIRED_SHORT: usize = 4;
    /// Pre-switch playhead position (seconds) at which T1, T2, T3,
    /// T5 and T7 commit their `set_mode` call. Far enough into the
    /// fixture for the warmup decoder to be in steady state, but
    /// well before the natural EOF.
    pub(super) const PRE_SWITCH_TARGET_SECS: f64 = 4.0;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    pub(super) const SEGMENT_DURATION_SECS: f64 = 2.0;
    pub(super) const SEGMENTS_PER_VARIANT: usize = 12;
    pub(super) const SINE_FREQ_HZ: f64 = 440.0;
    /// AAC shq (variant 2).
    pub(super) const VARIANT_AAC_HQ: usize = 2;
    /// AAC slq (variant 0).
    pub(super) const VARIANT_AAC_LQ: usize = 0;
    /// AAC smq (variant 1).
    pub(super) const VARIANT_AAC_MQ: usize = 1;
    pub(super) const VARIANT_BANDWIDTHS: [u64; 4] = [66_000, 134_000, 270_000, 988_000];
    /// FLAC slossless (variant 3).
    pub(super) const VARIANT_FLAC: usize = 3;
}

pub(super) mod phase_oracle {
    //! 440 Hz sine phase reference per the contract A1.
    //!
    //! The wave fixture renders an identical
    //! `sin(2π · 440 · frame_offset / sample_rate) · 32767` waveform on
    //! every variant via `kithara_test_utils::signal::SineWave`. The
    //! decoder pipeline (AAC and FLAC) introduces a static phase
    //! offset that varies per backend; we calibrate that offset on the
    //! first PRE-switch chunk and check seam continuity against the
    //! calibrated baseline.
    use std::f64::consts::TAU;

    /// Analytic phase of the source 440 Hz sine at the given absolute
    /// frame offset, modulo 2π. Domain: `[0, TAU)`.
    pub(crate) fn expected_phase(frame_offset: u64, sample_rate: u32, freq_hz: f64) -> f64 {
        let phase = TAU * freq_hz * (frame_offset as f64) / f64::from(sample_rate);
        phase.rem_euclid(TAU)
    }

    /// Measure phase from interleaved f32 PCM via in-phase / quadrature
    /// samples spaced one quarter-period apart. `pcm` is the chunk's
    /// interleaved buffer (LRLRLR for stereo); `channels` selects the
    /// stride between successive frames; `sample_offset` selects which
    /// frame inside the chunk to anchor on (0 = first frame).
    ///
    /// Returns `None` when the chunk does not contain enough trailing
    /// samples after `sample_offset` for a full quadrature pair, or
    /// when the in-phase + quadrature magnitude is below
    /// `magnitude_floor` (silence — phase is undefined there).
    pub(crate) fn measured_phase(
        pcm: &[f32],
        channels: u16,
        sample_offset: usize,
        sample_rate: u32,
        freq_hz: f64,
    ) -> Option<f64> {
        let stride = usize::from(channels.max(1));
        let q_samples = ((f64::from(sample_rate) / (4.0 * freq_hz)).round() as usize).max(1);
        let i_idx = sample_offset.checked_mul(stride)?;
        let q_idx = sample_offset.checked_add(q_samples)?.checked_mul(stride)?;
        let i_sample = pcm.get(i_idx).copied()?;
        let q_sample = pcm.get(q_idx).copied()?;
        let magnitude = (f64::from(i_sample).powi(2) + f64::from(q_sample).powi(2)).sqrt();
        const MAGNITUDE_FLOOR: f64 = 0.05;
        if magnitude < MAGNITUDE_FLOOR {
            return None;
        }
        // sin(theta) at the i-frame, sin(theta + π/2) = cos(theta) at
        // the q-frame ⇒ atan2(in_phase, quadrature) recovers theta.
        let phase = f64::atan2(f64::from(i_sample), f64::from(q_sample));
        Some(phase.rem_euclid(TAU))
    }

    /// Signed shortest distance between two phases, in `(-π, π]`.
    pub(crate) fn phase_diff_signed(a: f64, b: f64) -> f64 {
        let mut d = a - b;
        while d > std::f64::consts::PI {
            d -= TAU;
        }
        while d <= -std::f64::consts::PI {
            d += TAU;
        }
        d
    }

    /// Convert a signed phase delta back into "samples lost or
    /// repeated" so failure messages name the violation in concrete
    /// units. Positive = lost samples (phase ran ahead);
    /// negative = repeated samples (phase fell behind).
    pub(crate) fn phase_delta_in_samples(delta: f64, sample_rate: u32, freq_hz: f64) -> f64 {
        delta * f64::from(sample_rate) / (TAU * freq_hz)
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::*;

        struct Consts;
        impl Consts {
            const F: f64 = 440.0;
            const N_SAMPLE_SHIFT: u64 = 5;
            const SR: u32 = 44_100;
        }

        fn make_sine(frame_offset: u64, frames: usize, channels: u16) -> Vec<f32> {
            let mut out = Vec::with_capacity(frames * usize::from(channels));
            for i in 0..frames {
                let abs = frame_offset + i as u64;
                let theta = TAU * Consts::F * (abs as f64) / f64::from(Consts::SR);
                let s = theta.sin() as f32;
                for _ in 0..channels {
                    out.push(s);
                }
            }
            out
        }

        #[kithara::test]
        fn expected_phase_zero_at_start() {
            let p = expected_phase(0, Consts::SR, Consts::F);
            assert!(p.abs() < 1e-9, "phase at frame 0 must be 0, got {p}");
        }

        #[kithara::test]
        fn expected_phase_wraps_modulo_tau() {
            // Pick frames where 2π·440·n/sr is an exact multiple of TAU
            // by hand: at sr=44100 there is no integer n that gives
            // exactly k·2π (gcd issues), so we instead just assert
            // expected_phase is always inside [0, TAU) and that two
            // frames `n` and `n + 2sr` produce the same phase mod TAU
            // (`2sr` adds 2 full seconds = `2·440 = 880` complete
            // cycles).
            let p1 = expected_phase(123, Consts::SR, Consts::F);
            let p2 = expected_phase(123 + 2 * u64::from(Consts::SR), Consts::SR, Consts::F);
            let diff = phase_diff_signed(p1, p2).abs();
            assert!(
                diff < 1e-9,
                "phase mod TAU not idempotent under integer-cycle shift: \
                 p1={p1}, p2={p2}, diff={diff}"
            );
            for n in [0u64, 1, 7, 100, 12345] {
                let p = expected_phase(n, Consts::SR, Consts::F);
                assert!(
                    (0.0..TAU).contains(&p),
                    "expected_phase({n}) = {p} outside [0, TAU)"
                );
            }
        }

        #[kithara::test]
        fn measured_matches_expected_no_shift() {
            let pcm = make_sine(0, 1024, 2);
            let measured =
                measured_phase(&pcm, 2, 0, Consts::SR, Consts::F).expect("magnitude above floor");
            let expected = expected_phase(0, Consts::SR, Consts::F);
            let diff = phase_diff_signed(measured, expected).abs();
            // atan2(0, 1) = 0 — must match expected to within float epsilon.
            assert!(
                diff < 1e-6,
                "no-shift phase mismatch: diff={diff} (measured={measured}, expected={expected})"
            );
        }

        #[kithara::test]
        fn measured_lags_by_n_samples_reports_n() {
            // Place the analytic source N frames LATER and ask phase_oracle
            // to read it from the original origin — the measured phase
            // should be smaller by ω·N·T = 2π · 440 · N / sample_rate.
            let n = Consts::N_SAMPLE_SHIFT;
            let pcm = make_sine(n, 1024, 2);
            let measured =
                measured_phase(&pcm, 2, 0, Consts::SR, Consts::F).expect("magnitude above floor");
            let expected_at_origin = expected_phase(0, Consts::SR, Consts::F);
            let diff = phase_diff_signed(measured, expected_at_origin);
            let samples = phase_delta_in_samples(diff, Consts::SR, Consts::F);
            assert!(
                (samples - n as f64).abs() < 0.05,
                "expected phase shift of {n} samples, oracle reported {samples} \
                 (raw diff={diff})"
            );
        }
    }
}

pub(super) mod params {
    //! 4-variant production-shaped wave fixture: 3×AAC LQ/MQ/HQ +
    //! 1×FLAC, all rendering the same 440 Hz sine through the
    //! fixture-server signal pipeline. Network profiles cover the
    //! `instant`, `slow`, and `flaky` axes called out in the spec.
    use kithara::{
        assets::StoreOptions,
        audio::Audio,
        decode::DecoderBackend,
        hls::{AbrMode, Hls, HlsConfig},
        stream::{AudioCodec, Stream},
    };
    use kithara_audio::AudioConfig;
    use kithara_test_utils::{
        HlsFixtureBuilder,
        fixture_protocol::{DelayRule, PackagedSignal},
    };
    use tokio_util::sync::CancellationToken;

    use super::Consts;

    /// Production-shaped 4-variant fMP4 HLS fixture mirroring
    /// `assets/hls/master.m3u8`: variants 0..2 carry AAC-LC at LQ/MQ/HQ
    /// bitrates, variant 3 carries FLAC lossless. All four render the
    /// same 440 Hz sine through `PackagedSignal::Sine`, so post-switch
    /// PCM is phase-comparable to pre-switch on every transition.
    pub(crate) fn wave_fixture_4_variants() -> HlsFixtureBuilder {
        HlsFixtureBuilder::new()
            .variant_count(4)
            .segments_per_variant(Consts::SEGMENTS_PER_VARIANT)
            .segment_duration_secs(Consts::SEGMENT_DURATION_SECS)
            .variant_bandwidths(Consts::VARIANT_BANDWIDTHS.to_vec())
            .packaged_audio_signal_aac_lc(
                Consts::SAMPLE_RATE,
                Consts::CHANNELS,
                PackagedSignal::Sine {
                    freq_hz: Consts::SINE_FREQ_HZ,
                },
            )
            .override_variant_codec(3, AudioCodec::Flac)
    }

    /// Network profile shape used by the parameterised T-tests.
    /// `delay_rules` is fed to `HlsFixtureBuilder::delay_rules`.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum NetworkProfile {
        Instant,
        Slow { target_variant: usize },
        Flaky,
    }

    impl NetworkProfile {
        pub(crate) fn delay_rules(self) -> Vec<DelayRule> {
            match self {
                Self::Instant => Vec::new(),
                Self::Slow { target_variant } => vec![DelayRule {
                    variant: Some(target_variant),
                    segment_gte: Some(2),
                    delay_ms: 600,
                    ..Default::default()
                }],
                Self::Flaky => (0..Consts::SEGMENTS_PER_VARIANT)
                    .filter(|s| s % 3 == 0)
                    .map(|s| DelayRule {
                        segment_eq: Some(s),
                        delay_ms: 200,
                        ..Default::default()
                    })
                    .collect(),
            }
        }

        pub(crate) fn label(self) -> &'static str {
            match self {
                Self::Instant => "instant",
                Self::Slow { .. } => "slow",
                Self::Flaky => "flaky",
            }
        }
    }

    /// Open an `Audio<Stream<Hls>>` against a wave fixture. Wires
    /// `prefetch_count` through `HlsConfig::with_download_batch_size`
    /// so contract tests can assert A4 with the configured value
    /// rather than a hard-coded constant.
    pub(crate) async fn open_audio(
        url: &url::Url,
        store: StoreOptions,
        abr: AbrMode,
        backend: DecoderBackend,
        prefetch_count: usize,
    ) -> Audio<Stream<Hls>> {
        let cancel = CancellationToken::new();
        let hls_config = HlsConfig::new(url.clone())
            .with_store(store)
            .with_cancel(cancel)
            .with_initial_abr_mode(abr)
            .with_download_batch_size(prefetch_count);
        let config = AudioConfig::<Hls>::new(hls_config).with_decoder_backend(backend);
        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .unwrap_or_else(|err| panic!("audio open failed for {url}: {err}"));
        let _ = audio.preload();
        audio
    }
}

pub(super) mod counters {
    //! Typed views over the production probes that the contract
    //! suite asserts on. Each helper returns concrete, typed events —
    //! tests should NOT hand-decode tracing fields.
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::Path,
    };

    use kithara_platform::time::Instant;
    use kithara_test_utils::probe_capture::{ProbeEvent, Recorder};

    /// One `emit_fetch_cmd` USDT event lifted into typed form.
    #[derive(Clone, Debug)]
    pub(crate) struct FetchEmit {
        pub(crate) at: Instant,
        pub(crate) seek_epoch: u64,
        pub(crate) variant: usize,
        pub(crate) segment_index: usize,
        pub(crate) plan_need_init: bool,
    }

    impl FetchEmit {
        fn from_event(event: &ProbeEvent) -> Option<Self> {
            Some(Self {
                at: event.at,
                seek_epoch: event.u64("seek_epoch")?,
                variant: usize::try_from(event.u64("variant")?).ok()?,
                segment_index: usize::try_from(event.u64("segment_index")?).ok()?,
                plan_need_init: event.u64("plan_need_init")? == 1,
            })
        }
    }

    /// All `emit_fetch_cmd` events on or after `since` for the given
    /// variant. Native scheduler ground truth — what the scheduler
    /// decided to fetch, regardless of whether the bytes ever
    /// arrived.
    pub(crate) fn fetch_emits_for(
        recorder: &Recorder,
        variant: usize,
        since: Instant,
    ) -> Vec<FetchEmit> {
        recorder
            .events_with_probe("emit_fetch_cmd")
            .iter()
            .filter(|e| e.at >= since)
            .filter_map(FetchEmit::from_event)
            .filter(|emit| emit.variant == variant)
            .collect()
    }

    /// Every `emit_fetch_cmd` event since the recorder started, any
    /// variant. T7-style tests use this to walk the global decision
    /// log.
    pub(crate) fn all_fetch_emits(recorder: &Recorder) -> Vec<FetchEmit> {
        recorder
            .events_with_probe("emit_fetch_cmd")
            .iter()
            .filter_map(FetchEmit::from_event)
            .collect()
    }

    /// Walk the asset-store directory at `root` and bucket every
    /// `vN_M.<ext>` file under its `(variant, segment_index)` key.
    /// Init segments use `vN.<ext>` (no underscore) so they are
    /// excluded — match the disk shape in the contract spec where
    /// disk media segments == network media fetches.
    pub(crate) fn disk_files_per_variant(root: &Path) -> BTreeMap<usize, BTreeSet<usize>> {
        let mut by_variant: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        walk_disk(root, root, &mut by_variant);
        by_variant
    }

    fn walk_disk(dir: &Path, base: &Path, by_variant: &mut BTreeMap<usize, BTreeSet<usize>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_disk(&path, base, by_variant);
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(rest) = stem.strip_prefix('v') else {
                continue;
            };
            let Some((variant, segment)) = rest.split_once('_') else {
                continue;
            };
            let (Ok(v), Ok(s)) = (variant.parse::<usize>(), segment.parse::<usize>()) else {
                continue;
            };
            by_variant.entry(v).or_default().insert(s);
        }
    }

    /// First `record_abr_variant_committed` event matching the
    /// `(from, to)` pair, or `None` if it never fired.
    pub(crate) fn first_abr_commit(
        recorder: &Recorder,
        variant_from: usize,
        variant_to: usize,
    ) -> Option<Instant> {
        recorder
            .events_with_probe("record_abr_variant_committed")
            .into_iter()
            .find(|e| {
                e.u64("variant_from") == Some(variant_from as u64)
                    && e.u64("variant_to") == Some(variant_to as u64)
            })
            .map(|e| e.at)
    }
}

pub(super) mod assertions {
    //! Equality predicates with exact failure messages.
    use std::{collections::BTreeSet, fmt::Debug};

    /// Assert that `actual == expected`. Failure prints both values
    /// and a free-form `label` so the spec axiom is named directly.
    pub(crate) fn assert_exact_count<T: PartialEq + Debug>(actual: T, expected: T, label: &str) {
        assert!(
            actual == expected,
            "[{label}] expected exactly {expected:?}, got {actual:?}"
        );
    }

    /// Assert two `BTreeSet`s are equal; on failure prints the
    /// asymmetric differences so the test caller sees which file or
    /// fetch is in one set but not the other.
    pub(crate) fn assert_sets_equal<T: Ord + Debug + Clone>(
        actual: &BTreeSet<T>,
        expected: &BTreeSet<T>,
        label: &str,
    ) {
        if actual == expected {
            return;
        }
        let only_actual: BTreeSet<T> = actual.difference(expected).cloned().collect();
        let only_expected: BTreeSet<T> = expected.difference(actual).cloned().collect();
        panic!(
            "[{label}] sets differ\n  only in actual:   {only_actual:?}\n  only in expected: {only_expected:?}"
        );
    }
}
