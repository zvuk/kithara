use std::{num::NonZeroU32, sync::LazyLock};

use kithara::{host::HostOwned, play::player::PlayerControlSource};

use crate::{
    FfiHostConfig,
    core::host::lifecycle::Lifecycle,
    pools::{FfiHost, FfiPools},
    types::FfiError,
};

static HOST: LazyLock<Lifecycle<FfiHost>> = LazyLock::new(Lifecycle::default);

/// Initialize the process-wide audio host exactly once.
///
/// # Errors
/// Returns a typed lifecycle or host-construction error.
pub(crate) fn initialize_host(config: FfiHostConfig) -> Result<(), FfiError> {
    HOST.initialize(|| FfiHost::new(config.into_domain()?).map_err(FfiError::from))
}

pub(crate) fn ensure_default_host() -> Result<(), FfiError> {
    HOST.ensure_initialized(|| {
        FfiHost::new(FfiHostConfig::default().into_domain()?).map_err(FfiError::from)
    })
}

#[cfg(test)]
pub(crate) fn initialize_test_host() {
    ensure_default_host()
        .unwrap_or_else(|error| panic!("test FFI host initialization failed: {error}"));
}

pub(crate) fn insert<P>(player: P) -> Result<HostOwned<P>, FfiError>
where
    P: PlayerControlSource<Schema = FfiPools>,
{
    HOST.with_ready_mut(|host| host.insert(player))?
        .map_err(FfiError::from)
}

pub(crate) fn requested_sample_rate() -> Result<NonZeroU32, FfiError> {
    HOST.with_ready(FfiHost::requested_sample_rate)
}

pub(crate) fn remove<P>(player: &HostOwned<P>) -> Result<(), FfiError>
where
    P: PlayerControlSource<Schema = FfiPools>,
{
    HOST.with_ready_mut(|host| host.remove(player))?
        .map_err(FfiError::from)
}
