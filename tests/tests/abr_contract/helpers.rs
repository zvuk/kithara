/// Single point of truth for every constant the contract suite
/// shares. Grouped on a uninhabited type so callers spell the
/// constant out as `Consts::SAMPLE_RATE` etc. — that satisfies the
/// `style.multiple-private-module-consts` lint and keeps the values
/// reachable from every T-test through one `use`.
pub(crate) struct Consts;

impl Consts {
    pub(crate) const CHANNELS: u16 = 2;
    /// Length of the post-seek observation window, in chunks. Long
    /// enough to expose any late `variant_from` chunk that would
    /// indicate a double-decode regression (T7).
    pub(crate) const POST_SWITCH_CHUNKS_REQUIRED_LONG: usize = 12;
    /// Number of post-switch chunks the suite must observe before
    /// asserting on phase / fetch-count contracts. Four chunks span
    /// roughly one segment of audio at the test's sample rate.
    pub(crate) const POST_SWITCH_CHUNKS_REQUIRED_SHORT: usize = 4;
    /// Pre-switch playhead position (seconds) at which T1, T2, T3,
    /// T5 and T7 commit their `set_mode` call. Far enough into the
    /// fixture for the warmup decoder to be in steady state, but
    /// well before the natural EOF.
    pub(crate) const PRE_SWITCH_TARGET_SECS: f64 = 4.0;
    pub(crate) const SAMPLE_RATE: u32 = 44_100;
    pub(crate) const SEGMENT_DURATION_SECS: f64 = 2.0;
    pub(crate) const SEGMENTS_PER_VARIANT: usize = 12;
    pub(crate) const SINE_FREQ_HZ: f64 = 440.0;
    /// AAC shq (variant 2).
    pub(crate) const VARIANT_AAC_HQ: usize = 2;
    /// AAC slq (variant 0).
    pub(crate) const VARIANT_AAC_LQ: usize = 0;
    /// AAC smq (variant 1).
    pub(crate) const VARIANT_AAC_MQ: usize = 1;
    pub(crate) const VARIANT_BANDWIDTHS: [u64; 4] = [66_000, 134_000, 270_000, 988_000];
    /// FLAC slossless (variant 3).
    pub(crate) const VARIANT_FLAC: usize = 3;
}

pub(crate) mod phase_oracle {
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
            assert!(
                diff < 1e-6,
                "no-shift phase mismatch: diff={diff} (measured={measured}, expected={expected})"
            );
        }

        #[kithara::test]
        fn measured_lags_by_n_samples_reports_n() {
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

pub(crate) mod params {
    use kithara::{
        assets::AssetStore,
        audio::{Audio, AudioConfig},
        decode::DecoderBackend,
        hls::{AbrMode, Hls, HlsConfig},
        platform::CancelToken,
        stream::{AudioCodec, Stream},
    };
    use kithara_integration_tests::{
        HlsFixtureBuilder,
        fixture_protocol::{DelayRule, PackagedSignal},
    };

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
        store: AssetStore,
        abr: AbrMode,
        backend: DecoderBackend,
        prefetch_count: usize,
    ) -> Audio<Stream<Hls>> {
        let cancel = CancelToken::never();
        let hls_config = HlsConfig::for_url(url.clone())
            .store(store)
            .cancel(cancel)
            .initial_abr_mode(abr)
            .download_batch_size(prefetch_count)
            .build();
        // pcm_buffer_chunks defaults to 10 (~1s audio). RED tests
        // must wait for specific probes (for example, decoder build_chunk
        // at timestamp >= 4s) without an active consumer, so the buffer
        // is expanded to ~30s so the decoder's background worker can run
        // autonomously and fill the channel while the test thread waits via
        // `wait_for_probe`.
        let config = AudioConfig::<Hls>::for_stream(hls_config)
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .pcm_buffer_chunks(300)
            .build();
        let mut audio = Audio::<Stream<Hls>>::new(config)
            .await
            .unwrap_or_else(|err| panic!("audio open failed for {url}: {err}"));
        let _ = audio.preload();
        audio
    }
}

