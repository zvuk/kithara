use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("URL canonicalization failed: {0}")]
    Canonicalization(String),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
