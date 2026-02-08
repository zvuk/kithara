//! HLS segment boundary analyzer.
//!
//! Decodes pre-downloaded HLS segments individually with Symphonia,
//! saves PCM to WAV files, and analyzes boundaries between segments.
//!
//! Usage:
//!   cargo run -p kithara-decode --example segment_analyzer -- /tmp/hls_analysis
//!
//! Expected directory structure:
//!   /tmp/hls_analysis/
//!     slq/init.mp4, slq/seg_1.m4s, slq/seg_2.m4s, ...
//!     smq/init.mp4, smq/seg_1.m4s, ...

use std::{env, fs, io::Cursor, path::Path};

use symphonia::{
    core::{
        codecs::{CodecParameters, audio::AudioDecoderOptions},
        formats::{FormatOptions, TrackType},
        io::MediaSourceStream,
    },
    default::formats::IsoMp4Reader,
};

fn main() {
    let base_dir = env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/hls_analysis".to_string());
    let base = Path::new(&base_dir);

    let variants = ["slq", "smq", "shq", "flac"];
    let segments: Vec<usize> = (1..=10).collect();

    let out_dir = base.join("wav");
    fs::create_dir_all(&out_dir).expect("Failed to create wav output dir");

    let mut all_results: Vec<(String, usize, SegmentResult)> = Vec::new();

    for variant in &variants {
        let init_path = base.join(variant).join("init.mp4");
        if !init_path.exists() {
            eprintln!("Skipping {variant}: no init.mp4");
            continue;
        }
        let init_data = fs::read(&init_path).expect("Failed to read init segment");
        println!("\n{}", "=".repeat(80));
        println!("VARIANT: {variant} (init: {} bytes)", init_data.len());
        println!("{}", "=".repeat(80));

        let mut prev_tail: Option<Vec<f32>> = None;
        let mut prev_seg_idx: usize = 0;

        for &seg_idx in &segments {
            let seg_path = base.join(variant).join(format!("seg_{seg_idx}.m4s"));
            if !seg_path.exists() {
                continue;
            }
            let seg_data = fs::read(&seg_path).expect("Failed to read segment");

            let mut fmp4_data = Vec::with_capacity(init_data.len() + seg_data.len());
            fmp4_data.extend_from_slice(&init_data);
            fmp4_data.extend_from_slice(&seg_data);

            match decode_fmp4(&fmp4_data) {
                Ok(decoded) => {
                    let wav_path = out_dir.join(format!("{variant}_seg{seg_idx}.wav"));
                    write_wav(
                        &wav_path,
                        &decoded.samples,
                        decoded.sample_rate,
                        decoded.channels,
                    );

                    let ch = decoded.channels as usize;
                    let total_frames = if ch > 0 {
                        decoded.samples.len() / ch
                    } else {
                        0
                    };

                    println!(
                        "\n  Segment {seg_idx}: {total_frames} frames, {}ch, {}Hz, {} bytes media",
                        decoded.channels,
                        decoded.sample_rate,
                        seg_data.len()
                    );

                    // Show first 4 frames.
                    let head_frames = 4.min(total_frames);
                    if head_frames > 0 && ch > 0 {
                        print!("    HEAD: [");
                        for f in 0..head_frames {
                            if f > 0 {
                                print!(" | ");
                            }
                            for c in 0..ch {
                                if c > 0 {
                                    print!(", ");
                                }
                                print!("{:.6}", decoded.samples[f * ch + c]);
                            }
                        }
                        println!("]");
                    }

                    // Priming detection.
                    let priming = count_priming_frames(&decoded.samples, ch);
                    if priming > 0 {
                        println!(
                            "    PRIMING: {priming} near-zero frames ({:.1}ms at {}Hz)",
                            priming as f64 * 1000.0 / decoded.sample_rate as f64,
                            decoded.sample_rate
                        );
                    }

                    // Show last 4 frames.
                    let tail_frames = 4.min(total_frames);
                    if tail_frames > 0 && ch > 0 {
                        let tail_start = decoded.samples.len() - tail_frames * ch;
                        print!("    TAIL: [");
                        for f in 0..tail_frames {
                            if f > 0 {
                                print!(" | ");
                            }
                            for c in 0..ch {
                                if c > 0 {
                                    print!(", ");
                                }
                                print!("{:.6}", decoded.samples[tail_start + f * ch + c]);
                            }
                        }
                        println!("]");
                    }

                    // Boundary with previous segment.
                    if let Some(ref prev) = prev_tail {
                        let boundary = analyze_boundary(prev, &decoded.samples, ch);
                        println!(
                            "    BOUNDARY {prev_seg_idx}->{seg_idx}: max_delta={:.6} ({})",
                            boundary.max_delta, boundary.severity
                        );
                        println!("      last:  {:?}", fmt_samples(&boundary.last_frame));
                        println!("      first: {:?}", fmt_samples(&boundary.first_frame));
                    }

                    // Store tail for next boundary check.
                    let tail_start = decoded.samples.len().saturating_sub(4 * ch);
                    prev_tail = Some(decoded.samples[tail_start..].to_vec());
                    prev_seg_idx = seg_idx;

                    all_results.push((
                        variant.to_string(),
                        seg_idx,
                        SegmentResult {
                            total_frames,
                            sample_rate: decoded.sample_rate,
                            channels: decoded.channels,
                            priming_frames: priming,
                            head: decoded.samples[..4.min(decoded.samples.len()) * ch].to_vec(),
                            tail: decoded.samples[tail_start..].to_vec(),
                        },
                    ));
                }
                Err(e) => {
                    eprintln!("  Segment {seg_idx}: DECODE ERROR: {e}");
                }
            }
        }
    }

    // Cross-variant analysis.
    println!("\n{}", "=".repeat(80));
    println!("CROSS-VARIANT BOUNDARY ANALYSIS (ABR switch simulation)");
    println!("{}", "=".repeat(80));

    let switch_pairs = [
        ("slq", "smq", "AAC 66k -> AAC 134k"),
        ("slq", "shq", "AAC 66k -> AAC 270k"),
        ("slq", "flac", "AAC 66k -> FLAC 989k"),
        ("smq", "shq", "AAC 134k -> AAC 270k"),
        ("smq", "flac", "AAC 134k -> FLAC 989k"),
    ];

    for (from_var, to_var, desc) in &switch_pairs {
        println!("\n  {desc}:");
        for switch_at in [3, 5, 7] {
            let from = all_results
                .iter()
                .find(|(v, s, _)| v == from_var && *s == switch_at);
            let to = all_results
                .iter()
                .find(|(v, s, _)| v == to_var && *s == switch_at + 1);

            if let (Some((_, _, from_res)), Some((_, _, to_res))) = (from, to) {
                let ch = from_res.channels.max(to_res.channels) as usize;
                if ch > 0 && !from_res.tail.is_empty() && !to_res.head.is_empty() {
                    // Use actual head samples (which may include priming).
                    let boundary = analyze_boundary(&from_res.tail, &to_res.head, ch);
                    println!(
                        "    seg {switch_at}->{}: max_delta={:.6} ({}) [priming_in_new={}]",
                        switch_at + 1,
                        boundary.max_delta,
                        boundary.severity,
                        to_res.priming_frames
                    );
                    println!("      tail:  {:?}", fmt_samples(&boundary.last_frame));
                    println!("      head:  {:?}", fmt_samples(&boundary.first_frame));

                    // Also show what the FIRST NON-PRIMING sample is.
                    let skip = to_res.priming_frames * ch;
                    if skip > 0 && skip < to_res.head.len() {
                        let after_priming: Vec<f32> =
                            to_res.head[skip..skip + ch.min(to_res.head.len() - skip)].to_vec();
                        println!("      after_priming: {:?}", fmt_samples(&after_priming));
                    }
                }
            }
        }
    }

    println!("\n\nWAV files saved to: {}", out_dir.display());
}

