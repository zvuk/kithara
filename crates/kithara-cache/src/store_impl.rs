use crate::{
    AssetState, CachePath, CacheResult, CacheState, PutResult, lease::PinStore, store::Store,
};
use kithara_core::AssetId;
use std::path::PathBuf;

/// Indexing decorator that maintains state.json with metadata.
/// Provides total_bytes tracking and per-asset metadata.
/// FS remains the source of truth for file existence.
#[derive(Clone, Debug)]
pub struct IndexStore<S> {
    inner: S,
    root_dir: PathBuf,
    max_bytes: u64,
}

impl<S> IndexStore<S>
where
    S: Store,
{
    pub fn new(inner: S, root_dir: PathBuf, max_bytes: u64) -> Self {
        IndexStore {
            inner,
            root_dir,
            max_bytes,
        }
    }

    fn state_file(&self) -> PathBuf {
        self.root_dir.join("state.json")
    }

    fn load_state(&self) -> CacheResult<CacheState> {
        let state_file = self.state_file();
        if !state_file.exists() {
            return Ok(CacheState {
                max_bytes: self.max_bytes,
                total_bytes: 0,
                assets: std::collections::HashMap::new(),
            });
        }

        let content = std::fs::read_to_string(&state_file)?;
        let state: CacheState = serde_json::from_str(&content)?;
        Ok(state)
    }

    fn save_state(&self, state: &CacheState) -> CacheResult<()> {
        let temp_file = self.state_file().with_extension("tmp");
        let content = serde_json::to_string_pretty(state)?;

        std::fs::write(&temp_file, content)?;
        std::fs::rename(&temp_file, self.state_file())?;
        Ok(())
    }

    fn sync_with_fs(&self, state: &mut CacheState) -> CacheResult<()> {
        let mut total_bytes = 0u64;
        let mut found_assets = std::collections::HashSet::new();

        // Scan filesystem to find actual assets
        for entry in std::fs::read_dir(&self.root_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            // Check if this looks like an asset directory (hex naming)
            if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                if dir_name.len() == 2 && dir_name.chars().all(|c| c.is_ascii_hexdigit()) {
                    // This is a 2-char prefix directory
                    for subdir_entry in std::fs::read_dir(&path)? {
                        let subdir_entry = subdir_entry?;
                        let subdir_path = subdir_entry.path();

                        if !subdir_path.is_dir() {
                            continue;
                        }

                        if let Some(subdir_name) = subdir_path.file_name().and_then(|n| n.to_str())
                        {
                            if subdir_name.len() == 2
                                && subdir_name.chars().all(|c| c.is_ascii_hexdigit())
                            {
                                // This is the asset directory
                                let asset_key = format!("{}{}", dir_name, subdir_name);
                                found_assets.insert(asset_key.clone());

                                // Calculate total size of this asset
                                let mut asset_size = 0u64;
                                for file_entry in std::fs::read_dir(&subdir_path)? {
                                    let file_entry = file_entry?;
                                    let file_path = file_entry.path();
                                    if file_path.is_file() {
                                        asset_size += file_entry.metadata()?.len();
                                    }
                                }

                                total_bytes += asset_size;

                                // Update or create asset state
                                let asset_state = state
                                    .assets
                                    .entry(asset_key.clone())
                                    .or_insert_with(|| AssetState {
                                        size_bytes: 0,
                                        last_access_ms: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_millis()
                                            as u64,
                                        created_ms: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_millis()
                                            as u64,
                                        pinned: false,
                                    });

                                // Update size from filesystem (source of truth)
                                asset_state.size_bytes = asset_size;
                            }
                        }
                    }
                }
            }
        }

        // Remove assets that no longer exist on filesystem
        state.assets.retain(|key, _| found_assets.contains(key));
        state.total_bytes = total_bytes;

        Ok(())
    }

    pub fn get_total_bytes(&self) -> CacheResult<u64> {
        let state = self.load_state()?;
        Ok(state.total_bytes)
    }

    pub fn get_asset_state(&self, asset: AssetId) -> CacheResult<Option<AssetState>> {
        let asset_key = hex::encode(asset.as_bytes());
        let state = self.load_state()?;
        Ok(state.assets.get(&asset_key).cloned())
    }

    pub fn touch(&self, asset: AssetId) -> CacheResult<()> {
        let mut state = self.load_state()?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let asset_key = hex::encode(asset.as_bytes());
        if let Some(asset_state) = state.assets.get_mut(&asset_key) {
            asset_state.last_access_ms = now_ms;
            self.save_state(&state)?;
        }
        Ok(())
    }

    pub fn stats(&self) -> CacheResult<(u64, usize, usize)> {
        let mut state = self.load_state()?;
        self.sync_with_fs(&mut state)?;
        let asset_count = state.assets.len();
        let pinned_assets = state.assets.values().filter(|asset| asset.pinned).count();
        Ok((state.total_bytes, asset_count, pinned_assets))
    }
}

