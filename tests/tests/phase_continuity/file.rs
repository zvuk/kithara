use std::{io::Write, num::NonZeroUsize};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    file::{File, FileConfig, FileSrc},
    stream::Stream,
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, TestTempDir,
};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use tracing::info;

use super::common::{
    CHANNELS, FREQ_HZ, MIN_SIGNAL_AMP, PhaseDrift, READ_FRAMES_AFTER_SEEK, READ_PENDING_RETRIES,
    SAMPLE_RATE, STREAM_FRAMES, SinePhaseSpec, TOLERANCE_SAMPLES, e2e_phase_scan,
    measure_phase_rad_window, seek_phase_scan, wrap_pi,
};

fn format_ext(fmt: SignalFormat) -> Option<&'static str> {
    match fmt {
        SignalFormat::Mp3 => Some("mp3"),
        SignalFormat::Aac => Some("aac"),
        SignalFormat::M4a => Some("m4a"),
        SignalFormat::Flac => Some("flac"),
        SignalFormat::Wav => Some("wav"),
    }
}

async fn run_case(
    format: SignalFormat,
    backend: DecoderBackend,
    ephemeral: bool,
    seek_count: usize,
    bit_rate: Option<u64>,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES as usize),
        format,
        bit_rate,
    };
    let url = helper.sine(&spec, FREQ_HZ).await;

    let temp_dir = TestTempDir::new();
    let store = if ephemeral {
        StoreOptions::builder()
            .cache_dir(temp_dir.path().into())
            .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
            .is_ephemeral(true)
            .build()
    } else {
        StoreOptions::builder()
            .cache_dir(temp_dir.path().into())
            .build()
    };

    let file_config = FileConfig::for_src(url.into()).store(store).build();
    // Park on ring underrun: the offline scan needs no wall-clock pacing.
    let audio_config = AudioConfig::<File>::for_stream(file_config)
        .decoder_backend(backend)
        .maybe_hint(format_ext(format).map(str::to_owned))
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<File>>::new(audio_config)
        .await
        .expect("create Audio<Stream<File>>");

    let total_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("file fixture should report duration");
    info!(
        ?format,
        ?backend,
        ephemeral,
        seek_count,
        total_secs,
        "fixture ready"
    );
    assert!(
        total_secs > 30.0 && total_secs < 120.0,
        "fixture duration out of sane range: {total_secs:.1}s",
    );
    let total_frames_truth = (total_secs * f64::from(SAMPLE_RATE)) as u64;

    let aspec = audio.spec();
    assert_eq!(aspec.sample_rate.get(), SAMPLE_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));

    let drifts = spawn_blocking(move || -> Vec<PhaseDrift> {
        let sine = SinePhaseSpec::default_440();
        if seek_count == 0 {
            e2e_phase_scan(&mut audio, sine, total_frames_truth)
        } else {
            seek_phase_scan(
                &mut audio,
                sine,
                total_secs,
                seek_count,
                0xCAFE_BEEF_F00D_1234u64,
                |_| {},
            )
        }
    })
    .await
    .expect("spawn_blocking joined");

    assert!(
        drifts.is_empty(),
        "phase continuity broken on {} scan(s) (format={format:?} backend={backend:?} seek_count={seek_count}): {drifts:?}",
        drifts.len(),
    );
}

/// Encode the sine fixture once (over the loopback test server, the only place
/// the `pub(crate)` encoder is reachable from `tests/`) and persist the raw
/// encoded bytes to a file inside `temp_dir`. This HTTP fetch is pure
/// fixture-build setup on the real tokio runtime; the bytes then live on disk
/// and the playback path under test (`FileSrc::Local`) never touches a socket.
async fn write_sine_fixture_to_disk(
    spec: &SignalSpec,
    temp_dir: &TestTempDir,
) -> std::path::PathBuf {
    let helper = TestServerHelper::new().await;
    let url = helper.sine(spec, FREQ_HZ).await;
    let bytes = reqwest::get(url)
        .await
        .expect("fetch encoded sine fixture")
        .bytes()
        .await
        .expect("read fixture body");
    let ext = format_ext(spec.format).expect("known signal format extension");
    let path = temp_dir.path().join(format!("sine_fixture.{ext}"));
    let mut file = std::fs::File::create(&path).expect("create fixture file");
    file.write_all(&bytes).expect("write fixture bytes");
    file.sync_all().expect("flush fixture file");
    path
}

