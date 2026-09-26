use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait},
};
use jni::sys::jlong;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub(super) enum ProbeError {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),

    #[error("no default audio output device")]
    NoOutputDevice,

    #[error("cpal {operation} failed: {details}")]
    Cpal {
        operation: &'static str,
        details: String,
    },
}

mod consts {
    pub(super) const FMT_F32: jlong = 0;
    pub(super) const FMT_F64: jlong = 9;
    pub(super) const FMT_I16: jlong = 1;
    pub(super) const FMT_I32: jlong = 4;
    pub(super) const FMT_I64: jlong = 5;
    pub(super) const FMT_I8: jlong = 3;
    pub(super) const FMT_OTHER: jlong = 10;
    pub(super) const FMT_U16: jlong = 2;
    pub(super) const FMT_U32: jlong = 7;
    pub(super) const FMT_U64: jlong = 8;
    pub(super) const FMT_U8: jlong = 6;
}

/// Enumerate the cpal default host and output device, log every supported
/// output config, and report the code of the default output sample format so
/// the host side can assert the f32 contract the firewheel graph produces.
pub(super) fn default_output_format() -> Result<jlong, ProbeError> {
    let host = cpal::default_host();
    info!(host = %host.id().name(), "cpal host");

    let device = host
        .default_output_device()
        .ok_or(ProbeError::NoOutputDevice)?;
    let device_name = device.description().map_or_else(
        |_| "<unknown>".to_owned(),
        |description| description.name().to_owned(),
    );
    info!(device = %device_name, "cpal default output device");

    let default_cfg = device
        .default_output_config()
        .map_err(|err| ProbeError::Cpal {
            operation: "default_output_config",
            details: err.to_string(),
        })?;
    info!(
        sample_format = ?default_cfg.sample_format(),
        channels = default_cfg.channels(),
        sample_rate = default_cfg.sample_rate(),
        buffer_size = %format!("{:?}", default_cfg.buffer_size()),
        "cpal default output config"
    );

    let configs = device
        .supported_output_configs()
        .map_err(|err| ProbeError::Cpal {
            operation: "supported_output_configs",
            details: err.to_string(),
        })?;
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

    Ok(match default_cfg.sample_format() {
        SampleFormat::F32 => consts::FMT_F32,
        SampleFormat::I16 => consts::FMT_I16,
        SampleFormat::U16 => consts::FMT_U16,
        SampleFormat::I8 => consts::FMT_I8,
        SampleFormat::I32 => consts::FMT_I32,
        SampleFormat::I64 => consts::FMT_I64,
        SampleFormat::U8 => consts::FMT_U8,
        SampleFormat::U32 => consts::FMT_U32,
        SampleFormat::U64 => consts::FMT_U64,
        SampleFormat::F64 => consts::FMT_F64,
        _ => consts::FMT_OTHER,
    })
}
