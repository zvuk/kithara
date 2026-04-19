//! Android-only diagnostic JNI entrypoint: render a track through the
//! offline firewheel backend and write the result as an IEEE-float WAV.
//!
//! Bypasses cpal / AAudio — the exact same decoder and firewheel graph
//! used in production runs, but samples go straight to disk instead of
//! to the audio device. Used to localise Android-only audio artefacts:
//! a clean WAV points at the output path (cpal/AAudio), a distorted WAV
//! points at the decoder / graph compiled for android.

// jni 0.22 deprecated `Env::get_string` in favour of `JString::mutf8_chars`
// + `JString::to_string`, but in this version the replacement method
// `JString::to_string(&self, &mut Env)` is not actually exposed — Rust's
// inherent `ToString::to_string(&self)` wins the method lookup. The
// deprecated path still works and is the only supported API here.
#![allow(deprecated)]

use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jint, jlong},
};
use kithara::{
    audio::{Audio, AudioConfig, AudioWorkerHandle},
    file::{File as FileSource, FileConfig, FileSrc},
    play::internal::{
        init_offline_backend,
        offline::{OfflinePlayer, resource_from_reader},
    },
    stream::Stream,
};
use tokio::{runtime::Builder, time::sleep};
use tracing::{error, info};

struct Consts;
impl Consts {
    // Result codes returned by `nativeRunOfflineCapture`.
    const RC_OK: jlong = 0;
    const RC_STRING_READ: jlong = 1;
    const RC_RUNTIME_BUILD: jlong = 2;
    const RC_AUDIO_BUILD: jlong = 3;
    const RC_OUTPUT_OPEN: jlong = 4;
    const RC_OUTPUT_WRITE: jlong = 5;
    const RC_HEADER_REWRITE: jlong = 6;

    // Default cpal sample format codes surfaced by `nativeProbeAndroidAudio`.
    // Mirrored in `Kithara.Test.SampleFormat` (Kotlin).
    const FMT_F32: jlong = 0;
    const FMT_I16: jlong = 1;
    const FMT_U16: jlong = 2;
    const FMT_I8: jlong = 3;
    const FMT_I32: jlong = 4;
    const FMT_I64: jlong = 5;
    const FMT_U8: jlong = 6;
    const FMT_U32: jlong = 7;
    const FMT_U64: jlong = 8;
    const FMT_F64: jlong = 9;
    const FMT_OTHER: jlong = 10;
    const FMT_ERR_NO_DEVICE: jlong = -1;
    const FMT_ERR_DEFAULT_CFG: jlong = -2;
    const FMT_ERR_SUPPORTED_CFGS: jlong = -3;

    // WAV output parameters for `nativeRunOfflineCapture`.
    const SAMPLE_RATE: u32 = 44_100;
    const BLOCK_FRAMES: usize = 512;
    const CHANNELS: u16 = 2;
    const BITS_PER_SAMPLE: u16 = 32;
    const WAV_FMT_CHUNK_SIZE: u32 = 16;
    const WAV_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAV_HEADER_BYTES: u32 = 36;
}

/// Render `seconds` of audio from `inputPath` through the offline backend
/// and write the interleaved stereo f32 stream to `outputPath` as WAV.
/// Returns `0` on success; non-zero error codes mirror the `RC_*` constants.
#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeRunOfflineCapture<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    input: JString<'local>,
    output: JString<'local>,
    seconds: jint,
) -> jlong {
    let mut input_path = String::new();
    let mut output_path = String::new();
    let mut string_rc: jlong = Consts::RC_OK;

    let _ = env.with_env_no_catch(|env| -> Result<(), jni::errors::Error> {
        match env.get_string(&input) {
            Ok(s) => input_path = s.to_string(),
            Err(err) => {
                error!(?err, "failed to read input jstring");
                string_rc = Consts::RC_STRING_READ;
                return Ok(());
            }
        }
        match env.get_string(&output) {
            Ok(s) => output_path = s.to_string(),
            Err(err) => {
                error!(?err, "failed to read output jstring");
                string_rc = Consts::RC_STRING_READ;
            }
        }
        Ok(())
    });

    if string_rc != Consts::RC_OK {
        return string_rc;
    }

    let runtime = match Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            error!(?err, "failed to build tokio runtime");
            return Consts::RC_RUNTIME_BUILD;
        }
    };

    let clamped_seconds = usize::try_from(seconds.max(1)).unwrap_or(1);
    runtime.block_on(run_capture(
        PathBuf::from(input_path),
        PathBuf::from(output_path),
        clamped_seconds,
    ))
}

