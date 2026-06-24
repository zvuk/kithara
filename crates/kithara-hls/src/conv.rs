#![forbid(unsafe_code)]

//! Parameterized conversion seam.

/// Convert `value: T` into `Self` using extra runtime parameters `P` that a
/// plain [`From`]/[`Into`] cannot carry. The HLS loading path folds parsed
/// playlist data together with its construction context into the live
/// [`PlaylistState`](crate::PlaylistState), the ABR `VariantInfo` list, and
/// each [`HlsVariant`](crate::variant::HlsVariant).
pub trait FromWithParams<T, P> {
    /// Build `Self` from `value` and the construction parameters `params`.
    fn build(value: T, params: P) -> Self;
}
