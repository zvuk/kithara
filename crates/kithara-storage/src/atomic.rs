#![forbid(unsafe_code)]

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{AtomicResourceExt, Resource, StorageError, StorageResult};

/// Options for opening an [`AtomicResource`].
#[derive(Clone, Debug)]
pub struct AtomicOptions {
    /// Path to the backing file.
    pub path: PathBuf,

    /// Mandatory cancellation token for this resource lifecycle.
    pub cancel: CancellationToken,
}

impl AtomicOptions {
    pub fn new(path: impl Into<PathBuf>, cancel: CancellationToken) -> Self {
        Self {
            path: path.into(),
            cancel,
        }
    }
}

/// Atomic whole-file resource for small metadata blobs (index, playlists, keys).
///
/// # Contract (normative)
/// - Writes are whole-object and atomic (`temp + rename`) via [`Resource::write`].
/// - Reads are whole-object via [`Resource::read`].
/// - Partial availability is not modeled for atomic resources.
#[derive(Clone)]
pub struct AtomicResource {
    inner: Arc<Inner>,
}

impl fmt::Debug for AtomicResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicResource")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl AtomicResource {
    /// Open (or create) an atomic resource.
    ///
    /// Note: The file may or may not exist. `read_all()` returns `Ok(Bytes::new())` if the file is
    /// absent. This keeps callers simple for "optional metadata" use-cases.
    pub fn open(opts: AtomicOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: opts.path,
                cancel: opts.cancel,
                state: Mutex::new(State::default()),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    async fn preflight(&self) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        // Important: do not hold the state lock across `.await` points.
        let failed = {
            let state = self.inner.state.lock().await;
            state.failed.clone()
        };

        if let Some(err) = failed {
            return Err(StorageError::Failed(err));
        }

        Ok(())
    }
}

#[async_trait]
impl Resource for AtomicResource {
    async fn write(&self, data: &[u8]) -> StorageResult<()> {
        self.preflight().await?;

        let Some(parent) = self.inner.path.parent() else {
            return Err(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "atomic resource path has no parent directory",
            )));
        };

        tokio::fs::create_dir_all(parent).await?;

        // Use a uniquely-named temp file in the same directory as the final path.
        // This avoids collisions and keeps the final `rename` atomic (same filesystem).
        let tmp_path = parent.join(format!(".atomic-{}.tmp", Uuid::new_v4()));
        let mut tmp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .await?;

        tmp.write_all(data).await?;
        tmp.flush().await?;
        tmp.sync_data().await?;
        drop(tmp);

        tokio::fs::rename(&tmp_path, &self.inner.path).await?;

        Ok(())
    }

    async fn read(&self) -> StorageResult<Bytes> {
        self.preflight().await?;

        match tokio::fs::read(&self.inner.path).await {
            Ok(v) => Ok(Bytes::from(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Bytes::new()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn commit(&self, _final_len: Option<u64>) -> StorageResult<()> {
        // Atomic resource commits are a no-op: each write is whole-object.
        self.preflight().await?;
        Ok(())
    }

    async fn fail(&self, error: impl Into<String> + Send) -> StorageResult<()> {
        let mut state = self.inner.state.lock().await;
        state.failed = Some(error.into());
        Ok(())
    }
}

impl AtomicResourceExt for AtomicResource {}

struct Inner {
    path: PathBuf,
    cancel: CancellationToken,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    failed: Option<String>,
}
