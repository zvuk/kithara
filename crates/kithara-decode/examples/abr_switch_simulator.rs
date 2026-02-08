//! ABR switch simulator — experimental verification of transition strategies.
//!
//! 1. Decodes all segments per variant continuously (single reader) → full WAV.
//! 2. Simulates ABR switch at segment boundaries.
//!    For each switch point, generates:
//!      - `_raw.wav`     — hard cut (current behavior, clicks)
//!      - `_overlap.wav` — silence detection + cross-correlation + crossfade
//!      - `_apple.wav`   — Apple implicit delay (2112 for AAC, 0 for FLAC) + crossfade
//!
//! Usage:
//!   cargo run -p kithara-decode --example abr_switch_simulator -- /tmp/hls_analysis

use std::{env, fs, io::Cursor, path::Path};

use symphonia::{
    core::{
        codecs::{CodecParameters, audio::AudioDecoderOptions},
        formats::{FormatOptions, TrackType},
        io::MediaSourceStream,
    },
    default::formats::IsoMp4Reader,
};

/// Apple's implicit encoder delay for AAC (TN2258 / QuickTime Appendix G).
/// When no `elst` or `iTunSMPB` is present, Apple assumes 2112 samples.
const APPLE_IMPLICIT_AAC_DELAY: usize = 2112;

/// FLAC has no standard implicit encoder delay.
const APPLE_IMPLICIT_FLAC_DELAY: usize = 0;

/// Returns Apple's implicit encoder delay for a variant name.
fn implicit_delay(variant: &str) -> usize {
    if variant == "slossless" {
        APPLE_IMPLICIT_FLAC_DELAY
    } else {
        // slq, smq, shq — all AAC
        APPLE_IMPLICIT_AAC_DELAY
    }
}

