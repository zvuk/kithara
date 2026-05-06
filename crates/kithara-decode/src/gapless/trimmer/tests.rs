use std::time::Duration;

use kithara_bufpool::PcmPool;
use kithara_test_utils::kithara;

use super::{Consts, GaplessTrimmer};
use crate::{GaplessInfo, PcmChunk, PcmMeta, PcmSpec, gapless::heuristic::SilenceTrimParams};

fn chunk(spec: PcmSpec, frame_offset: u64, frames: usize) -> PcmChunk {
    let samples = frames.saturating_mul(usize::from(spec.channels));
    let pcm = (0..samples)
        .map(|idx| f32::from(u16::try_from(idx).expect("BUG: test sample fits in u16")))
        .collect::<Vec<_>>();
    PcmChunk::new(
        PcmMeta {
            spec,
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(pcm),
    )
}

fn silent_chunk(spec: PcmSpec, frame_offset: u64, frames: usize) -> PcmChunk {
    let samples = frames.saturating_mul(usize::from(spec.channels));
    PcmChunk::new(
        PcmMeta {
            spec,
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(vec![0.0; samples]),
    )
}

fn custom_chunk(spec: PcmSpec, frame_offset: u64, pcm: Vec<f32>) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec,
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(pcm),
    )
}

fn mono_spec() -> PcmSpec {
    PcmSpec {
        channels: 1,
        sample_rate: 48_000,
    }
}

fn stereo_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 48_000,
    }
}

fn fade_frames_for(spec: PcmSpec) -> usize {
    // Mirrors FadeInState::for_sample_rate; cast safe for typical
    // sample rates well below usize::MAX.
    let computed = (u64::from(spec.sample_rate.max(1)) * Consts::FADE_IN_DURATION_MS) / 1000;
    computed.max(1) as usize
}

fn silence_params(threshold_db: f32, min_trim_frames: u64) -> SilenceTrimParams {
    SilenceTrimParams {
        threshold_db,
        min_trim_frames,
        scan_window_frames: 4096,
        trim_trailing: false,
    }
}

fn collect_pcm(out: &[PcmChunk]) -> Vec<f32> {
    out.iter().flat_map(|c| c.samples().to_vec()).collect()
}

// ---- Existing metadata-driven contract (unchanged behaviour) ----

#[kithara::test]
fn leading_trim_updates_offset_and_timestamp() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 576,
        trailing_frames: 0,
    });

    let mut ready = trimmer.push(chunk(spec, 0, 1024));
    assert_eq!(ready.len(), 1);
    let out = ready.remove(0);
    assert_eq!(out.frames(), 448);
    assert_eq!(out.meta.frame_offset, 576);
    assert_eq!(out.meta.timestamp, Duration::from_millis(12));
    // Metadata-driven trim does NOT apply a fade-in — the boundary
    // is sample-exact, so the first surviving sample stays as-is.
    assert_eq!(out.samples()[0], 576.0);
}

#[kithara::test]
fn leading_trim_can_consume_multiple_chunks() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 2400,
        trailing_frames: 0,
    });

    assert!(trimmer.push(chunk(spec, 0, 1024)).is_empty());
    assert!(trimmer.push(chunk(spec, 1024, 1024)).is_empty());

    let mut ready = trimmer.push(chunk(spec, 2048, 1024));
    assert_eq!(ready.len(), 1);
    let out = ready.remove(0);
    assert_eq!(out.frames(), 672);
    assert_eq!(out.meta.frame_offset, 2400);
    assert_eq!(out.samples()[0], 352.0);
}

#[kithara::test]
fn trailing_trim_buffers_until_flush() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 64,
    });

    assert!(trimmer.push(chunk(spec, 0, 32)).is_empty());

    let mut ready = trimmer.push(chunk(spec, 32, 64));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 32);

    let ready = trimmer.flush();
    assert!(ready.is_empty());
}

#[kithara::test]
fn trailing_trim_drops_tail_on_flush() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 2_048,
    });

    assert!(trimmer.push(chunk(spec, 0, 1_024)).is_empty());
    assert!(trimmer.push(chunk(spec, 1_024, 1_024)).is_empty());
    assert_eq!(trimmer.push(chunk(spec, 2_048, 1_024)).len(), 1);

    let ready = trimmer.flush();
    assert!(ready.is_empty());
}

#[kithara::test]
fn trailing_trim_handles_more_than_inline_tail_chunks() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 8,
    });
    let mut output = super::GaplessOutput::new();

    for frame in 0..8 {
        assert!(trimmer.push(chunk(spec, frame, 1)).is_empty());
    }

    output.extend(trimmer.push(chunk(spec, 8, 1)));
    output.extend(trimmer.flush());

    assert_eq!(output.len(), 1);
    let out = output.remove(0);
    assert_eq!(out.meta.frame_offset, 0);
    assert_eq!(out.frames(), 1);
}

