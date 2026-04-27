//! Hardware-attempt flow: try the platform backend, classify the outcome.
//!
//! The factory owns the `prefer_hardware` decision — this module only
//! probes whether the backend accepts the codec/container and reports
//! the construction result.

use kithara_stream::{AudioCodec, ContainerFormat};

use super::DecoderConfig;
use crate::{
    InnerDecoder,
    backend::{BoxedSource, HardwareBackend, hardware_accepts},
    error::DecodeError,
};

/// Outcome of a single hardware decoder attempt.
pub(super) enum HardwareAttempt {
    /// Hardware backend successfully produced a decoder.
    Decoded(Box<dyn InnerDecoder>),
    /// Backend does not accept this codec/container.
    Skipped,
    /// Hardware accepted the codec but construction failed. The error
    /// is propagated; no fallback path exists.
    Failed(DecodeError),
}

/// Attempt the hardware decoder path for `B`.
pub(super) fn try_hardware_decoder<B: HardwareBackend>(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> HardwareAttempt {
    let Some(resolved) = hardware_accepts::<B>(codec, container) else {
        return HardwareAttempt::Skipped;
    };
    tracing::info!(
        ?codec,
        requested_container = ?container,
        resolved_container = ?resolved,
        "Attempting hardware decoder"
    );
    into_hardware_attempt(
        B::try_create(source, config, codec, Some(resolved)),
        codec,
        container,
        resolved,
    )
}

fn into_hardware_attempt(
    result: Result<Box<dyn InnerDecoder>, DecodeError>,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    resolved: ContainerFormat,
) -> HardwareAttempt {
    match result {
        Ok(decoder) => {
            tracing::info!(
                ?codec,
                requested_container = ?container,
                resolved_container = ?resolved,
                "Hardware decoder created successfully"
            );
            HardwareAttempt::Decoded(decoder)
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                ?codec,
                requested_container = ?container,
                resolved_container = ?resolved,
                "Hardware decoder creation failed — terminal (no software fallback)"
            );
            HardwareAttempt::Failed(error)
        }
    }
}
