use crate::{CachePath, CacheResult, PutResult, store::Store};
use kithara_core::AssetId;
use std::path::PathBuf;

/// Base filesystem store implementation.
/// Provides tree-friendly layout and atomic write operations.
/// This layer doesn't know about eviction, leases, or indexing.
#[derive(Clone, Debug)]
pub struct FsStore {
    root_dir: PathBuf,
}

impl FsStore {
    pub fn new(root_dir: PathBuf) -> CacheResult<Self> {
        std::fs::create_dir_all(&root_dir)?;
        Ok(FsStore { root_dir })
    }

    fn asset_dir(&self, asset_id: AssetId) -> PathBuf {
        let asset_key = hex::encode(asset_id.as_bytes());
        self.root_dir.join(&asset_key[0..2]).join(&asset_key[2..4])
    }

    fn temp_file(&self, asset_id: AssetId, rel_path: &CachePath) -> PathBuf {
        let asset_dir = self.asset_dir(asset_id);
        asset_dir.join(format!("{}.tmp", rel_path.as_string()))
    }

    fn final_file(&self, asset_id: AssetId, rel_path: &CachePath) -> PathBuf {
        let asset_dir = self.asset_dir(asset_id);
        asset_dir.join(rel_path.as_path_buf())
    }
}

impl Store for FsStore {
    fn exists(&self, asset: AssetId, rel_path: &CachePath) -> bool {
        self.final_file(asset, rel_path).exists()
    }

    fn open(&self, asset: AssetId, rel_path: &CachePath) -> CacheResult<Option<std::fs::File>> {
        let path = self.final_file(asset, rel_path);
        if path.exists() {
            Ok(Some(std::fs::File::open(path)?))
        } else {
            Ok(None)
        }
    }

    fn put_atomic(
        &self,
        asset: AssetId,
        rel_path: &CachePath,
        bytes: &[u8],
    ) -> CacheResult<PutResult> {
        let asset_dir = self.asset_dir(asset);
        std::fs::create_dir_all(&asset_dir)?;

        let temp_path = self.temp_file(asset, rel_path);
        let final_path = self.final_file(asset, rel_path);

        std::fs::write(&temp_path, bytes)?;
        std::fs::rename(&temp_path, &final_path)?;

        Ok(PutResult {
            bytes_written: bytes.len() as u64,
        })
    }

    fn remove_all(&self, asset: AssetId) -> CacheResult<()> {
        let asset_dir = self.asset_dir(asset);
        if asset_dir.exists() {
            std::fs::remove_dir_all(&asset_dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kithara_core::AssetId;
    use std::env;

    fn create_temp_store() -> FsStore {
        let temp_dir =
            env::temp_dir().join(format!("kithara-fsstore-test-{}", uuid::Uuid::new_v4()));
        FsStore::new(temp_dir).unwrap()
    }

    #[test]
    fn fsstore_put_atomic_creates_file() {
        let store = create_temp_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap()).unwrap();
        let path = CachePath::from_single("test.txt").unwrap();
        let data = b"test data";

        let result = store.put_atomic(asset_id, &path, data).unwrap();
        assert_eq!(result.bytes_written, data.len() as u64);
        assert!(store.exists(asset_id, &path));

        let mut file = store.open(asset_id, &path).unwrap().unwrap();
        let mut content = String::new();
        use std::io::Read;
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "test data");
    }

    #[test]
    fn fsstore_put_atomic_overwrites_existing() {
        let store = create_temp_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap()).unwrap();
        let path = CachePath::from_single("test.txt").unwrap();

        store.put_atomic(asset_id, &path, b"original").unwrap();
        store.put_atomic(asset_id, &path, b"replaced").unwrap();

        let mut file = store.open(asset_id, &path).unwrap().unwrap();
        let mut content = String::new();
        use std::io::Read;
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "replaced");
    }

    #[test]
    fn fsstore_temp_files_not_visible_as_hits() {
        let store = create_temp_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap()).unwrap();
        let path = CachePath::from_single("test.txt").unwrap();
        let data = b"test data";

        let temp_path = store.temp_file(asset_id, &path);
        let final_path = store.final_file(asset_id, &path);

        // Before put: neither temp nor final should exist
        assert!(!temp_path.exists());
        assert!(!final_path.exists());
        assert!(!store.exists(asset_id, &path));

        // After put: only final should exist, temp should be gone
        store.put_atomic(asset_id, &path, data).unwrap();
        assert!(!temp_path.exists());
        assert!(final_path.exists());
        assert!(store.exists(asset_id, &path));
    }

    #[test]
    fn fsstore_asset_dir_layout_is_stable() {
        let store = create_temp_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap()).unwrap();

        let dir1 = store.asset_dir(asset_id);
        let dir2 = store.asset_dir(asset_id);

        assert_eq!(dir1, dir2);
        assert!(
            dir1.to_string_lossy()
                .contains(&format!("{:x}", asset_id.as_bytes()[0]))
        );
        assert!(
            dir1.to_string_lossy()
                .contains(&format!("{:x}", asset_id.as_bytes()[1]))
        );
    }

    #[test]
    fn fsstore_remove_all_deletes_asset_directory() {
        let store = create_temp_store();
        let asset_id =
            AssetId::from_url(&url::Url::parse("https://example.com/test.mp3").unwrap()).unwrap();
        let path = CachePath::from_single("test.txt").unwrap();

        // Create some files
        store.put_atomic(asset_id, &path, b"data").unwrap();
        assert!(store.exists(asset_id, &path));

        // Remove all
        store.remove_all(asset_id).unwrap();
        assert!(!store.exists(asset_id, &path));
    }
}