#[kithara::test]
fn disabled_trimmer_passes_through() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::disabled();
    let mut ready = trimmer.push(chunk(spec, 0, 128));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 128);
    assert!(trimmer.flush().is_empty());
}

#[kithara::test]
fn notify_seek_resets_leading_only() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 128,
        trailing_frames: 64,
    });

    assert!(trimmer.push(chunk(spec, 0, 64)).is_empty());
    trimmer.notify_seek();

    assert!(trimmer.push(chunk(spec, 64, 128)).is_empty());

    let mut ready = trimmer.flush();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 64);
}

// ---- Codec-priming mode ----

#[kithara::test]
fn codec_priming_with_zero_frames_is_disabled() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::codec_priming(0, spec.sample_rate);
    let mut ready = trimmer.push(chunk(spec, 0, 64));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 64);
}

#[kithara::test]
fn codec_priming_drops_leading_frames_and_fades_in() {
    let spec = mono_spec();
    // Use a synthetic constant signal so the fade-in shape is
    // easy to inspect: every sample is 1.0 prior to fading.
    let trim = 100u64;
    let trim_len = usize::try_from(trim).expect("BUG: test trim fits in usize");
    let total_frames = trim_len + fade_frames_for(spec) + 32;
    let pcm = vec![1.0_f32; total_frames];
    let mut trimmer = GaplessTrimmer::codec_priming(trim, spec.sample_rate);

    let ready = trimmer.push(custom_chunk(spec, 0, pcm));
    let pcm_out = collect_pcm(&ready);
    assert_eq!(pcm_out.len(), total_frames - trim_len);

    // Anti-click guarantees we want for downstream:
    //   - The first surviving frame is at very low amplitude (no
    //     hard step from 0 to full level).
    //   - Amplitude grows monotonically through the fade window.
    //   - After the fade window we are back at full level.
    let fade_len = fade_frames_for(spec);
    assert!(
        pcm_out[0].abs() < 0.05,
        "first sample {} not soft",
        pcm_out[0]
    );
    for window in pcm_out[..fade_len].windows(2) {
        assert!(
            window[1] >= window[0] - 1e-6,
            "fade-in must be monotonically increasing: {:?}",
            window
        );
    }
    for &sample in &pcm_out[fade_len..] {
        assert!(
            (sample - 1.0).abs() < 1e-5,
            "post-fade sample {sample} != 1.0"
        );
    }
}

#[kithara::test]
fn codec_priming_metadata_takes_precedence_when_combined() {
    // `from_info` is the metadata path; codec_priming is only the
    // fallback. The pipeline picks one or the other — but verify
    // that calling both constructors does not double-trim by
    // accident through some shared field.
    let spec = mono_spec();
    let metadata_trimmer = GaplessTrimmer::from_info(GaplessInfo {
        leading_frames: 50,
        trailing_frames: 0,
    });
    let codec_trimmer = GaplessTrimmer::codec_priming(50, spec.sample_rate);
    let pcm = vec![0.5_f32; 200];

    let mut from_info = metadata_trimmer;
    let from_info_out = collect_pcm(&from_info.push(custom_chunk(spec, 0, pcm.clone())));
    // No fade-in: first surviving sample is intact.
    assert_eq!(from_info_out[0], 0.5);

    let mut from_codec = codec_trimmer;
    let from_codec_out = collect_pcm(&from_codec.push(custom_chunk(spec, 0, pcm)));
    // Fade-in active: first surviving sample is much smaller.
    assert!(from_codec_out[0].abs() < 0.5 * 0.1);
}

// ---- Silence-trim mode ----

#[kithara::test]
fn silence_trim_below_threshold_is_trimmed() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    // 300 leading frames at -70 dB (below the -60 dB threshold) +
    // 100 frames at full amplitude.
    let mut pcm = vec![0.0003_f32; 300];
    pcm.extend(std::iter::repeat_n(0.5, 100));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    // We expect the 300 quiet frames gone, leaving 100 frames
    // (with a fade-in shaping the first ~144 of them).
    assert_eq!(pcm_out.len(), 100);
    assert!(pcm_out[0].abs() < 0.05, "fade-in must soften the boundary");
}

