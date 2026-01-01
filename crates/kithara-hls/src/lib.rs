#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_core::AssetId;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum HlsError {
    #[error("not implemented")]
    Unimplemented,
}

pub type HlsResult<T> = Result<T, HlsError>;

#[derive(Clone, Debug, Default)]
pub struct HlsOptions;

#[derive(Clone, Debug)]
pub struct HlsSource;

impl HlsSource {
    pub async fn open(_url: Url, _opts: HlsOptions) -> HlsResult<HlsSession> {
        unimplemented!("kithara-hls: HlsSource::open is not implemented yet")
    }
}

#[derive(Clone, Debug)]
pub struct HlsSession;

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        unimplemented!("kithara-hls: HlsSession::asset_id is not implemented yet")
    }

    pub fn stream(
        &self,
    ) -> Box<dyn futures::Stream<Item = HlsResult<Bytes>> + Send + Unpin + 'static> {
        Box::new(futures::stream::empty())
    }
}
