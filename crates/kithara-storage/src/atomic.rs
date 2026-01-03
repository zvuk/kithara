#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

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
}

#[async_trait]
impl Resource for AtomicResource {
    async fn write(&self, data: &[u8]) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        {
            let state = self.inner.state.lock().await;
            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
        }

        if let Some(parent) = self.inner.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let tmp_path = tmp_path_for(&self.inner.path);

        // Write temp file
        tokio::fs::write(&tmp_path, data).await?;

        // Best-effort durability would be: sync file + sync dir. We are intentionally not doing
        // that yet; `rename` provides atomicity but not fsync durability.
        tokio::fs::rename(&tmp_path, &self.inner.path).await?;

        Ok(())
    }

    async fn read(&self) -> StorageResult<Bytes> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        {
            let state = self.inner.state.lock().await;
            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
        }

        match tokio::fs::read(&self.inner.path).await {
            Ok(v) => Ok(Bytes::from(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Bytes::new()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn commit(&self, _final_len: Option<u64>) -> StorageResult<()> {
        // Atomic resource commits are a no-op: each write is whole-object.
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        let state = self.inner.state.lock().await;
        if let Some(err) = &state.failed {
            return Err(StorageError::Failed(err.clone()));
        }

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

fn tmp_path_for(final_path: &Path) -> PathBuf {
    // Simple deterministic temp path. We avoid random temp names to keep it small and predictable.
    // Concurrency is serialized by the caller (higher layers), and this resource isn't intended
    // for concurrent writers.
    let mut p = final_path.to_path_buf();

    let mut file_name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());

    file_name.push_str(".tmp");
    p.set_file_name(file_name);
    p
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "tests will be added once the new API is stabilized"]
    fn placeholder() {}
}
