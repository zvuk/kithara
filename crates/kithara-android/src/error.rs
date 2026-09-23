use thiserror::Error;

#[derive(Debug, Error)]
pub enum AndroidBackendError {
    #[error("android runtime was not initialized")]
    NotInitialized,

    #[error("android backend call {operation} failed with status {status}")]
    Status {
        operation: &'static str,
        status: i32,
    },

    #[error("android backend does not support pcm encoding {encoding}")]
    UnsupportedPcmEncoding { encoding: i32 },

    #[error("android backend failed during {operation}: {details}")]
    Operation {
        operation: &'static str,
        details: String,
    },

    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
}

impl AndroidBackendError {
    #[must_use]
    pub const fn status(operation: &'static str, status: i32) -> Self {
        Self::Status { operation, status }
    }

    #[must_use]
    pub fn operation<D: Into<String>>(operation: &'static str, details: D) -> Self {
        Self::Operation {
            operation,
            details: details.into(),
        }
    }

    /// The failure of the JNI call made for `operation`.
    pub(crate) fn jni(operation: &'static str) -> impl FnOnce(jni::errors::Error) -> Self {
        move |error| Self::operation(operation, error.to_string())
    }
}
