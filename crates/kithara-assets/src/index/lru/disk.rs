#![forbid(unsafe_code)]

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::Ordering},
};

use kithara_bufpool::BytePool;
use kithara_platform::{CancelToken, Mutex};
use kithara_storage::{Atomic, MmapDriver, StorageError};

use super::core::{LruIndex, LruInner, LruState};
use crate::{
    error::{AssetsError, AssetsResult},
    index::{persist, schema::LruIndexFile},
};

pub(super) struct LruPersist {
    cancel: CancelToken,
    res: OnceLock<Atomic<MmapDriver>>,
    path: PathBuf,
}

impl LruIndex {
    /// Construct a disk-backed index rooted at `path`.
    ///
    /// If the file already exists and is non-empty it is opened and
    /// hydrated synchronously. Otherwise the disk file is **not**
    /// materialised — it appears on the first [`LruIndex::touch`]
    /// or [`LruIndex::remove`].
    pub(crate) fn with_persist_at(path: PathBuf, cancel: CancelToken, pool: &BytePool) -> Self {
        let (initial, opened) = hydrate_existing(&path, &cancel, pool);
        Self {
            inner: Arc::new(LruInner {
                state: Mutex::new(initial),
                persist: Some(LruPersist {
                    path,
                    cancel,
                    res: opened.map_or_else(OnceLock::new, |a| {
                        let cell = OnceLock::new();
                        cell.set(a)
                            .unwrap_or_else(|_| unreachable!("freshly created cell"));
                        cell
                    }),
                }),
                hub: OnceLock::new(),
                dirty: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }
}

impl LruInner {
    pub(super) fn flush_with_durability(&self, durable: bool) -> AssetsResult<()> {
        let Some(persist) = self.persist.as_ref() else {
            self.dirty.store(false, Ordering::Release);
            return Ok(());
        };
        let snapshot = self.state.lock_sync().clone();
        let atomic = persist::init_atomic(&persist.res, &persist.path, &persist.cancel)?;
        write_state(atomic, &snapshot, durable)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

fn hydrate_existing(
    path: &std::path::Path,
    cancel: &CancelToken,
    pool: &BytePool,
) -> (LruState, Option<Atomic<MmapDriver>>) {
    let nonempty = fs::metadata(path).is_ok_and(|m| m.len() > 0);
    if !nonempty {
        return (LruState::default(), None);
    }
    match persist::open_existing(path, cancel) {
        Ok(res) => {
            let atomic = Atomic::new(res);
            let initial = read_state(&atomic, pool).unwrap_or_default();
            (initial, Some(atomic))
        }
        Err(e) => {
            tracing::debug!("open existing lru.bin failed: {e}");
            (LruState::default(), None)
        }
    }
}

fn read_state(res: &Atomic<MmapDriver>, pool: &BytePool) -> AssetsResult<LruState> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;

    if n == 0 {
        return Ok(LruState::default());
    }

    let file = match rkyv::access::<crate::index::schema::ArchivedLruIndexFile, rkyv::rancor::Error>(
        &buf[..n],
    ) {
        Ok(archived) => rkyv::deserialize::<LruIndexFile, rkyv::rancor::Error>(archived)
            .expect("BUG: LRU archived → owned deserialize"),
        Err(e) => {
            tracing::debug!("Failed to deserialize lru index: {}", e);
            return Ok(LruState::default());
        }
    };

    Ok(LruState::from(file))
}

fn write_state(res: &Atomic<MmapDriver>, state: &LruState, durable: bool) -> AssetsResult<()> {
    let file = LruIndexFile::from(state);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
        .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
    if durable {
        res.write_all_durable(&bytes)?;
    } else {
        res.write_all(&bytes)?;
    }
    Ok(())
}
