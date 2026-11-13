#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use arc_swap::ArcSwap;
use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::Arc;
use same_file::Handle;

use super::core::{AssetTree, Availability, AvailabilityIndex, Entry, InnerIndex};
use crate::{
    error::AssetsResult,
    index::persistence::{
        FlushHub, Flushable, IndexFile, read_archived,
        schema::{AssetAvailabilityFile, AvailabilityFile, ResourceAvailabilityFile},
        write_record,
    },
};

impl AvailabilityIndex {
    /// Enable disk persistence rooted at `path`. Hydrates the in-memory
    /// aggregate from the existing on-disk snapshot (if any). Idempotent.
    ///
    /// A failed load collapses silently — the aggregate stays empty and the
    /// file is replaced on the first flush.
    pub(crate) fn enable_persistence(&self, path: PathBuf, mut buffer: ByteBuffer) {
        let file = IndexFile::new(path);
        if let Err(e) = self.load_from(&file, &mut buffer) {
            tracing::debug!("read existing availability.bin failed: {e}");
        }
        self.inner.persist_to(file, InnerIndex::write_manifest);
    }

    /// Bind this aggregate to a [`FlushHub`] for coordinated flushing.
    /// Called once per `AssetStore` build; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        self.inner.attach_to(hub);
    }

    /// Force a synchronous flush. Routes through [`FlushHub::flush_now`]
    /// when a hub is attached, or runs the inline serialise+write path
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Propagates the first per-source flush error encountered.
    pub(crate) fn flush(&self) -> AssetsResult<()> {
        if let Some(hub) = self.inner.hub() {
            return hub.flush_now();
        }
        if !self.inner.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let result = Flushable::flush(&*self.inner);
        if result.is_err() {
            self.inner.dirty.store(true, Ordering::Release);
        }
        result
    }

    /// Load the availability index from a persistent resource.
    pub(crate) fn load_from(&self, file: &IndexFile, buf: &mut ByteBuffer) -> AssetsResult<()> {
        let Some(archived) =
            read_archived::<crate::index::schema::ArchivedAvailabilityFile>(file, buf)?
        else {
            return Ok(());
        };

        let mut loaded = AssetTree::new();
        for (root, asset_record) in archived.assets.iter() {
            let mut asset_map = HashMap::new();

            for (path, res_record) in asset_record.resources.iter() {
                let mut avail = Availability::default();
                res_record.ranges.iter().for_each(|r| {
                    avail.insert(r.0.to_native()..r.1.to_native());
                });

                let final_len: Option<u64> = res_record.final_len.as_ref().map(|l| l.to_native());

                match final_len {
                    Some(flen) => {
                        avail.mark_committed(flen);
                    }
                    None => avail.committed = res_record.is_committed,
                }

                asset_map.insert(
                    path.as_str().to_string(),
                    Entry::new(ArcSwap::from_pointee(avail)),
                );
            }

            loaded.insert(root.as_str().to_string(), Arc::new(asset_map));
        }
        self.edit_tree(|tree| tree.extend(loaded.clone()));
        Ok(())
    }

    /// Persist the aggregate index to a caller-supplied index file. Used by the cross-instance roundtrip tests; the
    /// production flush path goes through [`super::Flushable::flush`].
    #[cfg(test)]
    pub(crate) fn persist_to(&self, file: &IndexFile) -> AssetsResult<()> {
        write_record(file, &snapshot_aggregate(&self.inner.state), false)
    }
}

