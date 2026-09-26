/// Retained configuration that exposes an owned control-thread snapshot.
///
/// Snapshotting may clone or allocate and does not imply realtime safety.
/// Validation, preparation and application stay with the domain owner.
/// ```compile_fail
/// #[kithara_config::config]
/// struct Resource<T> { #[config(value)] resource: T }
/// ```
pub trait Config {
    /// Readable values; construction-only resources and secrets are excluded.
    type Values;

    /// Reads values without exposing excluded construction inputs.
    /// ```compile_fail
    /// #[kithara_config::config]
    /// struct Secret { #[config(skip = "credential")] token: String }
    /// let config = Secret::builder().token(String::from("private")).build();
    /// let _ = kithara_config::Config::values(&config).token;
    /// ```
    fn values(&self) -> Self::Values;
}
