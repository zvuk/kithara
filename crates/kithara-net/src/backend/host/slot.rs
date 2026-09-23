use kithara_platform::sync::{Arc, OnceLock};
use thiserror::Error;

use super::transport::HostTransport;
use crate::error::NetError;

/// The process's transport; every client resolves it per request.
static TRANSPORT: OnceLock<Arc<dyn HostTransport>> = OnceLock::new();

/// The process already has a transport; it keeps the first one.
#[derive(Debug, Error)]
#[error("an HTTP transport is already installed in this process")]
pub struct AlreadyInstalled;

/// Install the transport every request in this process runs through.
///
/// # Errors
///
/// Returns [`AlreadyInstalled`] when the process has a transport.
pub fn install(transport: Arc<dyn HostTransport>) -> Result<(), AlreadyInstalled> {
    TRANSPORT.set(transport).map_err(|_| AlreadyInstalled)
}

pub(super) fn installed() -> Result<&'static Arc<dyn HostTransport>, NetError> {
    TRANSPORT.get().ok_or(NetError::NoTransport)
}
