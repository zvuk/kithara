#[cfg(all(test, not(target_arch = "wasm32")))]
use kithara_platform::time::Duration;

/// Long enough to tell a park apart from a return, short enough to pay for.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const PARK_BUDGET: Duration = Duration::from_millis(50);
