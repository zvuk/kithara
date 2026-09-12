use thiserror::Error;

use crate::DecodeError;

#[derive(Debug, Error)]
pub(crate) enum AndroidBackendError {
    #[error("android backend does not support pcm encoding {encoding}")]
    UnsupportedPcmEncoding { encoding: i32 },

    #[error("android backend failed during {operation}: {details}")]
    Operation {
        operation: &'static str,
        details: String,
    },
}

impl AndroidBackendError {
    pub(crate) fn operation(operation: &'static str, details: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            details: details.into(),
        }
    }
}

impl From<AndroidBackendError> for DecodeError {
    fn from(err: AndroidBackendError) -> Self {
        Self::backend(err)
    }
}
