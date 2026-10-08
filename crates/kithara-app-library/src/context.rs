use kithara_net::HttpClient;
use kithara_platform::{CancelToken, tokio::runtime::Handle};
use kithara_ui::error::UiDocError;
use serde::de::DeserializeOwned;
use serde_yaml_ng::Value;

use crate::Registration;

/// Shared runtime and HTTP client for plugins.
#[derive(Clone)]
pub struct Environment {
    runtime: Handle,
    net: HttpClient,
}

impl Environment {
    #[must_use]
    pub const fn new(runtime: Handle, net: HttpClient) -> Self {
        Self { runtime, net }
    }

    /// Runtime for plugin tasks.
    #[must_use]
    pub const fn runtime(&self) -> &Handle {
        &self.runtime
    }

    /// The application's HTTP client.
    #[must_use]
    pub const fn net(&self) -> &HttpClient {
        &self.net
    }
}

/// Plugin cancellation token and resolved `sources` configuration entry.
pub struct Context {
    cancel: CancelToken,
    section: Value,
}

impl Context {
    #[must_use]
    pub const fn new(cancel: CancelToken, section: Value) -> Self {
        Self { cancel, section }
    }

    /// Consumes the context and returns the plugin cancellation token.
    #[must_use]
    pub fn cancel(self) -> CancelToken {
        self.cancel
    }

    /// Deserializes the configuration entry into the plugin schema.
    ///
    /// # Errors
    /// Returns [`SectionError`] when the entry does not match that schema.
    pub fn section<T: DeserializeOwned>(&self) -> Result<T, SectionError> {
        T::deserialize(&self.section).map_err(|_| SectionError)
    }
}

/// Configuration schema mismatch. Omits the entry value to protect secrets.
#[derive(Debug, thiserror::Error)]
#[error("the entry does not match the plugin's schema")]
pub struct SectionError;

/// Registration failure with the plugin id.
#[derive(Debug, thiserror::Error)]
#[error("sources.{id}: {cause}")]
pub struct RegisterError {
    id: &'static str,
    #[source]
    cause: Cause,
}

impl RegisterError {
    /// Associates a registration failure with its plugin id.
    #[must_use]
    pub const fn new(id: &'static str, cause: Cause) -> Self {
        Self { id, cause }
    }
}

/// Plugin registration error.
#[derive(Debug, thiserror::Error)]
pub enum Cause {
    #[error(transparent)]
    Section(#[from] SectionError),
    #[error("the plugin's document does not parse")]
    Document(#[source] Box<UiDocError>),
}

impl From<UiDocError> for Cause {
    fn from(error: UiDocError) -> Self {
        Self::Document(Box::new(error))
    }
}

/// Registers a plugin from its `sources` configuration entry.
pub struct Factory {
    /// Plugin id and key in the `sources` map.
    pub id: &'static str,
    /// Registers the plugin with shared services and its own context.
    pub register: fn(&Environment, Context) -> Result<Registration, Cause>,
}
