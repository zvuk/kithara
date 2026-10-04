use crate::live::{CheckedConfig, LiveConfig};

/// Retained configuration that exposes an owned control-thread snapshot.
///
/// Snapshotting may clone or allocate and does not imply realtime safety.
/// Validation, preparation and application stay with the domain owner.
/// ```compile_fail
/// #[derive(kithara_config::Config)]
/// struct Resource<T> { #[config(value)] resource: T }
/// ```
pub trait Config {
    /// Readable values; construction-only resources and secrets are excluded.
    type Values;

    /// Reads values without exposing excluded construction inputs.
    /// ```compile_fail
    /// #[derive(kithara_config::Config)]
    /// struct Secret { #[config(skip = "credential")] token: String }
    /// let config = Secret::builder().token(String::from("private")).build();
    /// let _ = kithara_config::Config::values(&config).token;
    /// ```
    fn values(&self) -> Self::Values;
}

/// An owner that retains the configuration governing its behavior.
///
/// Consumers should read settings through this reference. A snapshot from
/// [`Config::values`] is for observation, not a second mutable source.
pub trait ConfigOwner {
    /// The retained configuration type.
    type Config: Config;

    /// The configuration used by this owner.
    fn config(&self) -> &Self::Config;
}

/// An owner whose accepted configuration can be changed through a mutable borrow.
///
/// Owners with shared or realtime state keep their own application boundary.
pub trait ConfigOwnerMut: ConfigOwner {
    /// The same configuration returned by [`ConfigOwner::config`].
    fn config_mut(&mut self) -> &mut Self::Config;

    /// Assigns one live field once its field check accepts the value.
    ///
    /// # Errors
    /// Returns the field check's refusal without changing the configuration.
    fn apply_config_change(
        &mut self,
        change: <Self::Config as LiveConfig>::Change,
    ) -> Result<(), <Self::Config as CheckedConfig>::Error>
    where
        Self::Config: LiveConfig,
    {
        let change = <Self::Config as LiveConfig>::check(change)?;
        self.config_mut().apply_change(change);
        Ok(())
    }
}