impl InnerIndex {
    /// Force the detached cohort before publishing its selected availability.
    /// Only a missing file whose key has already left both the selected and
    /// live aggregate is a completed deletion; every other barrier error
    /// preserves the obligations and prevents manifest publication.
    fn barrier_pending_files(&self, index: &AvailabilityFile) -> AssetsResult<()> {
        let paths: Vec<PathBuf> = self
            .pending_durability
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let queued: Vec<_> = paths
            .into_iter()
            .filter_map(|path| self.pending_durability.remove(&path))
            .collect();
        for (path, key) in &queued {
            let (root, relative) = AvailabilityIndex::resolve_refs(key);
            let selected = index
                .assets
                .get(root)
                .and_then(|asset| asset.resources.get(relative));
            let synced = OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|file| sync_selected_file(file, path, selected));
            if let Err(error) = synced {
                let present = self
                    .assets
                    .load()
                    .get(root)
                    .is_some_and(|asset| asset.contains_key(relative));
                if error.kind() == ErrorKind::NotFound && selected.is_none() && !present {
                    continue;
                }
                for (path, key) in queued {
                    self.pending_durability.entry(path).or_insert(key);
                }
                return Err(error.into());
            }
        }
        Ok(())
    }

    /// Committed files are forced onto the medium before the manifest names them; reversed, a crash
    /// could leave the manifest vouching for bytes that never landed.
    pub(super) fn write_manifest(&self, file: &IndexFile, durable: bool) -> AssetsResult<()> {
        let index = snapshot_aggregate(self);
        self.barrier_pending_files(&index)?;
        write_record(file, &index, durable)
    }
}

