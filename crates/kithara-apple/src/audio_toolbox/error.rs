use thiserror::Error;

use super::sys::{OSStatus, os_status_to_string};

#[derive(Debug, Error)]
pub enum AudioToolboxError {
    #[error("{detail}")]
    Config { detail: &'static str },
    #[error("{op} failed: {status} ({message})")]
    Status {
        op: &'static str,
        status: OSStatus,
        message: String,
    },
}

impl AudioToolboxError {
    #[must_use]
    pub fn config(detail: &'static str) -> Self {
        Self::Config { detail }
    }

    #[must_use]
    pub fn status(op: &'static str, status: OSStatus) -> Self {
        Self::Status {
            op,
            status,
            message: os_status_to_string(status),
        }
    }
}
