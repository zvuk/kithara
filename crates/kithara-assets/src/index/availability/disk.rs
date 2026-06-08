#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::Ordering},
};

use kithara_platform::{CancellationToken, Mutex};
use kithara_storage::{Atomic, MmapDriver, StorageError};

use super::core::{Availability, AvailabilityIndex, InnerIndex};
use crate::{
    error::{AssetsError, AssetsResult},
    index::{
        persist,
        schema::{AssetAvailabilityFile, AvailabilityFile, ResourceAvailabilityFile},
    },
};

pub(super) struct AvailabilityPersist {
    cancel: CancellationToken,
    res: OnceLock<Atomic<MmapDriver>>,
    path: PathBuf,
}

impl AvailabilityIndex {
    /// Enable disk persistence rooted at `path`. Hydrates the
    /// in-memory aggregate from the existing on-disk snapshot (if
    /// any), then caches the `Atomic<MmapDriver>` for subsequent
    /// flushes. Idempotent: subsequent calls are no-ops.
    ///
    /// Failures (open, load) collapse silently — the aggregate
    /// stays empty and the persist resource is materialised lazily
    /// on first flush.
    pub(crate) fn enable_persistence(&self, path: PathBuf, cancel: CancellationToken) {
        let opened = if path.exists() {
            match persist::open_existing(&path, &cancel) {
                Ok(res) => {
                    let atomic = Atomic::new(res);
                    let _ = self.load_from(&atomic);
                    Some(atomic)
                }
                Err(e) => {
                    tracing::debug!("open existing availability.bin failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        let _ = self.inner.persist.set(AvailabilityPersist {
            path,
            cancel,
            res: opened.map_or_else(OnceLock::new, |a| {
                let cell = OnceLock::new();
                cell.set(a)
                    .unwrap_or_else(|_| unreachable!("freshly created cell"));
                cell
            }),
        });
    }

    /// Load the availability index from a persistent resource.
    pub(crate) fn load_from(&self, res: &Atomic<MmapDriver>) -> AssetsResult<()> {
        let mut buf = Vec::new();
        let n = res.read_into(&mut buf)?;
        if n == 0 {
            return Ok(());
        }

        let archived = match rkyv::access::<
            crate::index::schema::ArchivedAvailabilityFile,
            rkyv::rancor::Error,
        >(&buf[..n])
        {
            Ok(archived) => archived,
            Err(e) => {
                tracing::debug!("Failed to validate availability index: {}", e);
                return Ok(());
            }
        };

        for (root, asset_record) in archived.assets.iter() {
            let root_str = root.as_str().to_string();
            let asset_map = Arc::new(dashmap::DashMap::new());

            for (path, res_record) in asset_record.resources.iter() {
                let mut avail = Availability::default();
                res_record
                    .ranges
                    .iter()
                    .for_each(|r| avail.insert(r.0.to_native()..r.1.to_native()));

                let final_len: Option<u64> = res_record.final_len.as_ref().map(|l| l.to_native());

                match final_len {
                    Some(flen) => avail.mark_committed(flen),
                    None => avail.committed = res_record.is_committed,
                }

                asset_map.insert(path.as_str().to_string(), Arc::new(Mutex::new(avail)));
            }

            self.inner.assets.insert(root_str, asset_map);
        }
        Ok(())
    }

    /// Persist the aggregate index to a caller-supplied storage
    /// resource. Used by the cross-instance roundtrip tests; the
    /// production flush path goes through [`super::Flushable::flush`].
    #[cfg(test)]
    pub(crate) fn persist_to(&self, res: &Atomic<MmapDriver>) -> AssetsResult<()> {
        write_aggregate(&self.inner, res, false)
    }
}

impl InnerIndex {
    pub(super) fn flush_with_durability(&self, durable: bool) -> AssetsResult<()> {
        let Some(p) = self.persist.get() else {
            self.dirty.store(false, Ordering::Release);
            return Ok(());
        };
        let atomic = persist::init_atomic(&p.res, &p.path, &p.cancel)?;
        write_aggregate(self, atomic, durable)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

/// Serialise the aggregate into an `Atomic`-wrapped storage resource.
fn write_aggregate(
    inner: &InnerIndex,
    res: &Atomic<MmapDriver>,
    durable: bool,
) -> AssetsResult<()> {
    let assets = inner
        .assets
        .iter()
        .filter_map(|entry| {
            let resources: BTreeMap<String, ResourceAvailabilityFile> = entry
                .value()
                .iter()
                .filter_map(|res_entry| {
                    let record = {
                        let avail = res_entry.value().lock_sync();
                        // The crash-recovery snapshot is a COMMITTED-only
                        // contract: an uncommitted partial write (whose `.tmp`
                        // was never renamed) must be invisible after a rebuild,
                        // exactly as the slow path reports it Missing. Persisting
                        // its in-flight ranges would let a flush that races the
                        // writer's cleanup resurrect a partial segment on the
                        // next open — corrupting availability and a real crash
                        // recovery alike. The live in-memory ranges still serve
                        // in-flight readers; only the persisted snapshot filters.
                        if !avail.committed {
                            return None;
                        }
                        ResourceAvailabilityFile {
                            ranges: avail.ranges.iter().map(|r| (r.start, r.end)).collect(),
                            final_len: avail.final_len,
                            is_committed: avail.committed,
                        }
                    };
                    Some((res_entry.key().clone(), record))
                })
                .collect();
            if resources.is_empty() {
                return None;
            }
            Some((entry.key().clone(), AssetAvailabilityFile { resources }))
        })
        .collect();
    let file = AvailabilityFile { assets, version: 1 };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
        .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
    if durable {
        res.write_all_durable(&bytes)?;
    } else {
        res.write_all(&bytes)?;
    }
    Ok(())
}