fn main() {
    let base_dir = env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/hls_analysis".to_string());
    let base = Path::new(&base_dir);

    let out_dir = base.join("abr_sim");
    fs::create_dir_all(&out_dir).expect("Failed to create output dir");

    let variants = ["slq", "smq", "shq", "slossless"];
    let max_seg = 37;

    // =========================================================================
    // Part 1: Full continuous WAV per variant.
    // =========================================================================
    println!("{}", "=".repeat(80));
    println!("PART 1: Continuous decode (single reader per variant, all segments)");
    println!("{}", "=".repeat(80));

    for variant in &variants {
        let decoded = match decode_continuous(base, variant, 1, max_seg) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {variant}: ERROR: {e}");
                continue;
            }
        };
        let wav_path = out_dir.join(format!("{variant}_continuous.wav"));
        write_wav(
            &wav_path,
            &decoded.samples,
            decoded.sample_rate,
            decoded.channels,
        );
        println!(
            "  {variant}: {} frames, {}ch, {}Hz → {}",
            decoded.frame_count(),
            decoded.channels,
            decoded.sample_rate,
            wav_path.display()
        );
    }

    // =========================================================================
    // Part 1.5: Measure initial silence (encoder delay) per variant.
    // =========================================================================
    println!();
    println!("{}", "=".repeat(80));
    println!("ENCODER DELAY: initial silence per variant (continuous decode)");
    println!("{}", "=".repeat(80));

    for variant in &variants {
        let decoded = match decode_continuous(base, variant, 1, max_seg) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {variant}: ERROR: {e}");
                continue;
            }
        };
        let ch = decoded.channels as usize;
        let silence = count_leading_silence(&decoded.samples, ch, 0.001);
        let silence_ms = silence as f64 * 1000.0 / decoded.sample_rate as f64;
        let apple = implicit_delay(variant);
        let apple_delta = silence as isize - apple as isize;
        println!(
            "  {variant}: measured={silence} ({silence_ms:.1}ms)  \
             apple_implicit={apple}  delta={apple_delta}  \
             total {} frames",
            decoded.frame_count()
        );

        // Also check single segment 1 cold-start.
        let cold = match decode_cold_start(base, variant, 1) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {variant} cold seg1: ERROR: {e}");
                continue;
            }
        };
        let cold_silence = count_leading_silence(&cold.samples, ch, 0.001);
        let cold_ms = cold_silence as f64 * 1000.0 / cold.sample_rate as f64;
        println!(
            "    cold-start seg1: {cold_silence} silent frames ({cold_ms:.1}ms), \
             total {} frames",
            cold.frame_count()
        );
    }

    // =========================================================================
    // Part 2: ABR switch transitions.
    // =========================================================================
    let switch_pairs: &[(&str, &str, &str)] = &[
        ("slq", "slossless", "AAC 66k → FLAC"),
        ("slossless", "slq", "FLAC → AAC 66k"),
        //("smq", "slossless", "AAC 134k → FLAC"),
        //("shq", "slossless", "AAC 270k → FLAC"),
        //("slq", "smq", "AAC 66k → AAC 134k"),
        //("slq", "shq", "AAC 66k → AAC 270k"),
    ];

    for &(from_var, to_var, desc) in switch_pairs {
        println!();
        println!("{}", "=".repeat(80));
        println!("PART 2: {desc}  ({from_var} → {to_var})");
        println!("{}", "=".repeat(80));

        let switch_points: Vec<usize> = vec![1, 5, 17];
        for switch_after in switch_points {
            let cold_seg = switch_after + 1;

            // Old variant decoded continuously: segs 1..=switch_after.
            let old = match decode_continuous(base, from_var, 1, switch_after) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("  seg {switch_after}→{cold_seg}: ERROR old: {e}");
                    continue;
                }
            };

            // New variant cold-start: init + seg cold_seg.
            let new_cold = match decode_cold_start(base, to_var, cold_seg) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("  seg {switch_after}→{cold_seg}: ERROR new: {e}");
                    continue;
                }
            };

            let ch = old.channels.max(new_cold.channels) as usize;
            let rate = old.sample_rate;
            let out_ch = old.channels;

            // Decode both variants continuously 1..=cold_seg (warm) for overlap strategies.
            let old_full = match decode_continuous(base, from_var, 1, cold_seg) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("  seg {switch_after}→{cold_seg}: ERROR old_full: {e}");
                    continue;
                }
            };
            let new_full = match decode_continuous(base, to_var, 1, cold_seg) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("  seg {switch_after}→{cold_seg}: ERROR new_full: {e}");
                    continue;
                }
            };

            // ── Strategy 1: Raw (hard cut) — current behavior ───────────
            let raw = concat_pcm(&old.samples, &new_cold.samples);
            let raw_path = out_dir.join(format!("{from_var}_to_{to_var}_at{cold_seg}_raw.wav"));
            write_wav(&raw_path, &raw, rate, out_ch);

            let tail_old = last_frame(&old.samples, ch);
            let head_cold = first_frame(&new_cold.samples, ch);
            let delta_raw = max_delta(&tail_old, &head_cold);

            // ── Strategy 2: Silence detection + xcorr + crossfade ───────
            let old_delay = count_leading_silence(&old_full.samples, ch, 0.001);
            let new_delay = count_leading_silence(&new_full.samples, ch, 0.001);
            let delay_diff_measured = old_delay as isize - new_delay as isize;

            let old_pos = old.samples.len();
            let overlap_result = build_overlap(
                &old_full.samples,
                &new_full.samples,
                old_pos,
                delay_diff_measured,
                ch,
                rate,
                true, // use cross-correlation
            );

            let overlap_path =
                out_dir.join(format!("{from_var}_to_{to_var}_at{cold_seg}_overlap.wav"));
            write_wav(&overlap_path, &overlap_result.output, rate, out_ch);

            // ── Strategy 3: Apple implicit delay + crossfade ────────────
            let apple_old = implicit_delay(from_var) as isize;
            let apple_new = implicit_delay(to_var) as isize;
            let delay_diff_apple = apple_old - apple_new;

            let apple_result = build_overlap(
                &old_full.samples,
                &new_full.samples,
                old_pos,
                delay_diff_apple,
                ch,
                rate,
                true, // use cross-correlation
            );

            let apple_path = out_dir.join(format!("{from_var}_to_{to_var}_at{cold_seg}_apple.wav"));
            write_wav(&apple_path, &apple_result.output, rate, out_ch);

            // ── Report ──────────────────────────────────────────────────
            println!(
                "  seg {switch_after}→{cold_seg}:  raw={delta_raw:.4} ({})",
                severity(delta_raw),
            );
            println!(
                "    silence_detect: delay_diff={delay_diff_measured:+}  \
                 xcorr={:+}  pos={}→{}",
                overlap_result.xcorr_offset,
                old_pos / ch.max(1),
                overlap_result.new_pos_aligned / ch.max(1),
            );
            println!(
                "    apple_implicit: delay_diff={delay_diff_apple:+}  \
                 xcorr={:+}  pos={}→{}  \
                 (delta from measured: {})",
                apple_result.xcorr_offset,
                old_pos / ch.max(1),
                apple_result.new_pos_aligned / ch.max(1),
                delay_diff_apple - delay_diff_measured,
            );
        }
    }

    println!();
    println!("{}", "=".repeat(80));
    println!("All WAV files: {}", out_dir.display());
    println!();
    println!("Legend:");
    println!("  *_raw.wav     — hard cut (clicks)");
    println!("  *_overlap.wav — silence detection + xcorr + 20ms crossfade");
    println!("  *_apple.wav   — Apple implicit 2112/0 + xcorr + 20ms crossfade");
}

