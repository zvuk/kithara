use sha2::{Digest, Sha256};

#[must_use]
pub(crate) fn blob_key(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(hash))
}