#[kithara::test]
fn silence_trim_above_threshold_preserves_audio() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    // 300 frames at -50 dB (~3.16e-3) — above the -60 dB threshold.
    // This is a quiet but real signal, and must not be trimmed.
    let pcm = vec![3.16e-3_f32; 300];
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 300);
    // No fade-in applied — original samples should be intact.
    assert!(pcm_out.iter().all(|s| (*s - 3.16e-3).abs() < 1e-7));
}

#[kithara::test]
fn silence_trim_preserves_quiet_intro_below_threshold_then_above() {
    // Quiet pre-roll that *crosses* the threshold inside the scan
    // window: first 200 frames -70 dB, then 200 frames at -55 dB.
    // The boundary lands at frame 200 → trim 200, fade-in. Then
    // the surviving content should otherwise be untouched.
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let mut pcm = vec![0.0003_f32; 200];
    pcm.extend(std::iter::repeat_n(0.001_8, 200));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 200);
    let fade_len = fade_frames_for(spec).min(pcm_out.len());
    // After the fade window we should be at the original level.
    for &sample in &pcm_out[fade_len..] {
        assert!((sample - 0.001_8).abs() < 1e-7);
    }
}

#[kithara::test]
fn silence_trim_min_frames_boundary_under_min() {
    // 31 silent frames is below the 32-frame threshold → no trim.
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let mut pcm = vec![0.0_f32; 31];
    pcm.extend(std::iter::repeat_n(0.5, 64));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 95);
    // First sample is the original 0.0 — no fade-in.
    assert_eq!(pcm_out[0], 0.0);
}

#[kithara::test]
fn silence_trim_min_frames_boundary_at_min() {
    // Exactly 32 silent frames → trim, fade-in armed.
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let mut pcm = vec![0.0_f32; 32];
    pcm.extend(std::iter::repeat_n(0.5, 64));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 64);
    assert!(pcm_out[0].abs() < 0.05);
}

#[kithara::test]
fn silence_trim_scan_window_exhausted_preserves_audio() {
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 256,
        trim_trailing: false,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    // 300 silent frames — longer than the scan window → keep
    // everything. This guards against eating long fade-ins.
    let pcm = vec![0.0_f32; 300];
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 300);
    assert!(pcm_out.iter().all(|s| *s == 0.0));
}

#[kithara::test]
fn silence_trim_no_op_with_immediate_content() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = vec![0.5_f32; 256];
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 256);
    // No trim, no fade-in.
    assert!(pcm_out.iter().all(|s| (*s - 0.5).abs() < 1e-7));
}

#[kithara::test]
fn silence_trim_trailing_disabled_by_default() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let mut pcm = vec![0.5_f32; 64];
    pcm.extend(std::iter::repeat_n(0.0, 64));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    // Trailing silence preserved.
    assert_eq!(pcm_out.len(), 128);
}

#[kithara::test]
fn silence_trim_trailing_enabled() {
    // Window-RMS based trailing scan: the silent suffix must fit at
    // least one full window (5 ms @ 48 kHz = 240 frames). Use a
    // 480-frame silent tail and verify it gets dropped, while the
    // audible head is preserved (modulo the new trailing fade-out
    // applied to the last `Consts::FADE_OUT_DURATION_MS` of audio).
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 4096,
        trim_trailing: true,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    let audible_frames = 256;
    let silent_frames = 480;
    let mut pcm = vec![0.5_f32; audible_frames];
    pcm.extend(std::iter::repeat_n(0.0, silent_frames));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), audible_frames);

    let fade_frames =
        (u64::from(spec.sample_rate) * Consts::FADE_OUT_DURATION_MS / 1000).max(1) as usize;
    // Pre-fade region keeps the original signal.
    let untouched = pcm_out.len().saturating_sub(fade_frames);
    for &sample in &pcm_out[..untouched] {
        assert!(
            (sample - 0.5).abs() < 1e-7,
            "untouched sample changed: {sample}"
        );
    }
    // Fade-out region monotonically decreases from ~0.5 to ~0.
    for window in pcm_out[untouched..].windows(2) {
        assert!(
            window[1] <= window[0] + 1e-6,
            "fade-out must be monotonically non-increasing: {window:?}"
        );
    }
    assert!(
        pcm_out
            .last()
            .copied()
            .is_some_and(|sample| sample.abs() < 0.05),
        "last sample must be near zero after fade-out"
    );
}