// =============================================================================
// Overlap builder.
// =============================================================================

struct OverlapResult {
    output: Vec<f32>,
    xcorr_offset: isize,
    new_pos_aligned: usize,
}

/// Build overlap-crossfade output using the given delay_diff.
fn build_overlap(
    old_full: &[f32],
    new_full: &[f32],
    old_pos: usize,
    delay_diff: isize,
    ch: usize,
    rate: u32,
    use_xcorr: bool,
) -> OverlapResult {
    // Corresponding position in new_full: compensate for delay difference.
    let new_pos_est = (old_pos as isize - delay_diff * ch as isize).max(0) as usize;
    let new_pos = (new_pos_est / ch) * ch; // frame-align

    // Cross-correlation to fine-tune alignment.
    let xcorr_offset = if use_xcorr {
        let search_radius = 128 * ch;
        let window = 2048 * ch;
        cross_correlate(
            old_full,
            old_pos,
            new_full,
            new_pos,
            window,
            search_radius,
            ch,
        )
    } else {
        0
    };
    let new_pos_aligned =
        ((new_pos as isize + xcorr_offset * ch as isize).max(0) as usize / ch) * ch;

    // Short crossfade (20ms) at the aligned transition point.
    let cf_samples = (rate as usize * 20 / 1000) * ch;
    let old_cf_start = old_pos.saturating_sub(cf_samples);
    let new_cf_start = new_pos_aligned.saturating_sub(cf_samples);
    let old_tail = &old_full[old_cf_start..];
    let new_from = &new_full[new_cf_start.min(new_full.len())..];

    let cf_len = cf_samples.min(old_tail.len()).min(new_from.len());
    let cf_frames = if ch > 0 { cf_len / ch } else { 0 };

    let mut out = Vec::with_capacity(old_pos + new_from.len());
    // Pre-crossfade: old signal up to crossfade start.
    out.extend_from_slice(&old_full[..old_cf_start]);
    // Crossfade zone.
    for frame in 0..cf_frames {
        let t = (frame + 1) as f32 / (cf_frames + 1) as f32;
        for c in 0..ch {
            let idx = frame * ch + c;
            let old_val = old_tail[idx];
            let new_val = new_from[idx];
            out.push(old_val * (1.0 - t) + new_val * t);
        }
    }
    // Post-crossfade: new signal continues.
    if cf_len < new_from.len() {
        out.extend_from_slice(&new_from[cf_len..]);
    }

    OverlapResult {
        output: out,
        xcorr_offset,
        new_pos_aligned,
    }
}

// =============================================================================
// Decoding.
// =============================================================================

struct DecodedPcm {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

impl DecodedPcm {
    fn frame_count(&self) -> usize {
        if self.channels > 0 {
            self.samples.len() / self.channels as usize
        } else {
            0
        }
    }
}

/// Decode segments `first..=last` continuously with a single reader.
fn decode_continuous(
    base: &Path,
    variant: &str,
    first: usize,
    last: usize,
) -> Result<DecodedPcm, String> {
    let init_path = base.join(variant).join("init.mp4");
    let init_data = fs::read(&init_path).map_err(|e| format!("read init: {e}"))?;

    let mut fmp4_data = init_data;
    for seg_idx in first..=last {
        let seg_path = base.join(variant).join(format!("seg_{seg_idx}.m4s"));
        let seg_data = fs::read(&seg_path).map_err(|e| format!("read seg_{seg_idx}: {e}"))?;
        fmp4_data.extend_from_slice(&seg_data);
    }

    decode_fmp4(&fmp4_data)
}

/// Decode a single segment with a fresh reader (cold start).
fn decode_cold_start(base: &Path, variant: &str, seg_idx: usize) -> Result<DecodedPcm, String> {
    let init_path = base.join(variant).join("init.mp4");
    let init_data = fs::read(&init_path).map_err(|e| format!("read init: {e}"))?;

    let seg_path = base.join(variant).join(format!("seg_{seg_idx}.m4s"));
    let seg_data = fs::read(&seg_path).map_err(|e| format!("read seg_{seg_idx}: {e}"))?;

    let mut fmp4_data = Vec::with_capacity(init_data.len() + seg_data.len());
    fmp4_data.extend_from_slice(&init_data);
    fmp4_data.extend_from_slice(&seg_data);

    decode_fmp4(&fmp4_data)
}

/// Decode an fMP4 buffer (init + media segments) to interleaved f32 PCM.
fn decode_fmp4(data: &[u8]) -> Result<DecodedPcm, String> {
    let cursor = Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let format_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };

