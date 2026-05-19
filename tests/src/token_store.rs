use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hls_url::HlsSpec;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum TokenRoute {
    Signal,
    Hls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TokenRequest {
    pub hls_spec: Option<HlsSpec>,
    pub signal_kind: Option<String>,
    pub signal_spec_with_ext: Option<String>,
    pub route: TokenRoute,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TokenResponse {
    pub token: String,
}

pub(crate) fn is_token(candidate: &str) -> bool {
    Uuid::parse_str(candidate).is_ok()
}
