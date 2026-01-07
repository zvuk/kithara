use sha2::{Digest, Sha256};
use url::Url;

use crate::{canonicalization::canonicalize_for_asset, error::AssetsResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetId([u8; 32]);

impl AssetId {
    pub fn from_url(url: &Url) -> AssetsResult<AssetId> {
        let canonical = canonicalize_for_asset(url)?;
        let hash = Sha256::digest(canonical.as_bytes());
        Ok(AssetId(hash.into()))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