/// Socket-free twin of [`run_case`]: the fixture bytes are read from a local
/// file via `FileSrc::Local` (which builds its own private `AssetStore` and
/// never registers a `Downloader` / issues an HTTP request), so the entire
/// playback pipeline — storage waits, decode, resample — runs without any
/// loopback transport. This is the path the `flash` quiescence engine can
/// virtualize end to end. Asserts the identical phase-continuity contract.
async fn local_run_case(format: SignalFormat, backend: DecoderBackend, bit_rate: Option<u64>) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES as usize),
        format,
        bit_rate,
    };

    let temp_dir = TestTempDir::new();
    let fixture_path = write_sine_fixture_to_disk(&spec, &temp_dir).await;

    let file_config = FileConfig::for_src(FileSrc::Local(fixture_path)).build();
    let audio_config = AudioConfig::<File>::for_stream(file_config)
        .decoder_backend(backend)
        .maybe_hint(format_ext(format).map(str::to_owned))
        .build();
    let mut audio = Audio::<Stream<File>>::new(audio_config)
        .await
        .expect("create local Audio<Stream<File>>");

    let total_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("local file fixture should report duration");
    info!(?format, ?backend, total_secs, "local fixture ready");
    assert!(
        total_secs > 30.0 && total_secs < 120.0,
        "fixture duration out of sane range: {total_secs:.1}s",
    );
    let total_frames_truth = (total_secs * f64::from(SAMPLE_RATE)) as u64;

    let aspec = audio.spec();
    assert_eq!(aspec.sample_rate.get(), SAMPLE_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));

    // Keep the temp dir alive across the blocking scan: the source reads the
    // fixture from disk for the whole decode, so the backing file must outlive
    // the `Audio`.
    let drifts = spawn_blocking(move || -> Vec<PhaseDrift> {
        let sine = SinePhaseSpec::default_440();
        let drifts = e2e_phase_scan(&mut audio, sine, total_frames_truth);
        drop(audio);
        drifts
    })
    .await
    .expect("spawn_blocking joined");
    drop(temp_dir);

    assert!(
        drifts.is_empty(),
        "phase continuity broken on {} scan(s) over a local file (format={format:?} backend={backend:?}): {drifts:?}",
        drifts.len(),
    );
}

/// First flash equivalence proof on a socket-free pipeline.
///
/// Reads a sine MP3 fixture straight from disk through `FileSrc::Local` and
/// runs the production phase-continuity scan. Because `FileSrc::Local` builds
/// its own `AssetStore` and never registers a `Downloader`, there is no async
/// HTTP fetch for the virtual clock to race past — the whole pipeline collapses
/// onto the quiescence engine. Run the SAME test twice for the proof:
///
/// - default (real clock):    `cargo test … phase_continuity_file_local_socket_free`
/// - sim (virtual clock):     `cargo test … --features flash phase_continuity_file_local_socket_free`
///
/// Both must hold the sub-0.5-sample phase oracle bit-for-bit; the sim run is
/// deterministic and faster (the storage condvar / park waits collapse to zero
/// real time). The sub-0.5-sample assertions in `e2e_phase_scan` ARE the PCM
/// oracle — any virtualization-induced divergence trips them.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::mp3_symphonia(SignalFormat::Mp3, DecoderBackend::Symphonia, Some(320_000))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple(SignalFormat::Mp3, DecoderBackend::Apple, Some(320_000))
)]
async fn phase_continuity_file_local_socket_free(
    #[case] format: SignalFormat,
    #[case] backend: DecoderBackend,
    #[case] bit_rate: Option<u64>,
) {
    local_run_case(format, backend, bit_rate).await;
}

