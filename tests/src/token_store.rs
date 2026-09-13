use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hls_url::HlsSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TokenRequest {
    pub hls_spec: HlsSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TokenResponse {
    pub token: String,
}

pub(crate) fn is_token(candidate: &str) -> bool {
    Uuid::parse_str(candidate).is_ok()
}