    let reader = IsoMp4Reader::try_new(mss, format_opts)
        .map_err(|e| format!("Failed to create reader: {e}"))?;

    let mut format_reader: Box<dyn symphonia::core::formats::FormatReader> = Box::new(reader);

    let track = format_reader
        .default_track(TrackType::Audio)
        .ok_or("No audio track found")?
        .clone();

    let track_id = track.id;

    let codec_params = match &track.codec_params {
        Some(CodecParameters::Audio(params)) => params.clone(),
        _ => return Err("No audio codec params".to_string()),
    };

    let sample_rate = codec_params.sample_rate.unwrap_or(44100);
    let channels = codec_params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(2);

    let decoder_opts = AudioDecoderOptions { verify: false };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec_params, &decoder_opts)
        .map_err(|e| format!("Failed to create decoder: {e}"))?;

    let mut all_samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format_reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                eprintln!("      packet error: {e}");
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let num_samples = decoded.samples_interleaved();
                if num_samples == 0 {
                    continue;
                }
                let mut pcm = vec![0.0f32; num_samples];
                decoded.copy_to_slice_interleaved(&mut pcm);
                all_samples.extend_from_slice(&pcm);
            }
            Err(e) => {
                eprintln!("      decode error: {e}");
            }
        }
    }

    Ok(DecodedPcm {
        samples: all_samples,
        sample_rate,
        channels,
    })
}

// =============================================================================
// PCM manipulation.
// =============================================================================

fn concat_pcm(a: &[f32], b: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out
}

// =============================================================================
// Analysis helpers.
// =============================================================================

/// Count leading near-silent frames (all channels below threshold).
fn count_leading_silence(samples: &[f32], channels: usize, threshold: f32) -> usize {
    if channels == 0 || samples.is_empty() {
        return 0;
    }
    let frames = samples.len() / channels;
    let mut count = 0;
    for frame in 0..frames {
        let offset = frame * channels;
        let max_abs = (0..channels)
            .map(|ch| samples[offset + ch].abs())
            .fold(0.0_f32, f32::max);
        if max_abs < threshold {
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Cross-correlate two signals around given positions to find fine alignment.
/// Returns offset in frames: positive = shift new_signal right.
fn cross_correlate(
    a: &[f32],
    a_center: usize,
    b: &[f32],
    b_center: usize,
    window: usize, // in samples (interleaved)
    radius: usize, // search radius in samples (interleaved)
    channels: usize,
) -> isize {
    if channels == 0 || a.is_empty() || b.is_empty() {
        return 0;
    }
    let frame_radius = radius / channels;
    let frame_window = window / channels;

    let mut best_corr = f64::MIN;
    let mut best_offset: isize = 0;

    for shift in -(frame_radius as isize)..=(frame_radius as isize) {
        let mut corr = 0.0_f64;
        let mut count = 0_usize;

        for frame in 0..frame_window {
            let a_idx = a_center as isize + (frame as isize) * channels as isize;
            let b_idx = b_center as isize + (frame as isize + shift) * channels as isize;

            if a_idx >= 0
                && (a_idx as usize + channels) <= a.len()
                && b_idx >= 0
                && (b_idx as usize + channels) <= b.len()
            {
                for c in 0..channels {
                    let va = a[a_idx as usize + c] as f64;
                    let vb = b[b_idx as usize + c] as f64;
                    corr += va * vb;
                }
                count += 1;
            }
        }

        if count > 0 {
            corr /= count as f64;
            if corr > best_corr {
                best_corr = corr;
                best_offset = shift;
            }
        }
    }

    best_offset
}

fn last_frame(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 || samples.len() < channels {
        return vec![];
    }
    samples[samples.len() - channels..].to_vec()
}

fn first_frame(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 || samples.is_empty() {
        return vec![];
    }
    samples[..channels.min(samples.len())].to_vec()
}

fn max_delta(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0_f32, f32::max)
}

fn severity(delta: f32) -> &'static str {
    if delta > 0.5 {
        "SEVERE"
    } else if delta > 0.1 {
        "MODERATE"
    } else if delta > 0.01 {
        "MILD"
    } else {
        "SMOOTH"
    }
}

fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: u16) {
    use std::io::Write;

    let bits_per_sample: u16 = 32;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_size = (samples.len() * 4) as u32;
    let file_size = 36 + data_size;

    let mut buf: Vec<u8> = Vec::with_capacity(file_size as usize + 8);

    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }

    let mut file = fs::File::create(path).expect("Failed to create WAV file");
    file.write_all(&buf).expect("Failed to write WAV");
}