async fn decode_pcm_seconds(
    format: SignalFormat,
    backend: DecoderBackend,
    secs: f64,
    bit_rate: Option<u64>,
) -> Vec<f32> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES as usize),
        format,
        bit_rate,
    };
    let url = helper.sine(&spec, FREQ_HZ).await;
    let temp_dir = TestTempDir::new();
    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .is_ephemeral(true)
        .build();
    let file_config = FileConfig::for_src(url.into()).store(store).build();
    // Park on ring underrun instead of spinning on Pending.
    let audio_config = AudioConfig::<File>::for_stream(file_config)
        .decoder_backend(backend)
        .maybe_hint(format_ext(format).map(str::to_owned))
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<File>>::new(audio_config)
        .await
        .expect("create Audio<Stream<File>>");
    let aspec = audio.spec();
    let chan = aspec.channels as usize;
    assert_eq!(aspec.sample_rate.get(), SAMPLE_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));
    let total_frames_target = (secs * f64::from(SAMPLE_RATE)) as usize;
    spawn_blocking(move || -> Vec<f32> {
        let mut pcm: Vec<f32> = Vec::with_capacity(total_frames_target * chan);
        let mut buf = vec![0.0_f32; 4096 * chan];
        let mut got = 0usize;
        let mut pending_streak = 0usize;
        while got < total_frames_target {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    pending_streak = 0;
                    let n = count.get();
                    pcm.extend_from_slice(&buf[..n]);
                    got += n / chan;
                }
                Ok(ReadOutcome::Pending { .. }) => {
                    pending_streak += 1;
                    assert!(pending_streak < 4096, "decoder starved");
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode_pcm_seconds read error: {e}"),
            }
        }
        pcm
    })
    .await
    .expect("decode_pcm_seconds joined")
}

#[derive(Debug, Clone)]
struct CodecProfile {
    label: String,
    mean_amp: f64,
    amp_std: f64,
    phase_offset_samples: f64,
    phase_wobble_samples: f64,
    residual_snr_db: f64,
    windows: usize,
}

impl std::fmt::Display for CodecProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:<28}: amp={:.4}±{:.4} | phase_off={:+.3} samp | wobble={:.3} samp σ | SNR={:>6.1} dB | N={}",
            self.label,
            self.mean_amp,
            self.amp_std,
            self.phase_offset_samples,
            self.phase_wobble_samples,
            self.residual_snr_db,
            self.windows,
        )
    }
}

/// Decode PCM, profile encoder distortion against the theoretical sine.
/// Reports per-window LS-fit amplitude, phase wobble (std of measured
/// phase minus theoretical), and post-amp/phase-correction residual SNR
/// (how much noise the encoder injected on top of the sine).
fn profile_codec(label: &str, pcm: &[f32], chan: usize) -> CodecProfile {
    const WINDOW: usize = 128;
    const STRIDE: usize = 1024;
    profile_codec_window(label, pcm, chan, WINDOW, STRIDE)
}

fn profile_codec_window(
    label: &str,
    pcm: &[f32],
    chan: usize,
    window: usize,
    stride: usize,
) -> CodecProfile {
    let spec = SinePhaseSpec::default_440();
    let delta = spec.delta_rad_per_sample();
    let frames = pcm.len() / chan;
    assert!(frames >= window, "pcm too short: {frames} frames");
    let mut amps = Vec::new();
    let mut phase_devs_rad = Vec::new();
    let mut residuals = Vec::new();
    let mut frame = 0usize;
    while frame + window <= frames {
        let mono: Vec<f64> = (0..window)
            .map(|k| f64::from(pcm[(frame + k) * chan]))
            .collect();
        let (measured, amp) = measure_phase_rad_window(&mono, delta);
        let predicted = wrap_pi(delta * frame as f64);
        let dev = wrap_pi(measured - predicted);
        amps.push(amp);
        phase_devs_rad.push(dev);
        let mut sq = 0.0_f64;
        for (k, &s) in mono.iter().enumerate() {
            let recon = amp * (delta * (frame + k) as f64 + dev).sin();
            let r = s - recon;
            sq += r * r;
        }
        residuals.push((sq / window as f64).sqrt());
        frame += stride;
    }
    let n = amps.len() as f64;
    let mean_amp = amps.iter().sum::<f64>() / n;
    let amp_var = amps.iter().map(|a| (a - mean_amp).powi(2)).sum::<f64>() / n;
    let phase_mean = phase_devs_rad.iter().sum::<f64>() / n;
    let phase_var = phase_devs_rad
        .iter()
        .map(|p| (p - phase_mean).powi(2))
        .sum::<f64>()
        / n;
    let mean_resid = residuals.iter().sum::<f64>() / n;
    let snr = if mean_resid > 1e-12 {
        20.0 * (mean_amp / (mean_resid * std::f64::consts::SQRT_2)).log10()
    } else {
        f64::INFINITY
    };
    CodecProfile {
        label: label.to_string(),
        mean_amp,
        amp_std: amp_var.sqrt(),
        phase_offset_samples: phase_mean / delta,
        phase_wobble_samples: phase_var.sqrt() / delta,
        residual_snr_db: snr,
        windows: amps.len(),
    }
}

