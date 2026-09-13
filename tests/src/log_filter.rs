#[cfg(not(target_arch = "wasm32"))]
use std::env;

/// Get a `tracing` filter string for tests with wasm-safe fallback.
#[inline]
#[must_use]
pub fn rust_log_filter(default: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env::var("RUST_LOG").unwrap_or_else(|_| default.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        default.to_string()
    }
}
