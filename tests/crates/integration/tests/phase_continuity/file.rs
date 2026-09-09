use std::{num::NonZeroUsize, path::PathBuf};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    decode::{DecodeResult, DecoderBackend},
    file::{File, FileConfig, FileSrc},
    platform::{time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir,
    bufpool_ext::{Pools, TestPools, pools},
};
use kithara_test_fixtures::{
    SignalAsset, assets::by_name, integration_fixtures::listening_reference,
};
use tracing::info;
use url::Url;

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
use super::common::FREQ_HZ;
use super::common::{
    CHANNELS, MIN_SIGNAL_AMP, PhaseDrift, READ_FRAMES_AFTER_SEEK, READ_PENDING_RETRIES,
    SAMPLE_RATE, SinePhaseSpec, TOLERANCE_SAMPLES, e2e_phase_scan, measure_phase_rad_window,
    seek_phase_scan, wrap_pi,
};

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
const APPLE_FUSED_HOST_RATE: u32 = 48_000;

type ServedSignal = (SignalAsset, TestServerHelper, Url);

async fn served_signal(asset: SignalAsset) -> ServedSignal {
    let helper = TestServerHelper::new().await;
    let url = helper.signal(asset);
    (asset, helper, url)
}

fn local_signal(asset: SignalAsset) -> (SignalAsset, TestTempDir, PathBuf) {
    let temp_dir = TestTempDir::new();
    let path = temp_dir
        .path()
        .join(format!("sine_fixture.{}", asset.ext()));
    let bytes = by_name(asset.name())
        .expect("registered sine fixture")
        .bytes();
    std::fs::write(&path, bytes).expect("write sine fixture");
    (asset, temp_dir, path)
}

async fn open_audio(
    config: AudioConfig<File<TestPools>>,
    pools: &Pools,
) -> DecodeResult<RegisteredAudio<Stream<File<TestPools>>, TestPools>> {
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    worker.open(config).await
}

async fn run_case(
    source: ServedSignal,
    backend: DecoderBackend,
    ephemeral: bool,
    seek_count: usize,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (asset, _helper, url) = source;

    let temp_dir = TestTempDir::new();
    let pools = pools();
    let store = if ephemeral {
        AssetStore::builder(pools.clone())
            .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
            .backend(StorageBackend::Memory)
            .build()
    } else {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().into(),
            })
            .build()
    };

    let file_config = FileConfig::for_src(url.into())
        .store(store)
        .pools(pools.clone())
        .build();
    // Park on ring underrun: the offline scan needs no wall-clock pacing.
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_hint(Some(asset.ext().to_owned()))
        .block_on_underrun(true)
        .build();
    let mut audio = open_audio(audio_config, &pools)
        .await
        .expect("create Audio<Stream<File>>");

    let total_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("file fixture should report duration");
    info!(
        asset = asset.name(),
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
    let total_frames_truth =
        num_traits::cast::<f64, u64>(total_secs * f64::from(SAMPLE_RATE)).unwrap_or(u64::MAX);

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
        "phase continuity broken on {} scan(s) (asset={} backend={backend:?} seek_count={seek_count}): {drifts:?}",
        drifts.len(),
        asset.name(),
    );
}