fn write_wav_mono_f32(path: &std::path::Path, samples: &[f32], sample_rate: u32) {
    use std::io::Write;
    let mut file = std::fs::File::create(path).expect("create wav");
    let n = samples.len() as u32;
    let byte_rate = sample_rate * 2;
    let data_bytes = n * 2;
    let total = 36 + data_bytes;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&total.to_le_bytes()).unwrap();
    file.write_all(b"WAVE").unwrap();
    file.write_all(b"fmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&sample_rate.to_le_bytes()).unwrap();
    file.write_all(&byte_rate.to_le_bytes()).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_bytes.to_le_bytes()).unwrap();
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        file.write_all(&v.to_le_bytes()).unwrap();
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[ignore = "diagnostic: writes /tmp/aac_dump/fixture_raw.{aac,m4a}; run with --run-ignored"]
async fn dump_fixture_raw_bytes() {
    use std::path::PathBuf;
    let helper = TestServerHelper::new().await;
    let dump = PathBuf::from("/tmp/aac_dump");
    std::fs::create_dir_all(&dump).expect("mkdir");
    for (fmt, name) in [
        (SignalFormat::Aac, "fixture_raw.aac"),
        (SignalFormat::M4a, "fixture_raw.m4a"),
    ] {
        let spec = SignalSpec {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            length: SignalSpecLength::Frames((SAMPLE_RATE * 5) as usize),
            format: fmt,
            bit_rate: None,
        };
        let url = helper.sine(&spec, FREQ_HZ).await;
        let bytes = reqwest::get(url)
            .await
            .expect("fetch")
            .bytes()
            .await
            .expect("body");
        std::fs::write(dump.join(name), &bytes).expect("write");
        println!(
            "{name}: {} bytes, first 16: {:02x?}",
            bytes.len(),
            &bytes[..16.min(bytes.len())]
        );
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[ignore = "diagnostic: writes /tmp/aac_dump/*.wav for offline listening; run with --run-ignored"]
async fn dump_aac_for_listening() {
    use std::path::PathBuf;
    let dump_dir = PathBuf::from("/tmp/aac_dump");
    std::fs::create_dir_all(&dump_dir).expect("mkdir");

    let chan = CHANNELS as usize;
    let secs: f64 = 5.0;
    let total = (secs * f64::from(SAMPLE_RATE)) as usize;
    let sine = SinePhaseSpec::default_440();
    let delta = sine.delta_rad_per_sample();

    let ref_mono: Vec<f32> = (0..total)
        .map(|k| (delta * k as f64).sin() as f32 * 0.95)
        .collect();
    write_wav_mono_f32(
        &dump_dir.join("01_reference_440hz.wav"),
        &ref_mono,
        SAMPLE_RATE,
    );

    for (fmt, name) in [
        (SignalFormat::M4a, "02_decoded_m4a.wav"),
        (SignalFormat::Aac, "03_decoded_aac_raw.wav"),
        (SignalFormat::Mp3, "04_decoded_mp3.wav"),
        (SignalFormat::Flac, "05_decoded_flac.wav"),
    ] {
        let pcm = decode_pcm_seconds(fmt, DecoderBackend::Symphonia, secs, None).await;
        let mono: Vec<f32> = (0..pcm.len() / chan).map(|f| pcm[f * chan]).collect();
        write_wav_mono_f32(
            &dump_dir.join(name),
            &mono[..mono.len().min(total)],
            SAMPLE_RATE,
        );

        let aligned_to = mono.len().min(total);
        let window_len = aligned_to.min(8192);
        let mono_f64: Vec<f64> = mono[..window_len].iter().map(|&v| f64::from(v)).collect();
        let (phi, amp) = measure_phase_rad_window(&mono_f64, delta);
        let residual: Vec<f32> = (0..aligned_to)
            .map(|k| {
                let ref_aligned = amp * (delta * k as f64 + phi).sin();
                mono[k] - ref_aligned as f32
            })
            .collect();
        let residual_name = format!("{}.residual.wav", name.trim_end_matches(".wav"));
        write_wav_mono_f32(&dump_dir.join(&residual_name), &residual, SAMPLE_RATE);
        let rms: f64 = (residual.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>()
            / residual.len() as f64)
            .sqrt();
        let snr_db = if rms > 1e-9 {
            20.0 * (amp / rms).log10()
        } else {
            f64::INFINITY
        };
        println!(
            "{name}: amp_fit={amp:.4} phi={phi:+.3}rad residual_rms={rms:.5} → SNR={snr_db:.1} dB",
        );
    }
    println!("\nDumped to {}", dump_dir.display());
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::aac_128k(SignalFormat::Aac, 128_000)]
#[case::aac_192k(SignalFormat::Aac, 192_000)]
#[case::aac_256k(SignalFormat::Aac, 256_000)]
#[case::aac_320k(SignalFormat::Aac, 320_000)]
#[case::m4a_128k(SignalFormat::M4a, 128_000)]
#[case::m4a_192k(SignalFormat::M4a, 192_000)]
#[case::m4a_256k(SignalFormat::M4a, 256_000)]
#[case::m4a_320k(SignalFormat::M4a, 320_000)]
#[case::mp3_128k(SignalFormat::Mp3, 128_000)]
#[case::mp3_192k(SignalFormat::Mp3, 192_000)]
#[case::mp3_256k(SignalFormat::Mp3, 256_000)]
#[case::mp3_320k(SignalFormat::Mp3, 320_000)]
async fn bit_rate_e2e_does_not_hang(#[case] format: SignalFormat, #[case] bit_rate: u64) {
    let helper = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES as usize),
        format,
        bit_rate: Some(bit_rate),
    };
    let url = helper.sine(&spec, FREQ_HZ).await;

    let temp_dir = TestTempDir::new();
    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .is_ephemeral(true)
        .build();
    let file_config = FileConfig::for_src(url.into()).store(store).build();
    let audio_config = AudioConfig::<File>::for_stream(file_config)
        .decoder_backend(DecoderBackend::Symphonia)
        .maybe_hint(format_ext(format).map(str::to_owned))
        .build();

    let audio = Audio::<Stream<File>>::new(audio_config)
        .await
        .expect("create Audio<Stream<File>>");
    let duration_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("duration should be available");
    assert!(
        duration_secs > 30.0 && duration_secs < 120.0,
        "{format:?} @ {bit_rate}: duration {duration_secs} out of expected range"
    );
}