#[kithara::test]
fn silence_trim_trailing_window_rms_ignores_zero_crossings_in_audible_signal() {
    // Regression: per-sample trailing search used to misclassify
    // sine zero-crossings as silence (samples around ZCR could fall
    // below the -60 dB floor for 1-3 frames per cycle), eating into
    // audible content. Window-RMS over a few-millisecond window
    // integrates over many cycles and stays well above the floor.
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 4096,
        trim_trailing: true,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    // 4800 frames of an 800 Hz sine at amplitude 0.5 (100 ms @ 48 kHz),
    // followed by 480 frames of true silence (10 ms padding).
    let sine_frames: u32 = 4_800;
    let silent_frames: u32 = 480;
    let mut pcm = Vec::with_capacity((sine_frames + silent_frames) as usize);
    for n in 0..sine_frames {
        let t = f64::from(n) / f64::from(spec.sample_rate);
        let s: f64 = 0.5 * (2.0 * std::f64::consts::PI * 800.0 * t).sin();
        // f64 → f32 narrowing is the canonical PCM-sample conversion;
        // routing through `AsPrimitive` documents that intent and
        // keeps the cast outside Clippy's `as`-keyword lints.
        pcm.push(num_traits::cast::AsPrimitive::<f32>::as_(s));
    }
    pcm.extend(std::iter::repeat_n(0.0, silent_frames as usize));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    // Trim must drop only the silent padding, not any audible cycles.
    assert_eq!(pcm_out.len(), sine_frames as usize);

    // RMS of the surviving signal, *excluding* the fade-out window,
    // must match a 0.5-amplitude sine (≈ 0.354).
    let fade_frames =
        (u64::from(spec.sample_rate) * Consts::FADE_OUT_DURATION_MS / 1000).max(1) as usize;
    let pre_fade_end = pcm_out.len().saturating_sub(fade_frames);
    let pre_fade = &pcm_out[..pre_fade_end];
    let pre_fade_len = u32::try_from(pre_fade.len()).expect("BUG: pre-fade window fits in u32");
    let sum_sq: f64 = pre_fade.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms_f64: f64 = (sum_sq / f64::from(pre_fade_len)).sqrt();
    // f64 → f32 narrowing is the canonical PCM-sample conversion;
    // routing through `AsPrimitive` keeps it outside `as`-cast lints.
    let rms: f32 = num_traits::cast::AsPrimitive::<f32>::as_(rms_f64);
    assert!(
        (rms - 0.5 / std::f32::consts::SQRT_2).abs() < 0.02,
        "RMS of surviving signal should match a 0.5 sine: got {rms}"
    );
}

#[kithara::test]
fn silence_trim_seek_disables_leading_only() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    assert!(trimmer.push(silent_chunk(spec, 0, 128)).is_empty());
    trimmer.notify_seek();

    // After seek, leading trim is off — silence after the seek
    // point is real content the user requested.
    assert!(
        trimmer
            .push(custom_chunk(spec, 128, vec![0.0, 0.0, 3.0, 4.0]))
            .is_empty()
    );

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.meta.frame_offset, 128);
    assert_eq!(out.samples(), &[0.0, 0.0, 3.0, 4.0]);
}

#[kithara::test]
fn silence_trim_preserves_all_silence_track() {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    assert!(trimmer.push(silent_chunk(spec, 0, 128)).is_empty());

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.frames(), 128);
    assert!(out.samples().iter().all(|sample| *sample == 0.0));
}

#[kithara::test]
fn silence_trim_respects_multi_channel_threshold() {
    // Per-frame check: a frame is silent only if *all* channels
    // are below threshold. Stereo input with channel 1 louder
    // than threshold ends silence for the whole frame.
    let spec = stereo_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    // 32 silent stereo frames + one frame [0.0, 2e-3] (2e-3 is
    // louder than the -60 dB ≈ 1e-3 threshold).
    assert!(
        trimmer
            .push(custom_chunk(
                spec,
                0,
                vec![0.0; 32 * usize::from(spec.channels)]
            ))
            .is_empty()
    );
    assert!(
        trimmer
            .push(custom_chunk(spec, 32, vec![0.0, 2.0e-3]))
            .is_empty()
    );

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.meta.frame_offset, 32);
    assert_eq!(out.frames(), 1);
    // Fade-in only spans frames; one frame fits inside the fade
    // window so the value is attenuated by the first ramp gain.
    assert!(out.samples()[1].abs() < 2.0e-3);
}

#[kithara::test]
fn silence_trim_does_not_introduce_click_at_boundary() {
    // The whole point of the fade-in: when the audio jumps from
    // pure silence to a non-trivial level, the surviving stream
    // must not start with the full step. Verified by inspecting
    // the first sample after the trim.
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let mut pcm = vec![0.0_f32; 64];
    pcm.extend(std::iter::repeat_n(1.0, 256));
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert!(
        pcm_out[0].abs() < 0.1,
        "boundary sample {} too loud",
        pcm_out[0]
    );
}
