use crate::types::FfiError;

/// Drive pending browser Host commands and key callbacks on the main thread.
/// Call from the application's animation loop after host initialization.
///
/// # Errors
/// Returns a lifecycle error if the browser Host has not been initialized.
#[uniffi::export]
pub fn tick_host() -> Result<(), FfiError> {
    crate::web::bridge::require_initialized_domain()?;
    crate::web::bridge::tick_and_poll();
    crate::web::key_processor_bridge::pump();
    Ok(())
}