fn codec_label(format: SignalFormat, bit_rate: Option<u64>) -> String {
    let base = match format {
        SignalFormat::Wav => "WAV (lossless ref)",
        SignalFormat::Flac => "FLAC (lossless)",
        SignalFormat::Mp3 => "MP3 (lossy)",
        SignalFormat::Aac => "AAC raw (lossy)",
        SignalFormat::M4a => "M4A AAC (lossy)",
    };
    match bit_rate {
        Some(br) => format!("{base} @ {}k", br / 1000),
        None => base.to_string(),
    }
}

async fn run_codec_compare(
    fmt_a: SignalFormat,
    fmt_b: SignalFormat,
    backend: DecoderBackend,
    bit_rate_a: Option<u64>,
    bit_rate_b: Option<u64>,
) {
    const READ_SECS: f64 = 2.0;
    let pcm_a = decode_pcm_seconds(fmt_a, backend, READ_SECS, bit_rate_a).await;
    let pcm_b = decode_pcm_seconds(fmt_b, backend, READ_SECS, bit_rate_b).await;
    let chan = CHANNELS as usize;
    let label_a = codec_label(fmt_a, bit_rate_a);
    let label_b = codec_label(fmt_b, bit_rate_b);
    println!("\n===== {backend:?} =====");
    for &(w, s) in &[
        (128usize, 1024usize),
        (256, 1024),
        (512, 1024),
        (1024, 2048),
        (2048, 4096),
    ] {
        let prof_a = profile_codec_window(&label_a, &pcm_a, chan, w, s);
        let prof_b = profile_codec_window(&label_b, &pcm_b, chan, w, s);
        println!("  window={w} stride={s}");
        println!("    {prof_a}");
        println!("    {prof_b}");
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::mp3_vs_wav_symphonia(SignalFormat::Mp3, SignalFormat::Wav, DecoderBackend::Symphonia, None)]
#[case::aac_vs_wav_symphonia(SignalFormat::Aac, SignalFormat::Wav, DecoderBackend::Symphonia, None)]
#[case::m4a_vs_wav_symphonia(SignalFormat::M4a, SignalFormat::Wav, DecoderBackend::Symphonia, None)]
#[case::flac_vs_wav_symphonia(
    SignalFormat::Flac,
    SignalFormat::Wav,
    DecoderBackend::Symphonia,
    None
)]
#[case::aac_320k_vs_wav_symphonia(
    SignalFormat::Aac,
    SignalFormat::Wav,
    DecoderBackend::Symphonia,
    Some(320_000)
)]
#[case::m4a_320k_vs_wav_symphonia(
    SignalFormat::M4a,
    SignalFormat::Wav,
    DecoderBackend::Symphonia,
    Some(320_000)
)]
#[case::mp3_320k_vs_wav_symphonia(
    SignalFormat::Mp3,
    SignalFormat::Wav,
    DecoderBackend::Symphonia,
    Some(320_000)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_vs_wav_apple(SignalFormat::Mp3, SignalFormat::Wav, DecoderBackend::Apple, None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_vs_wav_apple(SignalFormat::Aac, SignalFormat::Wav, DecoderBackend::Apple, None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_vs_wav_apple(SignalFormat::M4a, SignalFormat::Wav, DecoderBackend::Apple, None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_vs_wav_apple(SignalFormat::Flac, SignalFormat::Wav, DecoderBackend::Apple, None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_320k_vs_wav_apple(
        SignalFormat::Aac,
        SignalFormat::Wav,
        DecoderBackend::Apple,
        Some(320_000)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_320k_vs_wav_apple(
        SignalFormat::M4a,
        SignalFormat::Wav,
        DecoderBackend::Apple,
        Some(320_000)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_320k_vs_wav_apple(
        SignalFormat::Mp3,
        SignalFormat::Wav,
        DecoderBackend::Apple,
        Some(320_000)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_vs_wav_android(SignalFormat::Mp3, SignalFormat::Wav, DecoderBackend::Android, None)
)]
#[cfg_attr(
    target_os = "android",
    case::aac_vs_wav_android(SignalFormat::Aac, SignalFormat::Wav, DecoderBackend::Android, None)
)]
#[cfg_attr(
    target_os = "android",
    case::m4a_vs_wav_android(SignalFormat::M4a, SignalFormat::Wav, DecoderBackend::Android, None)
)]
#[cfg_attr(
    target_os = "android",
    case::flac_vs_wav_android(
        SignalFormat::Flac,
        SignalFormat::Wav,
        DecoderBackend::Android,
        None
    )
)]
async fn codec_distortion_profile(
    #[case] codec: SignalFormat,
    #[case] reference: SignalFormat,
    #[case] backend: DecoderBackend,
    #[case] codec_bit_rate: Option<u64>,
) {
    run_codec_compare(codec, reference, backend, codec_bit_rate, None).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::sentinel_mp3_apple_eph_e2e(
        SignalFormat::Mp3,
        DecoderBackend::Apple,
        true,
        0,
        Some(320_000)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple_eph_10seek(SignalFormat::Mp3, DecoderBackend::Apple, true, 10, Some(320_000))
)]
#[case::mp3_symphonia_eph_e2e(SignalFormat::Mp3, DecoderBackend::Symphonia, true, 0, Some(320_000))]
#[case::mp3_symphonia_eph_10seek(
    SignalFormat::Mp3,
    DecoderBackend::Symphonia,
    true,
    10,
    Some(320_000)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_apple_eph_e2e(SignalFormat::M4a, DecoderBackend::Apple, true, 0, Some(320_000))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_apple_eph_10seek(SignalFormat::M4a, DecoderBackend::Apple, true, 10, Some(320_000))
)]
#[case::m4a_symphonia_eph_e2e(SignalFormat::M4a, DecoderBackend::Symphonia, true, 0, Some(320_000))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_e2e(SignalFormat::Flac, DecoderBackend::Apple, true, 0, None)
)]
#[case::flac_symphonia_eph_e2e(SignalFormat::Flac, DecoderBackend::Symphonia, true, 0, None)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple_eph_e2e(SignalFormat::Aac, DecoderBackend::Apple, true, 0, Some(320_000))
)]
#[case::aac_symphonia_eph_e2e(SignalFormat::Aac, DecoderBackend::Symphonia, true, 0, Some(320_000))]
#[cfg_attr(
    target_os = "android",
    case::mp3_android_eph_e2e(SignalFormat::Mp3, DecoderBackend::Android, true, 0, Some(320_000))
)]
#[cfg_attr(
    target_os = "android",
    case::m4a_android_eph_e2e(SignalFormat::M4a, DecoderBackend::Android, true, 0, Some(320_000))
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android_eph_e2e(SignalFormat::Flac, DecoderBackend::Android, true, 0, None)
)]
async fn phase_continuity_file(
    #[case] format: SignalFormat,
    #[case] backend: DecoderBackend,
    #[case] ephemeral: bool,
    #[case] seek_count: usize,
    #[case] bit_rate: Option<u64>,
) {
    run_case(format, backend, ephemeral, seek_count, bit_rate).await;
}