pub(crate) mod counters {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::Path,
    };

    use kithara::platform::time::Instant;
    use kithara_test_utils::probe::capture::{ProbeEvent, Recorder};

    /// One `dispatch` USDT event lifted into typed form.
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

    /// All `dispatch` events on or after `since` for the given
    /// variant. Native scheduler ground truth — what the scheduler
    /// decided to fetch, regardless of whether the bytes ever
    /// arrived.
    pub(crate) fn fetch_emits_for(
        recorder: &Recorder,
        variant: usize,
        since: Instant,
    ) -> Vec<FetchEmit> {
        recorder
            .events_with_probe("dispatch")
            .iter()
            .filter(|e| e.at >= since)
            .filter_map(FetchEmit::from_event)
            .filter(|emit| emit.variant == variant)
            .collect()
    }

    /// Every `dispatch` event since the recorder started, any
    /// variant. Walks the global ABR-driven scheduler decision log.
    pub(crate) fn all_fetch_emits(recorder: &Recorder) -> Vec<FetchEmit> {
        recorder
            .events_with_probe("dispatch")
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

    /// First `notify_commit` event whose target variant is
    /// `variant_to`, or `None` if no such commit fired.
    ///
    /// `variant_from` is accepted for caller readability. The probe
    /// carries the *target* variant (`target` ↦ `target_variant_index`),
    /// so we filter on `variant_to`; the `variant_from` argument is unused
    /// at the probe-event level today.
    pub(crate) fn first_abr_commit(
        recorder: &Recorder,
        variant_from: usize,
        variant_to: usize,
    ) -> Option<Instant> {
        let _ = variant_from;
        recorder
            .events_with_probe("notify_commit")
            .into_iter()
            .find(|e| e.u64("target") == Some(variant_to as u64))
            .map(|e| e.at)
    }
}

pub(crate) mod diagnostics {
    use std::collections::BTreeMap;

    use kithara_test_utils::probe::capture::Recorder;

    /// Probes printed in full — they describe the scheduler's decision
    /// stream end-to-end. Listed once, every firing is shown.
    const KEY_PROBES: &[&str] = &[
        "poll_next",
        "dispatch",
        "on_complete_seg",
        "rebuild",
        "on_reader_advance",
        "on_evict",
        "start_request",
        "establish",
        "with_soft_timeout",
        "deliver",
        "finish_request",
        "abort_request",
        "fail_request",
        "commit_pending",
        "notify_commit",
        "init_segment_range",
        "segment_after_byte",
        "segment_at_time",
        "find_at_offset",
        "set_position",
        "advance",
        "position",
        "queue_resolved_segment_request",
    ];

    /// Format a recorder snapshot for inclusion in a panic message:
    /// per-probe firing counts, every firing of every "key" probe in
    /// `KEY_PROBES`, then the last `tail_n` raw events.
    pub(crate) fn format_probe_dump(recorder: &Recorder, tail_n: usize) -> String {
        let events = recorder.snapshot();
        let mut out = String::new();
        out.push_str(&format!("probe events captured: {}\n", events.len()));

        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for e in &events {
            if let Some(name) = e.probe_name() {
                *counts.entry(name.to_string()).or_default() += 1;
            }
        }
        out.push_str("probe firing counts:\n");
        for (name, count) in &counts {
            out.push_str(&format!("  {name}: {count}\n"));
        }

        out.push_str("key probe events (every firing):\n");
        for e in &events {
            let Some(name) = e.probe_name() else {
                continue;
            };
            if KEY_PROBES.contains(&name) {
                out.push_str(&format!("  {e:?}\n"));
            }
        }

        let tail_start = events.len().saturating_sub(tail_n);
        out.push_str(&format!(
            "last {} raw events (of {}):\n",
            events.len() - tail_start,
            events.len()
        ));
        for e in &events[tail_start..] {
            out.push_str(&format!("  {e:?}\n"));
        }
        out
    }
}

