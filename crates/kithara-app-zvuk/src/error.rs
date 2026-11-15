use std::num::NonZeroU16;

use kithara_net::NetError;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// Structured server diagnostics for a rejected `GraphQL` operation.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQlError {
    /// Server-provided description of the failure.
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) path: Vec<Value>,
}

/// A catalogue operation that could not produce a confirmed result.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// The HTTP 401 token-rejection response.
    #[error("Zvuk rejected the authentication token")]
    AuthenticationRejected,
    /// Any other non-success HTTP status.
    #[error("Zvuk HTTP {0}")]
    Status(NonZeroU16),
    /// The HTTP transport failed before returning a response.
    #[error("Zvuk HTTP transport failed")]
    Transport(#[source] NetError),
    /// The server reported structured `GraphQL` failures.
    #[error("{}", messages(.0))]
    GraphQl(Vec<GraphQlError>),
    /// The response did not satisfy the requested operation's shape.
    #[error("Invalid Zvuk response: {0}")]
    Protocol(&'static str),
}

impl From<NetError> for Error {
    fn from(error: NetError) -> Self {
        match error {
            NetError::Status { status, .. } if status.get() == 401 => Self::AuthenticationRejected,
            NetError::Status { status, .. } => Self::Status(status),
            other => Self::Transport(other),
        }
    }
}

fn messages(errors: &[GraphQlError]) -> String {
    errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<&str>>()
        .join("; ")
}