/// Force the opened file, then verify the canonical path still names those
/// bytes at the extent selected for this manifest. A rewrite may replace the
/// path or shrink its payload while a flush is holding an older snapshot.
fn sync_selected_file(
    file: File,
    path: &Path,
    selected: Option<&ResourceAvailabilityFile>,
) -> io::Result<()> {
    let synced = Handle::from_file(file)?;
    synced.as_file().sync_data()?;
    let canonical = Handle::from_path(path)?;
    let synced_len = synced.as_file().metadata()?.len();
    let canonical_len = canonical.as_file().metadata()?.len();
    let extent_matches = selected.is_none_or(|record| {
        record.final_len == Some(synced_len)
            && record.ranges.iter().all(|(_, end)| *end <= synced_len)
    });
    if synced != canonical || synced_len != canonical_len || !extent_matches {
        return Err(io::Error::other(format!(
            "availability file changed during durability barrier: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Freeze committed values before taking their durability cohort. Tree
/// guards alone are insufficient: each entry still holds a live `ArcSwap`.
fn snapshot_aggregate(inner: &InnerIndex) -> AvailabilityFile {
    let tree = inner.assets.load();
    let assets = tree
        .iter()
        .filter_map(|(root, asset)| {
            let resources: BTreeMap<String, ResourceAvailabilityFile> = asset
                .iter()
                .filter_map(|(path, entry)| {
                    let avail = entry.load();
                    if !avail.committed {
                        return None;
                    }
                    let record = ResourceAvailabilityFile {
                        ranges: avail.ranges.iter().map(|r| (r.start, r.end)).collect(),
                        final_len: avail.final_len,
                        is_committed: avail.committed,
                    };
                    Some((path.clone(), record))
                })
                .collect();
            if resources.is_empty() {
                return None;
            }
            Some((root.clone(), AssetAvailabilityFile { resources }))
        })
        .collect();
    AvailabilityFile { assets, version: 1 }
}

#[cfg(test)]
mod tests {
    use std::{fs, ops::Range};

    use kithara_platform::CancelToken;
    use kithara_storage::{
        AtomicChunked, AvailabilityObserver, MmapDriver, MmapOptions, OpenIntent, OpenMode,
        Resource,
    };
    use kithara_test_utils::kithara;
    use tempfile::TempDir;

    use super::*;
    use crate::{error::AssetsError, index::ScopedAvailabilityObserver, layout::ResourceKey};

    struct CheckpointOnCommit {
        observer: Arc<ScopedAvailabilityObserver>,
        index: AvailabilityIndex,
        canonical: PathBuf,
    }

    impl AvailabilityObserver for CheckpointOnCommit {
        fn on_commit(&self, final_len: u64) {
            self.observer.on_commit(final_len);
            let flushed = self.index.flush();
            assert!(
                self.canonical.is_file(),
                "a commit observer may checkpoint only after the canonical file exists"
            );
            flushed.unwrap();
        }

        fn on_write(&self, range: Range<u64>) {
            self.observer.on_write(range);
        }
    }

    #[kithara::test]
    fn atomic_commit_publishes_canonical_file_before_manifest() {
        let dir = TempDir::new().unwrap();
        let pools = crate::test_pools::pools();
        let manifest = dir.path().join("availability.bin");
        let canonical = dir.path().join("asset/segment.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        index.enable_persistence(manifest.clone(), crate::test_pools::byte_buffer(&pools));
        let observer: Arc<dyn AvailabilityObserver> = Arc::new(CheckpointOnCommit {
            observer: ScopedAvailabilityObserver::for_file(
                key.clone(),
                index.clone(),
                canonical.clone(),
            ),
            index: index.clone(),
            canonical: canonical.clone(),
        });
        let resource =
            AtomicChunked::<MmapDriver>::open_deferred(canonical.clone(), move |target, intent| {
                let mode = match intent {
                    OpenIntent::Fresh => OpenMode::ReadWrite,
                    OpenIntent::Reopen => OpenMode::ReadOnly,
                };
                Resource::open_with_observer(
                    CancelToken::never(),
                    MmapOptions::for_path(target.to_path_buf())
                        .mode(mode)
                        .build(),
                    Some(Arc::clone(&observer)),
                )
            })
            .unwrap();
        resource.write_at(0, b"payload").unwrap();
        assert!(!canonical.exists());

        resource.commit(Some(7)).unwrap();

        assert_eq!(fs::read(&canonical).unwrap(), b"payload");
        let restored = AvailabilityIndex::new();
        restored
            .load_from(
                &IndexFile::new(manifest),
                &mut crate::test_pools::byte_buffer(&pools),
            )
            .unwrap();
        assert_eq!(restored.final_len(&key), Some(7));
        assert!(restored.contains_range(&key, 0..7));
    }

    #[kithara::test]
    #[case::missing(false)]
    #[case::directory(true)]
    fn a_failed_file_barrier_preserves_the_previous_manifest(#[case] directory: bool) {
        let dir = TempDir::new().unwrap();
        let pools = crate::test_pools::pools();
        let manifest = dir.path().join("availability.bin");
        let canonical = dir.path().join("segment.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        index.enable_persistence(manifest.clone(), crate::test_pools::byte_buffer(&pools));
        index.record_commit(&ResourceKey::relative("asset", "already-durable.bin"), 3);
        index.flush().unwrap();
        let previous = fs::read(&manifest).unwrap();
        if directory {
            fs::create_dir(&canonical).unwrap();
        }
        let observer =
            ScopedAvailabilityObserver::for_file(key.clone(), index.clone(), canonical.clone());
        observer.on_commit(7);

        assert!(matches!(index.flush(), Err(AssetsError::Io(_))));
        assert_eq!(fs::read(&manifest).unwrap(), previous);
        assert!(matches!(index.flush(), Err(AssetsError::Io(_))));
        if directory {
            fs::remove_dir(&canonical).unwrap();
        }
        fs::write(&canonical, b"payload").unwrap();
        index.flush().unwrap();

        let restored = AvailabilityIndex::new();
        restored
            .load_from(
                &IndexFile::new(manifest),
                &mut crate::test_pools::byte_buffer(&pools),
            )
            .unwrap();
        assert_eq!(restored.final_len(&key), Some(7));
    }

    #[kithara::test]
    fn a_deleted_resource_has_no_manifest_barrier_obligation() {
        let dir = TempDir::new().unwrap();
        let pools = crate::test_pools::pools();
        let manifest = dir.path().join("availability.bin");
        let canonical = dir.path().join("segment.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        index.enable_persistence(manifest.clone(), crate::test_pools::byte_buffer(&pools));
        fs::write(&canonical, b"payload").unwrap();
        ScopedAvailabilityObserver::for_file(key.clone(), index.clone(), canonical.clone())
            .on_commit(7);
        fs::remove_file(&canonical).unwrap();
        index.remove(&key);

        index.flush().unwrap();

        let restored = AvailabilityIndex::new();
        restored
            .load_from(
                &IndexFile::new(manifest),
                &mut crate::test_pools::byte_buffer(&pools),
            )
            .unwrap();
        assert_eq!(restored.final_len(&key), None);
    }

    #[kithara::test]
    fn every_same_size_file_commit_requires_a_durability_barrier() {
        let dir = TempDir::new().unwrap();
        let pools = crate::test_pools::pools();
        let canonical = dir.path().join("segment.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        index.enable_persistence(
            dir.path().join("availability.bin"),
            crate::test_pools::byte_buffer(&pools),
        );
        let observer = ScopedAvailabilityObserver::for_file(key, index.clone(), canonical.clone());
        fs::write(&canonical, b"first").unwrap();
        observer.on_commit(5);
        index.flush().unwrap();

        fs::write(&canonical, b"later").unwrap();
        observer.on_commit(5);
        fs::remove_file(&canonical).unwrap();

        assert!(matches!(index.flush(), Err(AssetsError::Io(_))));
    }

    fn open_observed_resource(
        path: PathBuf,
        index: AvailabilityIndex,
        key: ResourceKey,
    ) -> AtomicChunked<MmapDriver> {
        let observer: Arc<dyn AvailabilityObserver> =
            ScopedAvailabilityObserver::for_file(key, index, path.clone());
        AtomicChunked::open_deferred(path, move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open_with_observer(
                CancelToken::never(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .build(),
                Some(Arc::clone(&observer)),
            )
        })
        .unwrap()
    }

    #[kithara::test]
    #[case::same_size(7)]
    #[case::shorter(3)]
    fn a_file_barrier_rejects_a_replaced_handle_even_at_the_same_length(#[case] final_len: usize) {
        let dir = TempDir::new().unwrap();
        let canonical = dir.path().join("segment.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        let resource = open_observed_resource(canonical.clone(), index.clone(), key);
        resource.write_at(0, b"payload").unwrap();
        resource.commit(Some(7)).unwrap();
        let selected = snapshot_aggregate(&index.inner.state);
        let opened = OpenOptions::new().write(true).open(&canonical).unwrap();

        resource.reactivate().unwrap();
        let replacement = vec![0xAB; final_len];
        resource.write_at(0, &replacement).unwrap();
        resource.commit(Some(final_len as u64)).unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), replacement);

        let record = &selected.assets["asset"].resources["segment.bin"];
        assert_eq!(record.final_len, Some(7));
        assert!(sync_selected_file(opened, &canonical, Some(record)).is_err());
    }

    #[kithara::test]
    #[case::empty(0)]
    #[case::shorter(3)]
    fn a_frozen_manifest_rejects_a_shrunk_resource_and_next_flush_records_its_extent(
        #[case] final_len: usize,
    ) {
        let dir = TempDir::new().unwrap();
        let pools = crate::test_pools::pools();
        let canonical = dir.path().join("segment.bin");
        let manifest = dir.path().join("availability.bin");
        let key = ResourceKey::relative("asset", "segment.bin");
        let index = AvailabilityIndex::new();
        index.enable_persistence(manifest.clone(), crate::test_pools::byte_buffer(&pools));
        let resource = open_observed_resource(canonical.clone(), index.clone(), key.clone());
        resource.write_at(0, b"payload").unwrap();
        resource.commit(Some(7)).unwrap();
        index.flush().unwrap();
        let selected = snapshot_aggregate(&index.inner.state);
        let previous = fs::read(&manifest).unwrap();

        resource.reactivate().unwrap();
        resource.write_at(0, &vec![0xAB; final_len]).unwrap();
        resource.commit(Some(final_len as u64)).unwrap();
        assert_eq!(fs::metadata(&canonical).unwrap().len(), final_len as u64);

        assert!(index.inner.state.barrier_pending_files(&selected).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), previous);
        index.flush().unwrap();

        let restored = AvailabilityIndex::new();
        restored
            .load_from(
                &IndexFile::new(manifest),
                &mut crate::test_pools::byte_buffer(&pools),
            )
            .unwrap();
        assert_eq!(restored.final_len(&key), Some(final_len as u64));
        assert!(restored.contains_range(&key, 0..final_len as u64));
        assert!(!restored.contains_range(&key, final_len as u64..7));
        assert!(
            restored
                .available_ranges(&key)
                .iter()
                .all(|range| range.end <= final_len as u64)
        );
    }
}
