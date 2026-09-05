use std::error::Error as StdError;

use kithara::bufpool::HasPool;
use kithara_assets::{
    AcquisitionResult, AssetReader, AssetStore, AssetWriter, AssetsError, ResourceKey, WriteSide,
};
use kithara_record::RecordingSink;
use thiserror::Error;

/// `RecordingSink` adapter for one canonical `AssetStore` resource transaction.
#[derive(Debug)]
pub struct AssetPartSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    key: ResourceKey,
    store: AssetStore<S>,
    writer: Option<AssetWriter<S>>,
}

impl<S> AssetPartSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Acquire a new pending asset resource as a recording transaction.
    ///
    /// # Errors
    /// Returns an assets error or rejects an already committed resource.
    pub fn acquire(store: &AssetStore<S>, key: &ResourceKey) -> Result<Self, AssetPartSinkError> {
        match store.acquire_resource(key, None)? {
            AcquisitionResult::Pending(writer) => Ok(Self {
                key: key.clone(),
                store: store.clone(),
                writer: Some(writer),
            }),
            AcquisitionResult::Ready(_) => Err(AssetPartSinkError::AlreadyCommitted),
            _ => Err(AssetPartSinkError::UnexpectedAcquisition),
        }
    }

    fn storage<E>(error: E) -> AssetPartSinkError
    where
        E: StdError + Send + Sync + 'static,
    {
        AssetPartSinkError::Storage(Box::new(error))
    }
}

impl<S> RecordingSink for AssetPartSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    type Output = AssetReader<S>;
    type Error = AssetPartSinkError;

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Self::Error> {
        self.writer
            .as_ref()
            .ok_or(AssetPartSinkError::Closed)?
            .write_at(offset, bytes)
            .map_err(Self::storage)
    }

    fn commit(&mut self, final_len: u64) -> Result<Self::Output, Self::Error> {
        self.writer
            .take()
            .ok_or(AssetPartSinkError::Closed)?
            .commit(Some(final_len))
            .map_err(Self::storage)
    }

    fn abort(&mut self) {
        if self.writer.take().is_none() {
            return;
        }
        if let Err(error) = self.store.remove_resource(&self.key) {
            tracing::warn!(%error, key = ?self.key, "recording asset rollback failed");
        }
    }
}

/// Failure while opening or operating an `AssetStore` recording sink.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetPartSinkError {
    /// Asset acquisition failed.
    #[error(transparent)]
    Assets(#[from] AssetsError),
    /// The target already contains a committed resource.
    #[error("recording asset is already committed")]
    AlreadyCommitted,
    /// The transaction has already committed or aborted.
    #[error("recording asset transaction is closed")]
    Closed,
    /// A newer assets acquisition phase is not supported by this adapter.
    #[error("recording asset returned an unsupported acquisition phase")]
    UnexpectedAcquisition,
    /// Backing write or commit failed.
    #[error("recording asset storage failed: {0}")]
    Storage(#[source] Box<dyn StdError + Send + Sync>),
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AssetResource, AssetSource, AssetStore, ReadSide, StorageBackend};
    use kithara_encode::EncodeConfig;
    use kithara_record::{RecordingConfig, RecordingCore};
    use kithara_test_utils::kithara;

    use super::AssetPartSink;
    use crate::pools;

    struct RecordingArtifact;

    #[kithara::test]
    fn recording_core_commits_a_readable_wav_to_memory_assets() {
        let pool = pools::build(&pools::PoolsSection::default())
            .unwrap_or_else(|error| panic!("app pools: {error}"));
        let store = AssetStore::builder(pool)
            .backend(StorageBackend::Memory)
            .build();
        let source = AssetSource::Local {
            path: std::env::temp_dir().join("kithara-recording-core-test"),
        };
        let key = store
            .scope::<RecordingArtifact>(&source)
            .and_then(|scope| {
                scope.key(&AssetResource::Named {
                    namespace: "recordings".to_owned(),
                    name: "master.wav".to_owned(),
                })
            })
            .unwrap_or_else(|error| panic!("recording asset key: {error}"));
        let sink = AssetPartSink::acquire(&store, &key)
            .unwrap_or_else(|error| panic!("recording sink: {error}"));
        let config = RecordingConfig::builder()
            .encode(
                EncodeConfig::builder()
                    .sample_rate(48_000)
                    .channels(2)
                    .build(),
            )
            .build();
        let mut recording = RecordingCore::new(&config, sink, Some(2))
            .unwrap_or_else(|error| panic!("recording session: {error}"));

        recording
            .push(&[0.25, -0.25, 0.5, -0.5])
            .unwrap_or_else(|error| panic!("record PCM: {error}"));
        let _reader = recording
            .finish()
            .unwrap_or_else(|error| panic!("finish recording: {error}"));

        let reader = store
            .open_resource(&key, None)
            .unwrap_or_else(|error| panic!("reopen committed recording: {error}"));
        let len = reader.len().expect("committed WAV length");
        let mut bytes = vec![0_u8; usize::try_from(len).expect("test WAV length fits usize")];
        let read = reader
            .read_at(0, &mut bytes)
            .unwrap_or_else(|error| panic!("read committed recording: {error}"));

        assert_eq!(read, 60);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3);
        assert_eq!(&bytes[44..48], &0.25_f32.to_le_bytes());
    }
}
