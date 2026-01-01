#![forbid(unsafe_code)]

use bytes::Bytes;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("not implemented")]
    Unimplemented,
}

pub type NetResult<T> = Result<T, NetError>;

#[derive(Clone, Debug, Default)]
pub struct NetOptions;

#[derive(Clone, Debug)]
pub struct NetClient {
    _opts: NetOptions,
}

impl NetClient {
    pub fn new(opts: NetOptions) -> Self {
        Self { _opts: opts }
    }

    pub async fn get_bytes(&self, _url: Url) -> NetResult<Bytes> {
        unimplemented!("kithara-net: NetClient::get_bytes is not implemented yet")
    }
}