pub(crate) mod probe_contracts {
    use std::collections::{BTreeMap, BTreeSet};

    use kithara_test_utils::probe::capture::{ProbeEvent, Recorder};

    /// Bucket events by `thread_id`, sorted by `thread_seq` within each
    /// bucket. Events without a `thread_id` (older probe expansions, or
    /// `probe_return` path) are dropped — the contract layer needs both
    /// fields to talk about per-thread order.
    fn group_by_thread(recorder: &Recorder) -> BTreeMap<u64, Vec<ProbeEvent>> {
        let mut by_thread: BTreeMap<u64, Vec<ProbeEvent>> = BTreeMap::new();
        for e in recorder.snapshot() {
            let Some(tid) = e.thread_id() else {
                continue;
            };
            by_thread.entry(tid).or_default().push(e);
        }
        for events in by_thread.values_mut() {
            events.sort_by_key(|e| e.thread_seq().unwrap_or(u64::MAX));
        }
        by_thread
    }

    /// All events matching `predicate`, grouped by `thread_id`.
    fn filter_by_thread<F>(recorder: &Recorder, predicate: F) -> BTreeMap<u64, Vec<ProbeEvent>>
    where
        F: Fn(&ProbeEvent) -> bool,
    {
        let mut by_thread = group_by_thread(recorder);
        for events in by_thread.values_mut() {
            events.retain(&predicate);
        }
        by_thread.retain(|_, v| !v.is_empty());
        by_thread
    }

    /// Verify the 5-stage HTTP request lifecycle (`start_request →
    /// with_soft_timeout(establish -> deliver) -> finish_request`) for
    /// `request_id` runs to completion on a single thread.
    ///
    /// Returns `Ok(())` on a complete lifecycle. On failure the error
    /// names the request id, the thread, the missing stage, and the
    /// last stage that was observed — so the operator sees "req=26
    /// reached `with_soft_timeout` and never delivered" instead of
    /// "test timed out". Cancelled paths (`abort_request` /
    /// `fail_request`) are accepted as terminal alternatives to
    /// `finish_request`.
    pub(crate) fn assert_request_lifecycle_complete(
        recorder: &Recorder,
        request_id: u64,
    ) -> Result<(), String> {
        const ENTRY_STAGES: &[&str] =
            &["start_request", "establish", "with_soft_timeout", "deliver"];
        const TERMINAL_STAGES: &[&str] = &["finish_request", "abort_request", "fail_request"];

        let by_thread = filter_by_thread(recorder, |e| e.u64("request_id") == Some(request_id));
        if by_thread.is_empty() {
            return Err(format!(
                "request_id={request_id}: no probes observed. expected {:?} → {:?}.",
                ENTRY_STAGES, TERMINAL_STAGES,
            ));
        }
        if by_thread.len() > 1 {
            return Err(format!(
                "request_id={request_id}: probes on multiple threads {:?} — \
                 spawn_fetch contract violated (one task = one thread).",
                by_thread.keys().collect::<Vec<_>>(),
            ));
        }
        let (thread_id, events) = by_thread.into_iter().next().expect("checked non-empty");
        let observed: Vec<&str> = events.iter().filter_map(ProbeEvent::probe_name).collect();

        for stage in ENTRY_STAGES {
            if !observed.contains(stage) {
                return Err(format!(
                    "request_id={request_id}, thread_id={thread_id}: \
                     missing stage `{stage}`. \
                     observed: {observed:?}. \
                     thread events: {events:#?}",
                ));
            }
        }
        if !TERMINAL_STAGES.iter().any(|t| observed.contains(t)) {
            return Err(format!(
                "request_id={request_id}, thread_id={thread_id}: \
                 no terminal stage observed \
                 (expected `finish_request`, `abort_request` or `fail_request`). \
                 observed: {observed:?}. \
                 this means the request did not reach a terminal delivery outcome. \
                 thread events: {events:#?}",
            ));
        }
        Ok(())
    }

