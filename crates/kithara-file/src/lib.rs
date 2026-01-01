#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_core::AssetId;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum FileError {
    #[error("not implemented")]
    Unimplemented,
}

pub type FileResult<T> = Result<T, FileError>;

#[derive(Clone, Debug, Default)]
pub struct FileSourceOptions;

#[derive(Clone, Debug)]
pub struct FileSource;

impl FileSource {
    pub async fn open(_url: Url, _opts: FileSourceOptions) -> FileResult<FileSession> {
        unimplemented!("kithara-file: FileSource::open is not implemented yet")
    }
}

#[derive(Clone, Debug)]
pub struct FileSession;

impl FileSession {
    pub fn asset_id(&self) -> AssetId {
        unimplemented!("kithara-file: FileSession::asset_id is not implemented yet")
    }

    pub fn stream(
        &self,
    ) -> Box<dyn futures::Stream<Item = FileResult<Bytes>> + Send + Unpin + 'static> {
        Box::new(futures::stream::empty())
    }
}