async fn run_capture(input: PathBuf, output: PathBuf, seconds: usize) -> jlong {
    init_offline_backend();

    info!(
        input = %input.display(),
        output = %output.display(),
        seconds,
        "offline capture: start"
    );

    let file_cfg = FileConfig::new(FileSrc::Local(input));
    let worker = AudioWorkerHandle::new();
    let audio_cfg = AudioConfig::<FileSource>::new(file_cfg)
        .with_hint("mp3")
        .with_worker(worker);

    let mut audio = match Audio::<Stream<FileSource>>::new(audio_cfg).await {
        Ok(a) => a,
        Err(err) => {
            error!(?err, "Audio::new failed");
            return Consts::RC_AUDIO_BUILD;
        }
    };

    audio.preload();

    let resource = resource_from_reader(audio);
    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "android-offline");

    let mut file = match File::create(&output) {
        Ok(f) => f,
        Err(err) => {
            error!(?err, path = %output.display(), "output open failed");
            return Consts::RC_OUTPUT_OPEN;
        }
    };

    if let Err(err) = write_wav_header(&mut file, 0) {
        error!(?err, "placeholder header write failed");
        return Consts::RC_OUTPUT_WRITE;
    }

    let target_frames = seconds.saturating_mul(Consts::SAMPLE_RATE as usize);
    let block_budget = Duration::from_secs_f64(
        f64::from(Consts::BLOCK_FRAMES as u32) / f64::from(Consts::SAMPLE_RATE),
    );
    let mut rendered_frames = 0usize;
    let mut buf = Vec::with_capacity(Consts::BLOCK_FRAMES * usize::from(Consts::CHANNELS) * 4);
    while rendered_frames < target_frames {
        let frames = (target_frames - rendered_frames).min(Consts::BLOCK_FRAMES);
        let tick = Instant::now();
        let block: Vec<f32> = player.render(frames);
        buf.clear();
        for sample in block {
            let bytes: [u8; 4] = f32::to_le_bytes(sample);
            buf.extend_from_slice(&bytes);
        }
        if let Err(err) = file.write_all(&buf) {
            error!(?err, "sample write failed");
            return Consts::RC_OUTPUT_WRITE;
        }
        rendered_frames += frames;
        // Throttle to real-time block budget so the decoder worker
        // (separate thread) keeps the ring buffer filled. Without this,
        // render outruns the worker and the output has silence gaps
        // which read as breakups in the captured WAV.
        if let Some(remaining) = block_budget.checked_sub(tick.elapsed()) {
            sleep(remaining).await;
        }
    }

    if let Err(err) = file.flush() {
        error!(?err, "flush failed");
        return Consts::RC_OUTPUT_WRITE;
    }

    let total_samples = rendered_frames.saturating_mul(usize::from(Consts::CHANNELS));
    if let Err(err) = write_wav_header(&mut file, total_samples) {
        error!(?err, "header rewrite failed");
        return Consts::RC_HEADER_REWRITE;
    }

    info!(
        frames = rendered_frames,
        bytes = total_samples * (usize::from(Consts::BITS_PER_SAMPLE) / 8),
        "offline capture: done"
    );
    Consts::RC_OK
}

