#![allow(deprecated)]

use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    num::NonZeroU32,
    path::PathBuf,
};

use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jint, jlong},
};
use kithara::{
    assets::StorageBackend,
    events::TrackId,
    host::HostConfig,
    output::{
        OfflineRenderError, OfflineRenderRequest, OfflineRenderer, RenderSink, RenderSinkError,
    },
    platform::{
        CancelScope,
        tokio::runtime::{Builder, Runtime},
    },
    play::{PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource, ResourceSrc},
    signal::AudioSpec,
};
use tracing::{error, info};

use crate::pools::{FfiHost, FfiResourceConfig, FfiStore, FfiWorker, build as build_pools};

struct Consts;
impl Consts {
    const BITS_PER_SAMPLE: u16 = 32;
    const CHANNELS: u16 = 2;
    const FMT_ERR_DEFAULT_CFG: jlong = -2;
    const FMT_ERR_NO_DEVICE: jlong = -1;
    const FMT_ERR_SUPPORTED_CFGS: jlong = -3;
    const FMT_F32: jlong = 0;

    const FMT_F64: jlong = 9;
    const FMT_I16: jlong = 1;
    const FMT_I32: jlong = 4;
    const FMT_I64: jlong = 5;
    const FMT_I8: jlong = 3;
    const FMT_OTHER: jlong = 10;
    const FMT_U16: jlong = 2;
    const FMT_U32: jlong = 7;
    const FMT_U64: jlong = 8;
    const FMT_U8: jlong = 6;
    const RC_AUDIO_BUILD: jlong = 3;
    const RC_HEADER_REWRITE: jlong = 6;
    const RC_OK: jlong = 0;
    const RC_OUTPUT_OPEN: jlong = 4;

    const RC_OUTPUT_WRITE: jlong = 5;
    const RC_RUNTIME_BUILD: jlong = 2;
    const RC_STRING_READ: jlong = 1;
    const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
        Some(rate) => rate,
        None => unreachable!(),
    };
    const WAV_FMT_CHUNK_SIZE: u32 = 16;
    const WAV_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAV_HEADER_BYTES: u32 = 36;
}

/// Render through an independent offline Host into a stereo float WAV.
/// Returns zero on success or a `RC_*` error code.
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

    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            error!(?err, "failed to build tokio runtime");
            return Consts::RC_RUNTIME_BUILD;
        }
    };

    match run_capture(
        &runtime,
        PathBuf::from(input_path),
        PathBuf::from(output_path),
        seconds.max(1).unsigned_abs(),
    ) {
        Ok(()) => Consts::RC_OK,
        Err(code) => code,
    }
}

fn run_capture(
    runtime: &Runtime,
    input: PathBuf,
    output: PathBuf,
    seconds: u32,
) -> Result<(), jlong> {
    info!(
        input = %input.display(),
        output = %output.display(),
        seconds,
        "offline capture: start"
    );

    let pools = build_pools().map_err(|err| {
        error!(?err, "buffer-pool initialization failed");
        Consts::RC_AUDIO_BUILD
    })?;
    let store = FfiStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let worker = FfiWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut host = FfiHost::new(
        HostConfig::offline(pools)
            .sample_rate(Consts::SAMPLE_RATE)
            .build(),
    )
    .map_err(|err| {
        error!(?err, "offline Host initialization failed");
        Consts::RC_AUDIO_BUILD
    })?;
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Consts::SAMPLE_RATE)
            .worker(worker)
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
    );
    let member = host.insert(player).map_err(|err| {
        error!(?err, "offline player insertion failed");
        Consts::RC_AUDIO_BUILD
    })?;
    let control = member.control();
    let config: FfiResourceConfig = control
        .prepare_config(
            FfiResourceConfig::for_src(ResourceSrc::Path(input))
                .store(store)
                .build(),
        )
        .map_err(|err| {
            error!(?err, "offline resource preparation failed");
            Consts::RC_AUDIO_BUILD
        })?;
    let resource = runtime.block_on(async {
        let mut resource = Resource::new(config).await.map_err(|err| {
            error!(?err, "offline resource open failed");
            Consts::RC_AUDIO_BUILD
        })?;
        resource.preload().await.map_err(|err| {
            error!(?err, "offline resource preload failed");
            Consts::RC_AUDIO_BUILD
        })?;
        Ok::<_, jlong>(resource)
    })?;
    control.insert(resource, TrackId::allocate(), None);
    control.select_item(0, true).map_err(|err| {
        error!(?err, "offline player selection failed");
        Consts::RC_AUDIO_BUILD
    })?;

    let mut file = File::create(&output).map_err(|err| {
        error!(?err, path = %output.display(), "output open failed");
        Consts::RC_OUTPUT_OPEN
    })?;
    write_wav_header(&mut file, 0).map_err(|err| {
        error!(?err, "placeholder header write failed");
        Consts::RC_OUTPUT_WRITE
    })?;
    let target_frames = u64::from(seconds) * u64::from(Consts::SAMPLE_RATE.get());
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(Consts::CHANNELS, Consts::SAMPLE_RATE))
        .frames(0..target_frames)
        .build();
    let cancel = CancelScope::new(None);
    let mut sink = WavSink {
        file,
        bytes: Vec::new(),
        samples: 0,
    };
    let report = host
        .render(&request, &cancel.token(), &mut sink)
        .map_err(|err| {
            error!(?err, "offline Host render failed");
            if matches!(err, OfflineRenderError::Sink { .. }) {
                Consts::RC_OUTPUT_WRITE
            } else {
                Consts::RC_AUDIO_BUILD
            }
        })?;
    if report.frames != target_frames
        || sink.samples as u64 != target_frames * u64::from(Consts::CHANNELS)
    {
        error!(
            frames = report.frames,
            samples = sink.samples,
            target_frames,
            "incomplete offline render"
        );
        return Err(Consts::RC_AUDIO_BUILD);
    }
    host.remove(&member).map_err(|err| {
        error!(?err, "offline player removal failed");
        Consts::RC_AUDIO_BUILD
    })?;
    sink.file.flush().map_err(|err| {
        error!(?err, "output flush failed");
        Consts::RC_OUTPUT_WRITE
    })?;
    write_wav_header(&mut sink.file, sink.samples).map_err(|err| {
        error!(?err, "header rewrite failed");
        Consts::RC_HEADER_REWRITE
    })?;
    info!(
        frames = report.frames,
        samples = sink.samples,
        "offline capture: done"
    );
    Ok(())
}

struct WavSink {
    file: File,
    bytes: Vec<u8>,
    samples: usize,
}

impl RenderSink for WavSink {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.bytes.clear();
        for sample in samples {
            self.bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.file
            .write_all(&self.bytes)
            .map_err(RenderSinkError::new)?;
        self.samples += samples.len();
        Ok(())
    }
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

    let device_name = device.description().map_or_else(
        |_| "<unknown>".to_owned(),
        |description| description.name().to_owned(),
    );
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
        .get()
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
    file.write_all(&Consts::SAMPLE_RATE.get().to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&Consts::BITS_PER_SAMPLE.to_le_bytes())?;

    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    Ok(())
}