/// Build an ephemeral AAC sine [`Audio`] over a file source. Shared by the
/// deterministic seek-to-0 warm-up repro below.
async fn build_aac_sine_audio(backend: DecoderBackend) -> Audio<Stream<File>> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES as usize),
        format: SignalFormat::Aac,
        bit_rate: Some(320_000),
    };
    let url = helper.sine(&spec, FREQ_HZ).await;
    let temp_dir = TestTempDir::new();
    // Keep the temp dir alive for the lifetime of the returned Audio by
    // leaking it: the source reads from `url` (an HTTP path) and only uses
    // the store as an ephemeral cache, so the leaked guard is harmless in
    // this short-lived test and avoids threading a guard through the caller.
    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .is_ephemeral(true)
        .build();
    std::mem::forget(temp_dir);
    let file_config = FileConfig::for_src(url.into()).store(store).build();
    // Park on ring underrun: covers both the cold and seeked handles.
    let audio_config = AudioConfig::<File>::for_stream(file_config)
        .decoder_backend(backend)
        .maybe_hint(Some("aac".to_owned()))
        .block_on_underrun(true)
        .build();
    Audio::<Stream<File>>::new(audio_config)
        .await
        .expect("create Audio<Stream<File>>")
}

/// Read forward until the first scan window whose fitted amplitude clears
/// [`MIN_SIGNAL_AMP`], returning that window's measured sine phase and the
/// absolute frame index it was consumed at. Drives the decoder offline
/// exactly like the production scan harness (`read_block` semantics).
fn first_signal_window_phase(
    audio: &mut Audio<Stream<File>>,
    chan: usize,
    delta: f64,
) -> (u64, f64) {
    let mut buf = vec![0.0_f32; READ_FRAMES_AFTER_SEEK * chan];
    let mut consumed: u64 = 0;
    let mut pending = 0usize;
    loop {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                pending = 0;
                let n = count.get();
                let frames = (n / chan) as u64;
                let mono: Vec<f64> = (0..(n / chan)).map(|f| f64::from(buf[f * chan])).collect();
                if mono.len() >= 2 {
                    let (phase, amp) = measure_phase_rad_window(&mono, delta);
                    if amp >= MIN_SIGNAL_AMP {
                        return (consumed, phase);
                    }
                }
                consumed += frames;
            }
            Ok(ReadOutcome::Pending { .. }) => {
                pending += 1;
                assert!(pending < READ_PENDING_RETRIES, "decoder starved");
            }
            Ok(ReadOutcome::Eof { .. }) => panic!("EOF before any signal window"),
            Err(e) => panic!("read error: {e}"),
        }
    }
}