struct DecodedSegment {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

struct SegmentResult {
    total_frames: usize,
    sample_rate: u32,
    channels: u16,
    priming_frames: usize,
    head: Vec<f32>,
    tail: Vec<f32>,
}

struct BoundaryInfo {
    max_delta: f32,
    severity: &'static str,
    last_frame: Vec<f32>,
    first_frame: Vec<f32>,
}

fn decode_fmp4(data: &[u8]) -> Result<DecodedSegment, String> {
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
                let spec = decoded.spec();
                let ch = spec.channels().count();
                let num_samples = decoded.samples_interleaved();
                if num_samples == 0 {
                    continue;
                }
                let mut pcm = vec![0.0f32; num_samples];
                decoded.copy_to_slice_interleaved(&mut pcm);

                // Verify channels match.
                if ch as u16 != channels {
                    eprintln!(
                        "      WARNING: channel count changed: expected {channels}, got {ch}"
                    );
                }

                all_samples.extend_from_slice(&pcm);
            }
            Err(e) => {
                eprintln!("      decode error: {e}");
            }
        }
    }

    Ok(DecodedSegment {
        samples: all_samples,
        sample_rate,
        channels,
    })
}

fn count_priming_frames(samples: &[f32], channels: usize) -> usize {
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
        if max_abs < 0.0001 {
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn analyze_boundary(tail: &[f32], head: &[f32], channels: usize) -> BoundaryInfo {
    let last_start = tail.len().saturating_sub(channels);
    let last_frame: Vec<f32> = tail[last_start..].to_vec();
    let first_frame: Vec<f32> = head[..channels.min(head.len())].to_vec();

    let mut max_delta: f32 = 0.0;
    for ch in 0..channels.min(last_frame.len()).min(first_frame.len()) {
        let delta = (first_frame[ch] - last_frame[ch]).abs();
        max_delta = max_delta.max(delta);
    }

    let severity = if max_delta > 0.5 {
        "SEVERE"
    } else if max_delta > 0.1 {
        "MODERATE"
    } else if max_delta > 0.01 {
        "MILD"
    } else {
        "SMOOTH"
    };

    BoundaryInfo {
        max_delta,
        severity,
        last_frame,
        first_frame,
    }
}

fn fmt_samples(samples: &[f32]) -> Vec<String> {
    samples.iter().map(|s| format!("{s:.6}")).collect()
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
