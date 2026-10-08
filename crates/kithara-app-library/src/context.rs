use kithara_net::HttpClient;
use kithara_platform::{CancelToken, tokio::runtime::Handle};
use kithara_ui::error::UiDocError;
use serde::de::DeserializeOwned;
use serde_yaml_ng::Value;

use crate::Registration;

/// What the application shares with every plugin, built once.
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

    /// The runtime a plugin spawns its tasks on.
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

/// What is a plugin's own: a cancellation of its own and its entry of the
/// document's `sources` map, references already resolved.
pub struct Context {
    cancel: CancelToken,
    section: Value,
}

impl Context {
    #[must_use]
    pub const fn new(cancel: CancelToken, section: Value) -> Self {
        Self { cancel, section }
    }

    /// The plugin's cancellation, taken once its entry has been read.
    #[must_use]
    pub fn cancel(self) -> CancelToken {
        self.cancel
    }

    /// The plugin's entry in the schema the plugin owns.
    ///
    /// # Errors
    /// Returns [`SectionError`] when the entry does not match that schema.
    pub fn section<T: DeserializeOwned>(&self) -> Result<T, SectionError> {
        T::deserialize(&self.section).map_err(|_| SectionError)
    }
}

/// A `sources` entry its plugin cannot read. It carries no part of the value,
/// which may hold a resolved secret.
#[derive(Debug, thiserror::Error)]
#[error("the entry does not match the plugin's schema")]
pub struct SectionError;

/// A plugin that could not register, named by its factory id.
#[derive(Debug, thiserror::Error)]
#[error("sources.{id}: {cause}")]
pub struct RegisterError {
    id: &'static str,
    #[source]
    cause: Cause,
}

impl RegisterError {
    /// Plugin `id` could not register for `cause`.
    #[must_use]
    pub const fn new(id: &'static str, cause: Cause) -> Self {
        Self { id, cause }
    }
}

/// Why a plugin could not register.
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

/// A plugin the application can mount, keyed by the `sources` entry it reads.
pub struct Factory {
    /// The plugin's id, which names its `sources` entry.
    pub id: &'static str,
    /// Builds the plugin's registration from what the application shares and
    /// what is its own.
    pub register: fn(&Environment, Context) -> Result<Registration, Cause>,
}
