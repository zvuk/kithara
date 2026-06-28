#![forbid(unsafe_code)]

//! Shared conversion traits used across the workspace.

/// Convert `value: T` into `Self` using extra runtime parameters `P` that a
/// plain [`From`]/[`Into`] cannot carry.
pub trait FromWithParams<T, P> {
    /// Build `Self` from `value` and the construction parameters `params`.
    fn build(value: T, params: P) -> Self;
}
