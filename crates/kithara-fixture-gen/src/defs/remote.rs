#![cfg(feature = "remote")]

use bytes::Bytes;
use kithara_drm::UniqueBinaryCipher;
use kithara_platform::time::Duration;
use kithara_test_macros as kithara;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;
use url::Url;

use crate::{
    context::BuildContext,
    hls_hydrate::{KeyPolicy, Options, flatten, hydrate},
};

mod consts {
    use super::Duration;

    pub(super) const AUTH_ENV: &str = "KITHARA_DRM_PROD_AUTH_TOKEN";
    pub(super) const CIPHER_ENV: &str = "KITHARA_DRM_PROD_KEY";
    pub(super) const MASTER: &str =
        "https://cdn-hls-slicer.zvuk.com/drm/track/172833120_3/master.m3u8";
    pub(super) const SEED: &str = "aaaaaaaa";
    pub(super) const SP_ZV_ENV: &str = "KITHARA_DRM_PROD_SP_ZV_TOKEN";
    pub(super) const TIMEOUT: Duration = Duration::from_secs(20);
    pub(super) const TRACK_TIMEOUT: Duration = Duration::from_secs(300);
}

#[derive(Debug, Error)]
enum RemoteError {
    #[error("repository variable {0} is missing")]
    Missing(&'static str),
    #[error("repository variable {0} is not a valid HTTP header value")]
    Header(&'static str),
    #[error("invalid remote fixture URL")]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Hydrate(#[from] crate::hls_hydrate::HydrateError),
}

fn required(name: &'static str) -> Result<String, RemoteError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(RemoteError::Missing(name))
}

fn header(
    headers: &mut HeaderMap,
    name: &'static str,
    env: &'static str,
) -> Result<(), RemoteError> {
    let value = HeaderValue::from_str(&required(env)?).map_err(|_| RemoteError::Header(env))?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

/// Zvuk production request options: the account headers, and the key
/// processor that turns a keyserver answer into the AES-128 key.
fn options(timeout: Duration) -> Result<Options, RemoteError> {
    let cipher_key = required(consts::CIPHER_ENV)?;
    let mut headers = HeaderMap::new();
    header(&mut headers, "x-auth-token", consts::AUTH_ENV)?;
    header(&mut headers, "x-sp-zv", consts::SP_ZV_ENV)?;
    let mut key_headers = headers.clone();
    key_headers.insert("x-encrypted-key", HeaderValue::from_static(consts::SEED));
    let cipher = UniqueBinaryCipher::new(&format!("{cipher_key}{}", consts::SEED));
    let processor =
        Box::new(move |bytes: Vec<u8>| Ok(cipher.decrypt(&Bytes::from(bytes)).to_vec()));
    Ok(Options {
        headers,
        timeout,
        key: Some(KeyPolicy {
            processor,
            headers: key_headers,
        }),
        refresh: &[consts::AUTH_ENV, consts::SP_ZV_ENV],
    })
}

#[kithara::asset(
    ext = "toml",
    content_type = "application/x-kithara-hls-bundle",
    env = [
        "KITHARA_DRM_PROD_AUTH_TOKEN",
        "KITHARA_DRM_PROD_KEY",
        "KITHARA_DRM_PROD_SP_ZV_TOKEN"
    ],
    optional
)]
#[case::gapless_172833120_3()]
fn remote_hls_bundle(context: &BuildContext<'_>) -> Result<Vec<u8>, RemoteError> {
    let master = Url::parse(consts::MASTER)?;
    Ok(hydrate(context, &master, &options(consts::TIMEOUT)?)?)
}

/// A catalogue track as delivered: the widest variant of its HLS stream,
/// decrypted into one fMP4 file.
#[kithara::asset(
    ext = "mp4",
    content_type = "audio/mp4",
    env = [
        "KITHARA_DRM_PROD_AUTH_TOKEN",
        "KITHARA_DRM_PROD_KEY",
        "KITHARA_DRM_PROD_SP_ZV_TOKEN"
    ],
    optional
)]
#[case::techno_160997949_1("https://cdn-hls-slicer.zvuk.com/drm/track/160997949_1/master.m3u8")]
fn remote_track(_context: &BuildContext<'_>, master: &str) -> Result<Vec<u8>, RemoteError> {
    Ok(flatten(
        &Url::parse(master)?,
        &options(consts::TRACK_TIMEOUT)?,
    )?)
}