    /// Verify `on_complete_seg` fires **exactly once per `seg=0..n`** on
    /// the pinned `variant`, all on a single thread. The set of
    /// committed `(variant, seg_idx)` pairs must equal
    /// `{(v, 0), (v, 1), …, (v, n-1)}` — no missing seg, no duplicates,
    /// no foreign variant, no out-of-range `seg_idx`.
    ///
    /// **Order is NOT enforced** because the streaming download path
    /// runs up to `max_concurrent` fetches in parallel, and
    /// `on_complete` fires in completion order, not in segment order.
    pub(crate) fn assert_commit_segment_strict(
        recorder: &Recorder,
        variant: usize,
        n: usize,
    ) -> Result<(), String> {
        let by_thread = filter_by_thread(recorder, |e| e.probe_name() == Some("on_complete_seg"));
        if by_thread.is_empty() {
            return Err(format!(
                "on_complete_seg: no events observed. expected seg=0..{} on variant={variant}.",
                n - 1,
            ));
        }
        if by_thread.len() > 1 {
            return Err(format!(
                "on_complete_seg: events on multiple threads {:?} — \
                 single-variant scheduler contract violated.",
                by_thread.keys().collect::<Vec<_>>(),
            ));
        }
        let (thread_id, events) = by_thread.into_iter().next().expect("checked non-empty");
        let pairs: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|e| {
                let v = usize::try_from(e.u64("variant")?).ok()?;
                let s = usize::try_from(e.u64("seg_idx")?).ok()?;
                Some((v, s))
            })
            .collect();
        let actual: BTreeSet<(usize, usize)> = pairs.iter().copied().collect();
        let expected: BTreeSet<(usize, usize)> = (0..n).map(|s| (variant, s)).collect();
        let total_events = pairs.len();
        let unique_seg_count = actual.len();
        if actual != expected || total_events != n {
            let emit_events: Vec<ProbeEvent> = recorder.events_with_probe("dispatch");
            let notify_events: Vec<ProbeEvent> = recorder.events_with_probe("notify_commit");
            let missing: Vec<(usize, usize)> = expected.difference(&actual).copied().collect();
            let extra: Vec<(usize, usize)> = actual.difference(&expected).copied().collect();
            // Count how many times each (variant, seg_idx) appeared,
            // showing only entries seen more than once so the duplicated
            // segment is visible immediately.
            let mut counts: BTreeMap<(usize, usize), usize> = BTreeMap::new();
            for pair in &pairs {
                *counts.entry(*pair).or_default() += 1;
            }
            let duplicates: Vec<((usize, usize), usize)> =
                counts.into_iter().filter(|(_, c)| *c > 1).collect();
            let duplicates_diag = if duplicates.is_empty() {
                String::new()
            } else {
                format!(
                    "\nduplicates ({}):\n{:#?}\n\
                     the same (variant, seg_idx) reached \
                     on_complete_seg more than once — race condition \
                     in the commit chain.",
                    duplicates.len(),
                    duplicates,
                )
            };
            return Err(format!(
                "on_complete_seg, thread_id={thread_id}: set of \
                 committed segments does not match \
                 ({total_events} events, {unique_seg_count} unique, \
                 expected n={n}).\n\
                 expected: {expected:?}\n\
                 actual:   {actual:?}\n\
                 missing:  {missing:?}\n\
                 extra:    {extra:?}\
                 {duplicates_diag}\n\
                 dispatch events ({}):\n{:#?}\n\
                 notify_commit events ({}):\n{:#?}",
                emit_events.len(),
                emit_events,
                notify_events.len(),
                notify_events,
            ));
        }
        Ok(())
    }

    /// One step in an expected probe-sequence: probe name + zero or more
    /// `(arg_key, expected_value)` constraints + human-readable note. The
    /// matcher walks the actual peer-thread events and tries to find every
    /// expected step in order; on failure the error message names which
    /// step did not match and what the operator should be looking at.
    struct ExpectedEvent<'a> {
        name: &'a str,
        args: Vec<(&'a str, u64)>,
        note: &'a str,
    }

    impl ExpectedEvent<'_> {
        fn matches(&self, e: &ProbeEvent) -> bool {
            if e.probe_name() != Some(self.name) {
                return false;
            }
            for (key, expected_val) in &self.args {
                if e.u64(key) != Some(*expected_val) {
                    return false;
                }
            }
            true
        }

        fn describe(&self) -> String {
            let parts: Vec<String> = self.args.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{}({})", self.name, parts.join(", "))
        }
    }

    /// Strict per-thread sequence contract for **post-apply scheduler
    /// chain** on the peer-thread of an `HlsPeer`.
    ///
    /// Encodes the FULL expected probe sequence that should fire on the
    /// peer-thread between `apply_thread_seq` (a baseline event from ABR
    /// commit) and the first `dispatch(variant=v_new)`.
    /// **Order is enforced strictly**: the matcher walks `peer_events`
    /// and increments an `expected[]` cursor every time it finds the
    /// next expected step; intervening probes are tolerated, but
    /// expected steps must appear in the given order.
    ///
    /// **Switch case** (`expect_midstream_switch=true`, `V_old≠V_new)`:
    /// 1. `notify_commit(target=V_new)` — boundary commit fired.
    /// 2. `rebuild(variant=V_new)` — `V_new` queue rebuilt from
    ///    `from_seg = decode_seg_at_apply`.
    /// 3. `dispatch(variant=V_new)` — first `V_new` fetch dispatched.
    ///
    /// **Noop case** (`expect_midstream_switch=false`, `V_old==V_new)`:
    /// scheduler continues normal `V_old` dispatch — no rebuild required.
    /// Sequence: `dispatch(V_old)`.
    ///
    /// Returns the first `dispatch(v_new)` event so the caller
    /// can assert C-A5 (forward-only) on its `segment_index`.
    pub(crate) fn assert_post_apply_fetch_chain(
        recorder: &Recorder,
        peer_thread_id: u64,
        apply_thread_seq: u64,
        v_old: usize,
        v_new: usize,
        expect_midstream_switch: bool,
        forward_only_floor: usize,
    ) -> Result<ProbeEvent, String> {
        let mut peer_events: Vec<ProbeEvent> = recorder
            .snapshot()
            .into_iter()
            .filter(|e| {
                e.thread_id() == Some(peer_thread_id)
                    && e.thread_seq().is_some_and(|s| s > apply_thread_seq)
            })
            .collect();
        peer_events.sort_by_key(|e| e.thread_seq().unwrap_or(u64::MAX));

        // Forbidden invariant (C-A5 forward-only): never dispatch a
        // media fetch for V_new at `seg_idx < forward_only_floor`.
        // After a switch all those segments are already in the
        // decoder; fetching V_new copies of them means scheduler
        // walked backwards onto already-played data.
        if expect_midstream_switch
            && let Some(bad_emit) = peer_events.iter().find(|e| {
                e.probe_name() == Some("dispatch")
                    && e.u64("variant") == Some(v_new as u64)
                    && e.u64("segment_index")
                        .is_some_and(|s| (s as usize) < forward_only_floor)
            })
        {
            let bad_seg = bad_emit.u64("segment_index").unwrap_or(u64::MAX) as usize;
            return Err(format!(
                "Forward-only invariant violated: \
                 `dispatch(variant=V_new={v_new}, segment_index={bad_seg})` \
                 fired post-apply on peer_thread={peer_thread_id} \
                 (forward_only_floor={forward_only_floor}).\n\
                 event: {bad_emit:?}",
            ));
        }

        let v_old_u = v_old as u64;
        let v_new_u = v_new as u64;
        let _ = v_old_u;

        let probe_variant = if expect_midstream_switch {
            v_new_u
        } else {
            v_old_u
        };
        let mut expected: Vec<ExpectedEvent<'static>> = Vec::new();
        if expect_midstream_switch {
            expected.push(ExpectedEvent {
                name: "notify_commit",
                args: vec![("target", v_new_u)],
                note: "boundary commit must fire for V_new",
            });
            expected.push(ExpectedEvent {
                name: "rebuild",
                args: vec![("variant", v_new_u)],
                note: "V_new queue must be rebuilt before dispatch",
            });
        }
        expected.push(ExpectedEvent {
            name: "dispatch",
            args: vec![("variant", probe_variant)],
            note: "first dispatch post-apply for the target variant — segment_index must be >= forward_only_floor",
        });

        let mut next_idx = 0usize;
        let mut last_match_pos: Option<usize> = None;
        for (i, e) in peer_events.iter().enumerate() {
            if next_idx >= expected.len() {
                break;
            }
            if expected[next_idx].matches(e) {
                next_idx += 1;
                last_match_pos = Some(i);
            }
        }

        if next_idx < expected.len() {
            let missed = &expected[next_idx];
            let start = last_match_pos.map_or(0, |i| i + 1);
            let observed_after_match: Vec<&ProbeEvent> =
                peer_events[start..].iter().take(40).collect();
            let prev_summary = if next_idx == 0 {
                "no expected step matched yet".to_string()
            } else {
                let prev = &expected[next_idx - 1];
                format!(
                    "matched expected[0..{next_idx}], last = {}",
                    prev.describe()
                )
            };
            return Err(format!(
                "post-apply chain expected[{next_idx}/{total}] not observed: \
                 expected `{exp}` ({note}).\n\
                 {prev_summary}.\n\
                 peer_thread={peer_thread_id}, \
                 apply_thread_seq={apply_thread_seq}, \
                 expect_midstream_switch={expect_midstream_switch}, \
                 v_old={v_old}, v_new={v_new}.\n\
                 peer events after last match ({n} shown of {total_evt}):\n{events:#?}",
                total = expected.len(),
                exp = missed.describe(),
                note = missed.note,
                n = observed_after_match.len(),
                total_evt = peer_events.len() - start,
                events = observed_after_match,
            ));
        }

        Ok(peer_events
            .iter()
            .find(|e| e.probe_name() == Some("dispatch") && e.u64("variant") == Some(probe_variant))
            .cloned()
            .expect("matched in expected sequence above"))
    }

    /// Verify the decoder reached at least `target_ts_us` microseconds
    /// of decoded audio (a `build_chunk` event with `timestamp >= target`).
    /// On failure the message reports the last observed timestamp and
    /// total chunks decoded — concrete evidence of a stall instead of
    /// "`wait_for_probe` timed out".
    pub(crate) fn assert_build_chunk_reached(
        recorder: &Recorder,
        target_ts_us: u64,
    ) -> Result<(), String> {
        let by_thread = filter_by_thread(recorder, |e| e.probe_name() == Some("build_chunk"));
        if by_thread.is_empty() {
            return Err("build_chunk: no events observed — decoder emitted no PCM frames".into());
        }
        if by_thread.len() > 1 {
            return Err(format!(
                "build_chunk: events on multiple threads {:?} — \
                 decoder is single-threaded in T8.",
                by_thread.keys().collect::<Vec<_>>(),
            ));
        }
        let (thread_id, events) = by_thread.into_iter().next().expect("checked non-empty");
        let last_ts = events
            .iter()
            .filter_map(|e| e.u64("timestamp"))
            .max()
            .unwrap_or(0);
        if last_ts >= target_ts_us {
            return Ok(());
        }
        Err(format!(
            "build_chunk, thread_id={thread_id}: decoder reached ts={last_ts}μs, \
             expected ts >= {target_ts_us}μs (chunks_observed={}). \
             decoder stalled — next step: check which fetch did not complete \
             (see `assert_request_lifecycle_complete`).",
            events.len(),
        ))
    }
}

pub(crate) mod assertions {
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