/// Deterministic regression for the AAC seek-to-0 decoder warm-up drift.
///
/// The fdk-aac C decoder retains MDCT/QMF/SBR overlap-add state that
/// `AudioDecoder::reset` (called on the seek path via `codec.flush()`) does
/// not clear. So the first access unit decoded after a seek used to inherit
/// the *pre-seek* overlap tail and emit a ~2-AU phase-shifted (or
/// fully-contaminated) first chunk — non-deterministically, gated on how
/// many packets were decoded before the seek arrived. Offline the load
/// harness only caught it intermittently (`phase_continuity_hls_aac_lc_*`,
/// `jump_samples ≈ ±43.46`); this forces the exact pre-condition without
/// relying on scheduler timing:
///
/// 1. a *cold* `Audio` reads its first signal window at frame 0 — the
///    ground-truth phase the sine carries at the start of the stream;
/// 2. a *seeked* `Audio` decodes deep into the stream (so the fdk overlap
///    state is fully warmed / non-cold), seeks back to 0, then reads its
///    first signal window at frame 0.
///
/// Both windows describe the same absolute content frame, so their measured
/// phases must match within sub-sample tolerance. Before the reset rebuild
/// they diverged by ~2 access units; the assertion compares against the
/// production [`TOLERANCE_SAMPLES`] — the same contract `hls.rs:211` enforces.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::aac_symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple(DecoderBackend::Apple)
)]
async fn seek_to_zero_decoder_warmup_is_deterministic(#[case] backend: DecoderBackend) {
    let delta = SinePhaseSpec::default_440().delta_rad_per_sample();
    let chan = CHANNELS as usize;

    let mut cold = build_aac_sine_audio(backend).await;
    let mut seeked = build_aac_sine_audio(backend).await;

    let (cold_phase, seek_phase) = spawn_blocking(move || {
        // Ground truth: cold-start first signal window phase at frame 0.
        let (cold_frame, cold_phase) = first_signal_window_phase(&mut cold, chan, delta);

        // Warm the fdk overlap state well past the encoder priming, then
        // seek back to the start. ~20k frames = ~20 AAC access units, far
        // beyond the ~1024-frame priming, guaranteeing a non-cold decoder.
        let mut warm = vec![0.0_f32; 4096 * chan];
        let mut warmed = 0u64;
        let mut pending = 0usize;
        while warmed < 20_000 {
            match seeked.read(&mut warm) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    pending = 0;
                    warmed += (count.get() / chan) as u64;
                }
                Ok(ReadOutcome::Pending { .. }) => {
                    pending += 1;
                    assert!(
                        pending < READ_PENDING_RETRIES,
                        "decoder starved while warming"
                    );
                }
                Ok(ReadOutcome::Eof { .. }) => panic!("EOF while warming"),
                Err(e) => panic!("warm read error: {e}"),
            }
        }
        seeked
            .seek(Duration::ZERO)
            .expect("seek to zero must succeed");
        let (seek_frame, seek_phase) = first_signal_window_phase(&mut seeked, chan, delta);

        // The first signal window must land at the same absolute frame in
        // both paths; otherwise the phase comparison is meaningless.
        assert_eq!(
            cold_frame, seek_frame,
            "first signal window landed at different frames: cold={cold_frame} seek={seek_frame}",
        );
        (cold_phase, seek_phase)
    })
    .await
    .expect("spawn_blocking joined");

    let jump_samples = wrap_pi(seek_phase - cold_phase) / delta;
    assert!(
        jump_samples.abs() <= TOLERANCE_SAMPLES,
        "seek-to-0 decoder warm-up is non-deterministic (backend={backend:?}): \
         cold_phase={cold_phase:.4}rad seek_phase={seek_phase:.4}rad \
         jump={jump_samples:.4} samples (> {TOLERANCE_SAMPLES} tolerance)",
    );
}