impl<S> PinStore for IndexStore<S>
where
    S: Store,
{
    fn pin(&self, asset: AssetId) -> CacheResult<()> {
        let mut state = self.load_state()?;
        let asset_key = hex::encode(asset.as_bytes());

        let asset_state = state
            .assets
            .entry(asset_key.clone())
            .or_insert_with(|| AssetState {
                size_bytes: 0,
                last_access_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
                created_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
                pinned: false,
            });

        asset_state.pinned = true;
        self.save_state(&state)?;
        Ok(())
    }

    fn unpin(&self, asset: AssetId) -> CacheResult<()> {
        let asset_key = hex::encode(asset.as_bytes());
        if let Ok(mut state) = self.load_state()
            && let Some(asset_state) = state.assets.get_mut(&asset_key)
        {
            asset_state.pinned = false;
            self.save_state(&state)?;
        }
        Ok(())
    }

    fn is_pinned(&self, asset: AssetId) -> CacheResult<bool> {
        let asset_key = hex::encode(asset.as_bytes());
        let state = self.load_state()?;
        Ok(state
            .assets
            .get(&asset_key)
            .map(|s| s.pinned)
            .unwrap_or(false))
    }
}

impl<S> Store for IndexStore<S>
where
    S: Store,
{
    fn exists(&self, asset: AssetId, rel_path: &CachePath) -> bool {
        self.inner.exists(asset, rel_path)
    }

    fn open(&self, asset: AssetId, rel_path: &CachePath) -> CacheResult<Option<std::fs::File>> {
        self.inner.open(asset, rel_path)
    }

    fn put_atomic(
        &self,
        asset: AssetId,
        rel_path: &CachePath,
        bytes: &[u8],
    ) -> CacheResult<PutResult> {
        // Calculate size change
        let old_size = if self.exists(asset, rel_path) {
            if let Ok(Some(file)) = self.open(asset, rel_path) {
                file.metadata()?.len()
            } else {
                0
            }
        } else {
            0
        };

        let result = self.inner.put_atomic(asset, rel_path, bytes)?;
        let size_change = bytes.len() as u64 - old_size;

        // Update state
        let mut state = self.load_state()?;
        let asset_key = hex::encode(asset.as_bytes());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let asset_state = state.assets.entry(asset_key).or_insert_with(|| AssetState {
            size_bytes: 0,
            last_access_ms: now_ms,
            created_ms: now_ms,
            pinned: false,
        });

        asset_state.size_bytes += size_change;
        asset_state.last_access_ms = now_ms;
        state.total_bytes += size_change;

        self.save_state(&state)?;

        Ok(PutResult {
            bytes_written: result.bytes_written,
        })
    }

    fn remove_all(&self, asset: AssetId) -> CacheResult<()> {
        let asset_key = hex::encode(asset.as_bytes());

        // Get current size for state update
        let current_size = {
            let state = self.load_state()?;
            state
                .assets
                .get(&asset_key)
                .map(|s| s.size_bytes)
                .unwrap_or(0)
        };

        self.inner.remove_all(asset)?;

        // Update state
        let mut state = self.load_state()?;
        state.total_bytes -= current_size;
        state.assets.remove(&asset_key);
        self.save_state(&state)?;

        Ok(())
    }
}