/// Socket-free twin of [`run_case`]: the fixture bytes are read from a local
/// file via `FileSrc::Local` (which builds its own private `AssetStore` and
/// never registers a `Downloader` / issues an HTTP request), so the entire
/// playback pipeline — storage waits, decode, resample — runs without any
/// loopback transport. This is the path the `flash` quiescence engine can
/// virtualize end to end. Asserts the identical phase-continuity contract.
async fn local_run_case(source: (SignalAsset, TestTempDir, PathBuf), backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (asset, temp_dir, fixture_path) = source;
    let pools = pools();

    let file_config = FileConfig::for_src(FileSrc::Local(fixture_path))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_hint(Some(asset.ext().to_owned()))
        .build();
    let mut audio = open_audio(audio_config, &pools)
        .await
        .expect("create local Audio<Stream<File>>");

    let total_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("local file fixture should report duration");
    info!(
        asset = asset.name(),
        ?backend,
        total_secs,
        "local fixture ready"
    );
    assert!(
        total_secs > 30.0 && total_secs < 120.0,
        "fixture duration out of sane range: {total_secs:.1}s",
    );
    let total_frames_truth =
        num_traits::cast::<f64, u64>(total_secs * f64::from(SAMPLE_RATE)).unwrap_or(u64::MAX);

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
        "phase continuity broken on {} scan(s) over a local file (asset={} backend={backend:?}): {drifts:?}",
        drifts.len(),
        asset.name(),
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
async fn local_apple_fused_run_case(source: (SignalAsset, TestTempDir, PathBuf)) {
    kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);

    let (_asset, temp_dir, fixture_path) = source;
    let pools = pools();

    let file_config = FileConfig::for_src(FileSrc::Local(fixture_path))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .build(),
        )
        .host_sample_rate(
            std::num::NonZeroU32::new(APPLE_FUSED_HOST_RATE).expect("nonzero host rate"),
        )
        .maybe_hint(Some("m4a".to_owned()))
        .build();
    let mut audio = open_audio(audio_config, &pools)
        .await
        .expect("create fused local Audio<Stream<File>>");

    let total_duration = audio
        .duration()
        .expect("local fused fixture should report duration");
    let total_secs = total_duration.as_secs_f64();
    info!(
        source_rate = SAMPLE_RATE,
        host_rate = APPLE_FUSED_HOST_RATE,
        total_secs,
        "Apple fused local fixture ready"
    );

    let aspec = audio.spec();
    assert_eq!(aspec.sample_rate.get(), APPLE_FUSED_HOST_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));

    let total_frames_truth = frames_for_duration_rounded(APPLE_FUSED_HOST_RATE, total_duration);
    let sine = SinePhaseSpec {
        freq_hz: FREQ_HZ,
        sample_rate: APPLE_FUSED_HOST_RATE,
        channels: CHANNELS,
    };

    let drifts = spawn_blocking(move || -> Vec<PhaseDrift> {
        let drifts = e2e_phase_scan(&mut audio, sine, total_frames_truth);
        drop(audio);
        drifts
    })
    .await
    .expect("fused local scan joined");
    drop(temp_dir);

    assert!(
        drifts.is_empty(),
        "phase continuity broken on Apple fused local file scan: {drifts:?}",
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
fn frames_for_duration_rounded(sample_rate: u32, duration: Duration) -> u64 {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(sample_rate))
        .saturating_add(500_000_000)
        .saturating_div(1_000_000_000);
    u64::try_from(frames).unwrap_or(u64::MAX)
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
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[case::mp3_symphonia(local_mp3_sine440_60_s_320_k(), DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple(local_mp3_sine440_60_s_320_k(), DecoderBackend::Apple)
)]
async fn phase_continuity_file_local_socket_free(
    #[case] asset: (SignalAsset, TestTempDir, PathBuf),
    #[case] backend: DecoderBackend,
) {
    local_run_case(asset, backend).await;
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn phase_continuity_file_apple_fused_src_44k_to_host_48k(
    local_m4a: (SignalAsset, TestTempDir, PathBuf),
) {
    local_apple_fused_run_case(local_m4a).await;
}

async fn decode_pcm_seconds(source: ServedSignal, backend: DecoderBackend, secs: f64) -> Vec<f32> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (asset, _helper, url) = source;
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .backend(StorageBackend::Memory)
        .build();
    let file_config = FileConfig::for_src(url.into())
        .store(store)
        .pools(pools.clone())
        .build();
    // Park on ring underrun instead of spinning on Pending.
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_hint(Some(asset.ext().to_owned()))
        .block_on_underrun(true)
        .build();
    let mut audio = open_audio(audio_config, &pools)
        .await
        .expect("create Audio<Stream<File>>");
    let aspec = audio.spec();
    let chan = aspec.channels as usize;
    assert_eq!(aspec.sample_rate.get(), SAMPLE_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));
    let total_frames_target =
        num_traits::cast::<f64, usize>(secs * f64::from(SAMPLE_RATE)).unwrap_or(usize::MAX);
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
            let recon = amp * delta.mul_add((frame + k) as f64, dev).sin();
            let r = s - recon;
            sq = r.mul_add(r, sq);
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
    let n = u32::try_from(samples.len()).unwrap_or(u32::MAX);
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
        let v = num_traits::cast::<f32, i16>(s.clamp(-1.0, 1.0) * 32767.0).unwrap_or(0);
        file.write_all(&v.to_le_bytes()).unwrap();
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[ignore = "diagnostic: writes /tmp/aac_dump/fixture_raw.{aac,m4a}; run with --run-ignored"]
async fn dump_fixture_raw_bytes(raw_signals: [(SignalAsset, &'static [u8]); 2]) {
    let dump = PathBuf::from("/tmp/aac_dump");
    std::fs::create_dir_all(&dump).expect("mkdir");
    for (asset, bytes) in raw_signals {
        let name = format!("fixture_raw.{}", asset.ext());
        std::fs::write(dump.join(&name), bytes).expect("write");
        println!(
            "{name}: {} bytes, first 16: {:02x?}",
            bytes.len(),
            &bytes[..16.min(bytes.len())]
        );
    }
}

#[kithara::fixture]
fn raw_signals() -> [(SignalAsset, &'static [u8]); 2] {
    [SignalAsset::AAC_SINE440_60S, SignalAsset::M4A_SINE440_60S].map(|asset| {
        (
            asset,
            by_name(asset.name()).expect("registered fixture").bytes(),
        )
    })
}

#[kithara::fixture]
async fn listening_sources() -> [ServedSignal; 4] {
    [
        served_signal(SignalAsset::M4A_SINE440_60S).await,
        served_signal(SignalAsset::AAC_SINE440_60S).await,
        served_signal(SignalAsset::MP3_SINE440_60S).await,
        served_signal(SignalAsset::FLAC_SINE440_60S).await,
    ]
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[ignore = "diagnostic: writes /tmp/aac_dump/*.wav for offline listening; run with --run-ignored"]
async fn dump_aac_for_listening(
    listening_reference: Vec<f32>,
    #[future(awt)] listening_sources: [ServedSignal; 4],
) {
    let dump_dir = PathBuf::from("/tmp/aac_dump");
    std::fs::create_dir_all(&dump_dir).expect("mkdir");

    let chan = CHANNELS as usize;
    let secs: f64 = 5.0;
    let total = num_traits::cast::<f64, usize>(secs * f64::from(SAMPLE_RATE)).unwrap_or(usize::MAX);
    let sine = SinePhaseSpec::default_440();
    let delta = sine.delta_rad_per_sample();

    let ref_mono = listening_reference;
    write_wav_mono_f32(
        &dump_dir.join("01_reference_440hz.wav"),
        &ref_mono,
        SAMPLE_RATE,
    );

    for (asset, name) in listening_sources.into_iter().zip([
        "02_decoded_m4a.wav",
        "03_decoded_aac_raw.wav",
        "04_decoded_mp3.wav",
        "05_decoded_flac.wav",
    ]) {
        let pcm = decode_pcm_seconds(asset, DecoderBackend::Symphonia, secs).await;
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
                let ref_aligned = amp * delta.mul_add(k as f64, phi).sin();
                mono[k] - num_traits::cast::<f64, f32>(ref_aligned).unwrap_or(0.0)
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
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[case::aac_128k(signal_aac_sine440_60_s_128_k().await)]
#[case::aac_192k(signal_aac_sine440_60_s_192_k().await)]
#[case::aac_256k(signal_aac_sine440_60_s_256_k().await)]
#[case::aac_320k(signal_aac_sine440_60_s_320_k().await)]
#[case::m4a_128k(signal_m4_a_sine440_60_s_128_k().await)]
#[case::m4a_192k(signal_m4_a_sine440_60_s_192_k().await)]
#[case::m4a_256k(signal_m4_a_sine440_60_s_256_k().await)]
#[case::m4a_320k(signal_m4_a_sine440_60_s_320_k().await)]
#[case::mp3_128k(signal_mp3_sine440_60_s_128_k().await)]
#[case::mp3_192k(signal_mp3_sine440_60_s_192_k().await)]
#[case::mp3_256k(signal_mp3_sine440_60_s_256_k().await)]
#[case::mp3_320k(signal_mp3_sine440_60_s_320_k().await)]
async fn bit_rate_e2e_does_not_hang(#[case] source: ServedSignal) {
    let (asset, _helper, url) = source;

    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .backend(StorageBackend::Memory)
        .build();
    let file_config = FileConfig::for_src(url.into())
        .store(store)
        .pools(pools.clone())
        .build();
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .maybe_hint(Some(asset.ext().to_owned()))
        .build();

    let audio = open_audio(audio_config, &pools)
        .await
        .expect("create Audio<Stream<File>>");
    let duration_secs = audio
        .duration()
        .map(|d| d.as_secs_f64())
        .expect("duration should be available");
    assert!(
        duration_secs > 30.0 && duration_secs < 120.0,
        "{}: duration {duration_secs} out of expected range",
        asset.name()
    );
}

fn codec_label(asset: SignalAsset) -> String {
    let kind = match asset.ext() {
        "wav" => "lossless ref",
        "flac" => "lossless",
        _ => "lossy",
    };
    format!("{} ({kind})", asset.name())
}

async fn run_codec_compare(asset_a: ServedSignal, asset_b: ServedSignal, backend: DecoderBackend) {
    const READ_SECS: f64 = 2.0;
    let label_a = codec_label(asset_a.0);
    let label_b = codec_label(asset_b.0);
    let pcm_a = decode_pcm_seconds(asset_a, backend, READ_SECS).await;
    let pcm_b = decode_pcm_seconds(asset_b, backend, READ_SECS).await;
    let chan = CHANNELS as usize;
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
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[case::mp3_vs_wav_symphonia(
    signal_mp3_sine440_60_s().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::aac_vs_wav_symphonia(
    signal_aac_sine440_60_s().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::m4a_vs_wav_symphonia(
    signal_m4_a_sine440_60_s().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::flac_vs_wav_symphonia(
    signal_flac_sine440_60_s().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::aac_320k_vs_wav_symphonia(
    signal_aac_sine440_60_s_320_k().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::m4a_320k_vs_wav_symphonia(
    signal_m4_a_sine440_60_s_320_k().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[case::mp3_320k_vs_wav_symphonia(
    signal_mp3_sine440_60_s_320_k().await,
    signal_wav_sine440_60_s().await,
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_vs_wav_apple(
        signal_mp3_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_vs_wav_apple(
        signal_aac_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_vs_wav_apple(
        signal_m4_a_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_vs_wav_apple(
        signal_flac_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_320k_vs_wav_apple(
        signal_aac_sine440_60_s_320_k().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_320k_vs_wav_apple(
        signal_m4_a_sine440_60_s_320_k().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_320k_vs_wav_apple(
        signal_mp3_sine440_60_s_320_k().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_vs_wav_android(
        signal_mp3_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Android
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_vs_wav_android(
        signal_aac_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Android
    )
)]
#[cfg_attr(
    target_os = "android",
    case::m4a_vs_wav_android(
        signal_m4_a_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Android
    )
)]
#[cfg_attr(
    target_os = "android",
    case::flac_vs_wav_android(
        signal_flac_sine440_60_s().await,
        signal_wav_sine440_60_s().await,
        DecoderBackend::Android
    )
)]
async fn codec_distortion_profile(
    #[case] codec: ServedSignal,
    #[case] reference: ServedSignal,
    #[case] backend: DecoderBackend,
) {
    run_codec_compare(codec, reference, backend).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::sentinel_mp3_apple_eph_e2e(
        signal_mp3_sine440_60_s_320_k().await,
        DecoderBackend::Apple,
        true,
        0
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple_eph_10seek(signal_mp3_sine440_60_s_320_k().await, DecoderBackend::Apple, true, 10)
)]
#[case::mp3_symphonia_eph_e2e(
    signal_mp3_sine440_60_s_320_k().await,
    DecoderBackend::Symphonia,
    true,
    0
)]
#[case::mp3_symphonia_eph_10seek(
    signal_mp3_sine440_60_s_320_k().await,
    DecoderBackend::Symphonia,
    true,
    10
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_apple_eph_e2e(signal_m4_a_sine440_60_s_320_k().await, DecoderBackend::Apple, true, 0)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::m4a_apple_eph_10seek(signal_m4_a_sine440_60_s_320_k().await, DecoderBackend::Apple, true, 10)
)]
#[case::m4a_symphonia_eph_e2e(
    signal_m4_a_sine440_60_s_320_k().await,
    DecoderBackend::Symphonia,
    true,
    0
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_e2e(signal_flac_sine440_60_s().await, DecoderBackend::Apple, true, 0)
)]
#[case::flac_symphonia_eph_e2e(signal_flac_sine440_60_s().await, DecoderBackend::Symphonia, true, 0)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple_eph_e2e(signal_aac_sine440_60_s_320_k().await, DecoderBackend::Apple, true, 0)
)]
#[case::aac_symphonia_eph_e2e(
    signal_aac_sine440_60_s_320_k().await,
    DecoderBackend::Symphonia,
    true,
    0
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_android_eph_e2e(signal_mp3_sine440_60_s_320_k().await, DecoderBackend::Android, true, 0)
)]
#[cfg_attr(
    target_os = "android",
    case::m4a_android_eph_e2e(signal_m4_a_sine440_60_s_320_k().await, DecoderBackend::Android, true, 0)
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android_eph_e2e(signal_flac_sine440_60_s().await, DecoderBackend::Android, true, 0)
)]
async fn phase_continuity_file(
    #[case] asset: ServedSignal,
    #[case] backend: DecoderBackend,
    #[case] ephemeral: bool,
    #[case] seek_count: usize,
) {
    run_case(asset, backend, ephemeral, seek_count).await;
}

/// Build an ephemeral AAC sine [`RegisteredAudio`] over a file source. Shared by the
/// deterministic seek-to-0 warm-up repro below.
async fn build_aac_sine_audio(
    backend: DecoderBackend,
    url: Url,
) -> RegisteredAudio<Stream<File<TestPools>>, TestPools> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .backend(StorageBackend::Memory)
        .build();
    let file_config = FileConfig::for_src(url.into())
        .store(store)
        .pools(pools.clone())
        .build();
    // Park on ring underrun: covers both the cold and seeked handles.
    let audio_config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_hint(Some("aac".to_owned()))
        .block_on_underrun(true)
        .build();
    open_audio(audio_config, &pools)
        .await
        .expect("create Audio<Stream<File>>")
}

/// Read forward until the first scan window whose fitted amplitude clears
/// [`MIN_SIGNAL_AMP`], returning that window's measured sine phase and the
/// absolute frame index it was consumed at. Drives the decoder offline
/// exactly like the production scan harness (`read_block` semantics).
fn first_signal_window_phase(
    audio: &mut RegisteredAudio<Stream<File<TestPools>>, TestPools>,
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
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
#[case::aac_symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple(DecoderBackend::Apple)
)]
async fn seek_to_zero_decoder_warmup_is_deterministic(
    #[case] backend: DecoderBackend,
    #[future(awt)] served_aac: ServedSignal,
) {
    let delta = SinePhaseSpec::default_440().delta_rad_per_sample();
    let chan = CHANNELS as usize;

    let (_asset, _server, url) = served_aac;
    let mut cold = build_aac_sine_audio(backend, url.clone()).await;
    let mut seeked = build_aac_sine_audio(backend, url).await;

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

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::fixture]
fn local_m4a() -> (SignalAsset, TestTempDir, PathBuf) {
    local_signal(SignalAsset::M4A_SINE440_60S_320K)
}

#[kithara::fixture]
async fn served_aac() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S_320K).await
}

#[kithara::fixture]
fn local_mp3_sine440_60_s_320_k() -> (SignalAsset, TestTempDir, PathBuf) {
    local_signal(SignalAsset::MP3_SINE440_60S_320K)
}

#[kithara::fixture]
async fn signal_aac_sine440_60_s_128_k() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S_128K).await
}

#[kithara::fixture]
async fn signal_aac_sine440_60_s_192_k() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S_192K).await
}

