#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU32,
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::Ordering},
};

use dashmap::DashMap;
use kithara_bufpool::BytePool;
use kithara_storage::{Atomic, MmapDriver, StorageError};
use tokio_util::sync::CancellationToken;

use super::core::{PinsIndex, PinsInner};
use crate::{
    error::{AssetsError, AssetsResult},
    index::{persist, schema::PinsIndexFile},
};

pub(super) struct PinsPersist {
    cancel: CancellationToken,
    res: OnceLock<Atomic<MmapDriver>>,
    path: PathBuf,
}

impl PinsIndex {
    /// Construct a disk-backed index rooted at `path`.
    ///
    /// If the file already exists and is non-empty, it is opened and
    /// hydrated synchronously. Otherwise the disk file is **not**
    /// materialised — it appears the first time [`PinsIndex::add`]
    /// or [`PinsIndex::remove`] flush a real change.
    pub fn with_persist_at(path: PathBuf, cancel: CancellationToken, pool: &BytePool) -> Self {
        let (initial, opened) = hydrate_existing(&path, &cancel, pool);
        Self {
            inner: Arc::new(PinsInner {
                pins: initial,
                persist: Some(PinsPersist {
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

impl PinsInner {
    pub(super) fn flush_with_durability(&self, durable: bool) -> AssetsResult<()> {
        let Some(persist) = self.persist.as_ref() else {
            self.dirty.store(false, Ordering::Release);
            return Ok(());
        };
        let snapshot: Vec<String> = self.pins.iter().map(|r| r.key().clone()).collect();
        let atomic = persist::init_atomic(&persist.res, &persist.path, &persist.cancel)?;
        write_pins(atomic, &snapshot, durable)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

fn hydrate_existing(
    path: &std::path::Path,
    cancel: &CancellationToken,
    pool: &BytePool,
) -> (DashMap<String, NonZeroU32>, Option<Atomic<MmapDriver>>) {
    let nonempty = fs::metadata(path).is_ok_and(|m| m.len() > 0);
    if !nonempty {
        return (DashMap::new(), None);
    }
    match persist::open_existing(path, cancel) {
        Ok(res) => {
            let atomic = Atomic::new(res);
            let initial = read_pins(&atomic, pool).unwrap_or_default();
            (initial, Some(atomic))
        }
        Err(e) => {
            tracing::debug!("open existing pins.bin failed: {e}");
            (DashMap::new(), None)
        }
    }
}

fn read_pins(
    res: &Atomic<MmapDriver>,
    pool: &BytePool,
) -> AssetsResult<DashMap<String, NonZeroU32>> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;

    if n == 0 {
        return Ok(DashMap::new());
    }

    let archived =
        match rkyv::access::<crate::index::schema::ArchivedPinsIndexFile, rkyv::rancor::Error>(
            &buf[..n],
        ) {
            Ok(a) => a,
            Err(e) => {
                tracing::debug!("Failed to validate pins index: {}", e);
                return Ok(DashMap::new());
            }
        };

    let pinned = archived
        .pinned
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| (k.as_str().to_string(), NonZeroU32::MIN))
        .collect();
    Ok(pinned)
}

fn write_pins(res: &Atomic<MmapDriver>, pins: &[String], durable: bool) -> AssetsResult<()> {
    let mut map = BTreeMap::new();
    for pin in pins {
        map.insert(pin.clone(), true);
    }
    let file = PinsIndexFile {
        version: 1,
        pinned: map,
    };

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
        .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
    if durable {
        res.write_all_durable(&bytes)?;
    } else {
        res.write_all(&bytes)?;
    }
    Ok(())
}
