use kithara_platform::time::Duration;
use reqwest::{blocking::Client, header::HeaderMap};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::hls_hydrate::{Deadline, HydrateError, RedactedUrl, fetch};

#[derive(Debug, Error)]
pub(crate) enum RemoteFileError {
    #[error(transparent)]
    Fetch(#[from] HydrateError),
    #[error("invalid remote fixture URL")]
    Url(#[from] url::ParseError),
    #[error("{url}: expected {expected} bytes, received {received}")]
    Length {
        url: RedactedUrl,
        expected: u64,
        received: u64,
    },
    #[error("{url}: SHA-256 mismatch, expected {expected}, received {received}")]
    Digest {
        url: RedactedUrl,
        expected: String,
        received: String,
    },
}

/// Downloads one public file and verifies its size and SHA-256 digest.
pub(crate) fn fetch_verified(
    url: &Url,
    sha256_hex: &str,
    length: u64,
    timeout: Duration,
) -> Result<Vec<u8>, RemoteFileError> {
    let client = Client::builder()
        .build()
        .map_err(|source| HydrateError::Request {
            url: RedactedUrl::new(url),
            source: source.without_url(),
        })?;
    let bytes = fetch(&client, url, &HeaderMap::new(), Deadline::new(timeout), &[])?;
    let received = bytes.len() as u64;
    if received != length {
        return Err(RemoteFileError::Length {
            url: RedactedUrl::new(url),
            expected: length,
            received,
        });
    }
    let digest = hex::encode(Sha256::digest(&bytes));
    if digest != sha256_hex {
        return Err(RemoteFileError::Digest {
            url: RedactedUrl::new(url),
            expected: sha256_hex.to_owned(),
            received: digest,
        });
    }
    Ok(bytes)
}