#[kithara::fixture]
async fn signal_aac_sine440_60_s_256_k() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S_256K).await
}

#[kithara::fixture]
async fn signal_aac_sine440_60_s_320_k() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S_320K).await
}

#[kithara::fixture]
async fn signal_m4_a_sine440_60_s_128_k() -> ServedSignal {
    served_signal(SignalAsset::M4A_SINE440_60S_128K).await
}

#[kithara::fixture]
async fn signal_m4_a_sine440_60_s_192_k() -> ServedSignal {
    served_signal(SignalAsset::M4A_SINE440_60S_192K).await
}

#[kithara::fixture]
async fn signal_m4_a_sine440_60_s_256_k() -> ServedSignal {
    served_signal(SignalAsset::M4A_SINE440_60S_256K).await
}

#[kithara::fixture]
async fn signal_m4_a_sine440_60_s_320_k() -> ServedSignal {
    served_signal(SignalAsset::M4A_SINE440_60S_320K).await
}

#[kithara::fixture]
async fn signal_mp3_sine440_60_s_128_k() -> ServedSignal {
    served_signal(SignalAsset::MP3_SINE440_60S_128K).await
}

#[kithara::fixture]
async fn signal_mp3_sine440_60_s_192_k() -> ServedSignal {
    served_signal(SignalAsset::MP3_SINE440_60S_192K).await
}

#[kithara::fixture]
async fn signal_mp3_sine440_60_s_256_k() -> ServedSignal {
    served_signal(SignalAsset::MP3_SINE440_60S_256K).await
}

#[kithara::fixture]
async fn signal_mp3_sine440_60_s_320_k() -> ServedSignal {
    served_signal(SignalAsset::MP3_SINE440_60S_320K).await
}

#[kithara::fixture]
async fn signal_mp3_sine440_60_s() -> ServedSignal {
    served_signal(SignalAsset::MP3_SINE440_60S).await
}

#[kithara::fixture]
async fn signal_wav_sine440_60_s() -> ServedSignal {
    served_signal(SignalAsset::WAV_SINE440_60S).await
}

#[kithara::fixture]
async fn signal_aac_sine440_60_s() -> ServedSignal {
    served_signal(SignalAsset::AAC_SINE440_60S).await
}

#[kithara::fixture]
async fn signal_m4_a_sine440_60_s() -> ServedSignal {
    served_signal(SignalAsset::M4A_SINE440_60S).await
}

#[kithara::fixture]
async fn signal_flac_sine440_60_s() -> ServedSignal {
    served_signal(SignalAsset::FLAC_SINE440_60S).await
}