impl<S> crate::evicting_store::EvictionSupport for IndexStore<S>
where
    S: Store,
{
    fn get_total_bytes(&self) -> CacheResult<u64> {
        let state = self.load_state()?;
        Ok(state.total_bytes)
    }

    fn get_all_assets(&self) -> CacheResult<Vec<(String, AssetState)>> {
        let state = self.load_state()?;
        let assets: Vec<_> = state.assets.into_iter().collect();

        // Sync with filesystem to ensure accuracy
        let mut temp_state = CacheState {
            max_bytes: state.max_bytes,
            total_bytes: state.total_bytes,
            assets: std::collections::HashMap::new(),
        };
        self.sync_with_fs(&mut temp_state)?;

        Ok(assets)
    }

    fn remove_assets_from_state(&self, keys: &[String]) -> CacheResult<()> {
        let mut state = self.load_state()?;
        let mut total_removed = 0u64;

        for key in keys {
            if let Some(asset_state) = state.assets.get(key) {
                total_removed += asset_state.size_bytes;
            }
            state.assets.remove(key);
        }

        state.total_bytes -= total_removed;
        self.save_state(&state)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::FsStore;
    use std::env;

    fn create_temp_index_store() -> IndexStore<FsStore> {
        let temp_dir =
            env::temp_dir().join(format!("kithara-indexstore-test-{}", uuid::Uuid::new_v4()));
        let fs_store = FsStore::new(temp_dir.clone()).unwrap();
        IndexStore::new(fs_store, temp_dir, 1024 * 1024)
    }

    #[test]
    fn indexstore_creates_state_file_on_first_put() {
        let store = create_temp_index_store();
        let asset_id = AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap());
        let path = CachePath::from_single("test.txt").unwrap();

        // Initially no state file
        assert!(!store.state_file().exists());

        // Put some data
        store.put_atomic(asset_id, &path, b"test data").unwrap();

        // State file should now exist
        assert!(store.state_file().exists());

        // Check state content
        let state = store.load_state().unwrap();
        assert_eq!(state.total_bytes, "test data".len() as u64);
        assert_eq!(state.assets.len(), 1);

        let asset_key = hex::encode(asset_id.as_bytes());
        let asset_state = state.assets.get(&asset_key).unwrap();
        assert_eq!(asset_state.size_bytes, "test data".len() as u64);
    }

    #[test]
    fn indexstore_atomic_state_save_is_crash_safe() {
        let store = create_temp_index_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/atomic.mp3").unwrap());
        let path = CachePath::from_single("atomic.txt").unwrap();

        store.put_atomic(asset_id, &path, b"atomic test").unwrap();

        let state_file = store.state_file();
        let temp_state_file = state_file.with_extension("tmp");

        // State file should exist, temp should not
        assert!(state_file.exists());
        assert!(!temp_state_file.exists());

        // Verify we can load it back
        let loaded_content = std::fs::read_to_string(&state_file).unwrap();
        let parsed_state: CacheState = serde_json::from_str(&loaded_content).unwrap();
        assert!(parsed_state.total_bytes > 0);
    }

    #[test]
    fn indexstore_touch_updates_access_time() {
        let store = create_temp_index_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/touch.mp3").unwrap());
        let path = CachePath::from_single("touch.txt").unwrap();

        // Put data to create asset state
        store.put_atomic(asset_id, &path, b"touch test").unwrap();

        // Get initial state
        let initial_state = store.load_state().unwrap();
        let initial_access = initial_state
            .assets
            .get(&hex::encode(asset_id.as_bytes()))
            .unwrap()
            .last_access_ms;

        // Wait a bit and touch
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.touch(asset_id).unwrap();

        // Verify access time was updated
        let updated_state = store.load_state().unwrap();
        let updated_access = updated_state
            .assets
            .get(&hex::encode(asset_id.as_bytes()))
            .unwrap()
            .last_access_ms;

        assert!(updated_access > initial_access);
    }

    #[test]
    fn indexstore_stats_are_accurate() {
        let store = create_temp_index_store();
        let asset1 = AssetId::from_url(&url::Url::parse("https://example.com/asset1.mp3").unwrap());
        let asset2 = AssetId::from_url(&url::Url::parse("https://example.com/asset2.mp3").unwrap());
        let path = CachePath::from_single("data.txt").unwrap();

        // Add two assets
        store.put_atomic(asset1, &path, &vec![b'a'; 50]).unwrap();
        store.put_atomic(asset2, &path, &vec![b'b'; 30]).unwrap();

        let (total_bytes, asset_count, pinned_assets) = store.stats().unwrap();
        assert_eq!(total_bytes, 80);
        assert_eq!(asset_count, 2);
        assert_eq!(pinned_assets, 0);
    }
}
