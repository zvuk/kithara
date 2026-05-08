use kithara_stream::AudioCodec;
use thiserror::Error;

use crate::DecodeError;

#[derive(Debug, Error)]
pub(crate) enum AndroidBackendError {
    #[error("android hardware backend is unavailable on API level {api_level}")]
    ApiLevelUnavailable { api_level: String },

    #[error("android hardware backend does not support codec {codec:?}")]
    UnsupportedCodec { codec: AudioCodec },

    #[error("android backend does not support pcm encoding {encoding}")]
    UnsupportedPcmEncoding { encoding: i32 },

    #[error("android backend failed during {operation}: {details}")]
    Operation {
        operation: &'static str,
        details: String,
    },
}

impl AndroidBackendError {
    pub(crate) fn api_level_unavailable(api_level: Option<u32>) -> Self {
        Self::ApiLevelUnavailable {
            api_level: api_level.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        }
    }

    pub(crate) fn into_decode_error(self) -> DecodeError {
        match self {
            Self::UnsupportedCodec { codec } => DecodeError::UnsupportedCodec(codec),
            Self::UnsupportedPcmEncoding { .. } => DecodeError::Backend(Box::new(self)),
            other => DecodeError::Backend(Box::new(other)),
        }
    }

    pub(crate) fn operation(operation: &'static str, details: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            details: details.into(),
        }
    }
}