/// Enumerate the cpal default host / output device and log every supported
/// output config via `tracing`. Returns a `FMT_*` code for the **default**
/// output sample format so the Kotlin side can assert against the expected
/// f32 contract the firewheel graph produces.
#[expect(unreachable_pub, reason = "JNI entrypoint must remain exported")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kithara_Kithara_nativeProbeAndroidAudio<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jlong {
    use cpal::{
        SampleFormat,
        traits::{DeviceTrait, HostTrait},
    };

    let host = cpal::default_host();
    info!(host = %host.id().name(), "cpal host");

    let Some(device) = host.default_output_device() else {
        error!("cpal: no default output device");
        return Consts::FMT_ERR_NO_DEVICE;
    };

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_owned());
    info!(device = %device_name, "cpal default output device");

    let default_cfg = match device.default_output_config() {
        Ok(cfg) => cfg,
        Err(err) => {
            error!(?err, "cpal: default_output_config failed");
            return Consts::FMT_ERR_DEFAULT_CFG;
        }
    };

    let default_fmt = default_cfg.sample_format();
    let default_channels = default_cfg.channels();
    let default_rate = default_cfg.sample_rate();
    let default_buffer = format!("{:?}", default_cfg.buffer_size());
    info!(
        sample_format = ?default_fmt,
        channels = default_channels,
        sample_rate = default_rate,
        buffer_size = %default_buffer,
        "cpal default output config"
    );

    match device.supported_output_configs() {
        Ok(configs) => {
            for (idx, cfg) in configs.enumerate() {
                info!(
                    idx,
                    format = ?cfg.sample_format(),
                    channels = cfg.channels(),
                    min_rate = cfg.min_sample_rate(),
                    max_rate = cfg.max_sample_rate(),
                    buffer_size = ?cfg.buffer_size(),
                    "cpal supported output config"
                );
            }
        }
        Err(err) => {
            error!(?err, "cpal: supported_output_configs failed");
            return Consts::FMT_ERR_SUPPORTED_CFGS;
        }
    };

    match default_fmt {
        SampleFormat::F32 => Consts::FMT_F32,
        SampleFormat::I16 => Consts::FMT_I16,
        SampleFormat::U16 => Consts::FMT_U16,
        SampleFormat::I8 => Consts::FMT_I8,
        SampleFormat::I32 => Consts::FMT_I32,
        SampleFormat::I64 => Consts::FMT_I64,
        SampleFormat::U8 => Consts::FMT_U8,
        SampleFormat::U32 => Consts::FMT_U32,
        SampleFormat::U64 => Consts::FMT_U64,
        SampleFormat::F64 => Consts::FMT_F64,
        _ => Consts::FMT_OTHER,
    }
}

fn write_wav_header(file: &mut File, total_samples: usize) -> std::io::Result<()> {
    let bytes_per_sample = u32::from(Consts::BITS_PER_SAMPLE) / 8;
    let channels = u32::from(Consts::CHANNELS);
    let data_size = u32::try_from(total_samples)
        .unwrap_or(u32::MAX)
        .saturating_mul(bytes_per_sample);
    let riff_size = Consts::WAV_HEADER_BYTES.saturating_add(data_size);
    let byte_rate = Consts::SAMPLE_RATE
        .saturating_mul(channels)
        .saturating_mul(bytes_per_sample);
    let block_align = (Consts::CHANNELS * Consts::BITS_PER_SAMPLE) / 8;

    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;

    file.write_all(b"fmt ")?;
    file.write_all(&Consts::WAV_FMT_CHUNK_SIZE.to_le_bytes())?;
    file.write_all(&Consts::WAV_FORMAT_IEEE_FLOAT.to_le_bytes())?;
    file.write_all(&Consts::CHANNELS.to_le_bytes())?;
    file.write_all(&Consts::SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&Consts::BITS_PER_SAMPLE.to_le_bytes())?;

    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    Ok(())
}
