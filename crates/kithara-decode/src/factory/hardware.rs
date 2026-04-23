//! Hardware-attempt flow: try the platform backend, classify the outcome.

use kithara_stream::{AudioCodec, ContainerFormat};

use super::DecoderConfig;
use crate::{
    InnerDecoder,
    backend::{BoxedSource, HardwareBackend, RecoverableHardwareError, hardware_accepts},
};

/// Outcome of a single hardware decoder attempt.
pub(super) enum HardwareAttempt {
    /// Hardware backend successfully produced a decoder.
    Decoded(Box<dyn InnerDecoder>),
    /// Hardware disabled or backend does not accept this codec/container.
    /// Source is untouched and ready for the software path.
    Skipped(BoxedSource),
    /// Hardware accepted the codec but construction failed. Source was
    /// recovered from the [`RecoverableHardwareError`] and can be retried
    /// against Symphonia.
    Failed(BoxedSource),
}

/// Attempt the hardware decoder path for `B`.
pub(super) fn try_hardware_decoder<B: HardwareBackend>(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> HardwareAttempt {
    if !config.prefer_hardware {
        return HardwareAttempt::Skipped(source);
    }
    let Some(resolved) = hardware_accepts::<B>(codec, container) else {
        return HardwareAttempt::Skipped(source);
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
    result: Result<Box<dyn InnerDecoder>, RecoverableHardwareError>,
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
        Err(recoverable) => {
            tracing::warn!(
                error = ?recoverable.error,
                ?codec,
                requested_container = ?container,
                resolved_container = ?resolved,
                "Hardware decoder creation failed; falling back to Symphonia"
            );
            HardwareAttempt::Failed(recoverable.source)
        }
    }
}
