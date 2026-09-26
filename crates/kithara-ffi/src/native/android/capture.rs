use std::{
    fmt::Display,
    fs::File,
    io::{Seek, SeekFrom, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use jni::{Env, objects::JString, sys::jint};
use kithara::{
    assets::StorageBackend,
    events::TrackId,
    host::HostConfig,
    output::{OfflineRenderRequest, OfflineRenderer, RenderSink, RenderSinkError},
    platform::{
        CancelScope,
        tokio::runtime::{Builder, Runtime},
    },
    play::{PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource, ResourceSrc, SelectionPlayback},
    signal::AudioSpec,
};
use thiserror::Error;
use tracing::info;

use crate::pools::{FfiHost, FfiResourceConfig, FfiStore, FfiWorker, build as build_pools};

#[derive(Debug, Error)]
pub(super) enum CaptureError {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),

    #[error("offline capture failed at {operation}: {details}")]
    Step {
        operation: &'static str,
        details: String,
    },
}

impl CaptureError {
    fn step<D: Display>(operation: &'static str, details: D) -> Self {
        Self::Step {
            operation,
            details: details.to_string(),
        }
    }
}

mod consts {
    use super::NonZeroU32;

    pub(super) const BITS_PER_SAMPLE: u16 = 32;
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
        Some(rate) => rate,
        None => unreachable!(),
    };
    pub(super) const WAV_FMT_CHUNK_SIZE: u32 = 16;
    pub(super) const WAV_FORMAT_IEEE_FLOAT: u16 = 3;
    pub(super) const WAV_HEADER_BYTES: u32 = 36;
}

pub(super) fn run(
    env: &mut Env<'_>,
    input: &JString<'_>,
    output: &JString<'_>,
    seconds: jint,
) -> Result<(), CaptureError> {
    let input = PathBuf::from(input.try_to_string(env)?);
    let output = PathBuf::from(output.try_to_string(env)?);
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| CaptureError::step("tokio-runtime", err))?;

    render(&runtime, &input, &output, seconds.max(1).unsigned_abs())
}

/// Render through an independent offline Host into a stereo float WAV.
fn render(
    runtime: &Runtime,
    input: &Path,
    output: &Path,
    seconds: u32,
) -> Result<(), CaptureError> {
    info!(
        input = %input.display(),
        output = %output.display(),
        seconds,
        "offline capture: start"
    );

    let pools = build_pools().map_err(|err| CaptureError::step("buffer-pools", err))?;
    let store = FfiStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let worker = FfiWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut host = FfiHost::new(
        HostConfig::offline(pools)
            .sample_rate(consts::SAMPLE_RATE)
            .build(),
    )
    .map_err(|err| CaptureError::step("offline-host", err))?;
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(consts::SAMPLE_RATE)
            .worker(worker)
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
    );
    let member = host
        .insert(player)
        .map_err(|err| CaptureError::step("player-insert", err))?;
    let control = member.control();
    let config: FfiResourceConfig = control
        .prepare_config(
            FfiResourceConfig::for_src(ResourceSrc::Path(input.to_path_buf()))
                .store(store)
                .build(),
        )
        .map_err(|err| CaptureError::step("resource-config", err))?;
    let resource = runtime.block_on(async {
        let mut resource = Resource::new(config).await.map_err(|err| {
            CaptureError::step("resource-open", format!("{}: {err}", input.display()))
        })?;
        resource.preload().await.map_err(|err| {
            CaptureError::step("resource-preload", format!("{}: {err}", input.display()))
        })?;
        Ok::<_, CaptureError>(resource)
    })?;
    control.insert(resource, TrackId::allocate(), None);
    control
        .select_item(0, SelectionPlayback::Play)
        .map_err(|err| CaptureError::step("player-select", err))?;

    let mut file = File::create(output)
        .map_err(|err| CaptureError::step("output-open", format!("{}: {err}", output.display())))?;
    write_wav_header(&mut file, 0).map_err(|err| CaptureError::step("header-write", err))?;
    let target_frames = u64::from(seconds) * u64::from(consts::SAMPLE_RATE.get());
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(consts::CHANNELS, consts::SAMPLE_RATE))
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
        .map_err(|err| CaptureError::step("render", err))?;
    if report.frames != target_frames
        || sink.samples as u64 != target_frames * u64::from(consts::CHANNELS)
    {
        return Err(CaptureError::step(
            "render",
            format!(
                "incomplete render: frames={} samples={} target={target_frames}",
                report.frames, sink.samples
            ),
        ));
    }
    host.remove(&member)
        .map_err(|err| CaptureError::step("player-remove", err))?;
    sink.file
        .flush()
        .map_err(|err| CaptureError::step("output-flush", err))?;
    write_wav_header(&mut sink.file, sink.samples)
        .map_err(|err| CaptureError::step("header-rewrite", err))?;
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

fn write_wav_header(file: &mut File, total_samples: usize) -> std::io::Result<()> {
    let bytes_per_sample = u32::from(consts::BITS_PER_SAMPLE) / 8;
    let channels = u32::from(consts::CHANNELS);
    let data_size = u32::try_from(total_samples)
        .unwrap_or(u32::MAX)
        .saturating_mul(bytes_per_sample);
    let riff_size = consts::WAV_HEADER_BYTES.saturating_add(data_size);
    let byte_rate = consts::SAMPLE_RATE
        .get()
        .saturating_mul(channels)
        .saturating_mul(bytes_per_sample);
    let block_align = (consts::CHANNELS * consts::BITS_PER_SAMPLE) / 8;

    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;

    file.write_all(b"fmt ")?;
    file.write_all(&consts::WAV_FMT_CHUNK_SIZE.to_le_bytes())?;
    file.write_all(&consts::WAV_FORMAT_IEEE_FLOAT.to_le_bytes())?;
    file.write_all(&consts::CHANNELS.to_le_bytes())?;
    file.write_all(&consts::SAMPLE_RATE.get().to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&consts::BITS_PER_SAMPLE.to_le_bytes())?;

    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    Ok(())
}
