#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use kithara_assets::AssetStore;
use kithara_core::AssetId;
use kithara_net::{HttpClient, NetOptions};
use url::Url;

use crate::{FileResult, FileSession, FileSourceOptions};

#[async_trait]
pub trait FileSourceContract: Send + Sync + 'static {
    async fn open(
        &self,
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FileSource;

#[async_trait]
impl FileSourceContract for FileSource {
    async fn open(
        &self,
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession> {
        let asset_id = AssetId::from_url(&url)?;
        let net_client = HttpClient::new(NetOptions::default());

        let session = FileSession::new(asset_id, url, net_client, opts, cache.map(Arc::new));

        Ok(session)
    }
}

impl FileSource {
    pub async fn open(
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession> {
        FileSource.open(url, opts, cache).await
    }
}
