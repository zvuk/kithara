#![forbid(unsafe_code)]

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not implemented")]
    Unimplemented,
}

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetId([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceHash([u8; 32]);

impl AssetId {
    pub fn from_url(_url: &url::Url) -> AssetId {
        unimplemented!("kithara-core: AssetId::from_url is not implemented yet")
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ResourceHash {
    pub fn from_url(_url: &url::Url) -> ResourceHash {
        unimplemented!("kithara-core: ResourceHash::from_url is not implemented yet")
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
