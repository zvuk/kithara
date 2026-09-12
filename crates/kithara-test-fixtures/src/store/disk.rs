use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Absolute store root, required at build time and optional as a runtime override.
pub const STORE_ENV: &str = "KITHARA_FIXTURE_CACHE";

/// Leading digest bytes kept in an asset id. 128 bits: short enough to read in
/// a path, wide enough that the build script's collision check never fires.
const ASSET_ID_BYTES: usize = 16;

/// Explicit fixture cache revision, shared by build-time and integration assets.
pub const CACHE_VERSION: &str = include_str!("../../cache-version");

/// Stable identity of one asset case: `sha2-256(func || 0x00 || case)`.
///
/// The pair is unique by construction — every accessor lands in one flat
/// module, so two cases sharing both halves could not coexist there.
#[must_use]
pub fn asset_id(func: &str, case: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(func.as_bytes());
    hasher.update([0u8]);
    hasher.update(case.as_bytes());
    hex::encode(&hasher.finalize()[..ASSET_ID_BYTES])
}

/// Reads the explicitly configured persistent store root.
///
/// # Errors
///
/// Returns an error when the parameter is missing or is not an absolute path.
pub fn root_from_env() -> io::Result<PathBuf> {
    let root = std::env::var_os(STORE_ENV).map(PathBuf::from);
    root.filter(|path| path.is_absolute()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("set {STORE_ENV} to an absolute persistent directory shared by worktrees"),
        )
    })
}

/// Select the runtime store once, using the build root when no override is set.
///
/// # Errors
/// Returns an error if an explicit override is not an absolute path.
pub fn runtime_root() -> io::Result<&'static Path> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    if let Some(root) = ROOT.get() {
        return Ok(root);
    }
    let root = if std::env::var_os(STORE_ENV).is_some() {
        root_from_env()?
    } else {
        PathBuf::from(env!("KITHARA_FIXTURE_CACHE"))
    };
    Ok(ROOT.get_or_init(|| root))
}

/// Exact store files selected for relocation to another executor.
#[derive(Debug, Serialize)]
pub struct StoreManifest {
    /// Absolute runtime store root selected by this process.
    pub root: PathBuf,
    /// Explicit revision containing the selected files.
    pub revision: &'static str,
    /// Sorted files, excluding producer locks and temporary writes.
    pub files: Vec<StoreFile>,
}

/// One immutable file's content identity and root-relative location.
#[derive(Debug, Serialize)]
pub struct StoreFile {
    /// Path relative to `StoreManifest::root`, including its revision.
    pub path: PathBuf,
    /// Lowercase SHA-256 digest of the complete file.
    pub sha256: String,
    /// Number of bytes covered by the digest.
    pub bytes: u64,
}

/// Inventory the current namespace, including nested HLS resources.
///
/// # Errors
/// Returns an error for unreadable files, symlinks, non-file entries or traversal.
pub fn manifest() -> io::Result<StoreManifest> {
    manifest_at(runtime_root()?)
}

fn manifest_at(root: &Path) -> io::Result<StoreManifest> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture root must be absolute without traversal",
        ));
    }
    let revision = CACHE_VERSION.trim();
    let revision_path = Path::new(revision);
    if revision_path.components().count() != 1
        || !matches!(
            revision_path.components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture revision must be one path component",
        ));
    }
    let namespace = namespace(root, revision);
    for directory in [root, namespace.as_path()] {
        if !fs::symlink_metadata(directory)?.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "fixture directory is not a real directory: {}",
                    directory.display()
                ),
            ));
        }
    }
    let mut files = Vec::new();
    collect_files(root, &namespace, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(StoreManifest {
        root: root.to_path_buf(),
        revision,
        files,
    })
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<StoreFile>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "fixture filename is not UTF-8")
            })?;
            if name.ends_with(".lock") || name.contains(".tmp.") {
                continue;
            }
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            if !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixture path escapes its store",
                ));
            }
            let mut input = File::open(&path)?;
            let mut digest = Sha256::new();
            let mut bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
                bytes += count as u64;
            }
            files.push(StoreFile {
                path: relative.to_path_buf(),
                sha256: hex::encode(digest.finalize()),
                bytes,
            });
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "fixture entry is not a regular file or directory: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Directory holding every entry of one explicit cache revision.
#[must_use]
pub fn namespace(root: &Path, fingerprint: &str) -> PathBuf {
    root.join(fingerprint)
}

/// Path of one single-file entry inside a namespace.
#[must_use]
pub fn entry_path(namespace: &Path, id: &str, ext: &str) -> PathBuf {
    namespace.join(format!("{id}.{ext}"))
}

/// Whether a complete entry is already present without reading its contents.
#[must_use]
pub fn has_entry(namespace: &Path, id: &str, ext: &str) -> bool {
    fs::metadata(entry_path(namespace, id, ext))
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() != 0)
}

/// Reads one entry. An empty file counts as absent — a half-written entry must
/// never be served.
#[must_use]
pub fn read_entry(namespace: &Path, id: &str, ext: &str) -> Option<Vec<u8>> {
    let bytes = fs::read(entry_path(namespace, id, ext)).ok()?;
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Writes one entry atomically: temporary file, `sync_all`, rename.
///
/// # Errors
///
/// Returns the underlying error when the namespace cannot be created or the
/// bytes cannot be written, synced, or renamed into place.
pub fn write_entry(namespace: &Path, id: &str, ext: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    fs::create_dir_all(namespace)?;
    let tmp_path = namespace.join(format!("{id}.{ext}.tmp.{}", std::process::id()));
    let write = (|| -> io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write {
        drop(fs::remove_file(&tmp_path));
        return Err(error);
    }
    let final_path = entry_path(namespace, id, ext);
    fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

/// Held while one entry is produced; serializes producers across processes.
#[must_use = "the lock must be held while the entry is produced"]
pub struct EntryLock {
    _file: File,
}

/// Takes the exclusive lock for one entry, creating the namespace if needed.
///
/// Callers must re-check the entry after acquiring the lock: a second producer
/// may have finished it while this one waited.
///
/// # Errors
///
/// Returns the underlying error when the namespace or the lock file cannot be
/// created, or when the lock cannot be taken.
pub fn lock_entry(namespace: &Path, id: &str) -> io::Result<EntryLock> {
    fs::create_dir_all(namespace)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(namespace.join(format!("{id}.lock")))?;
    fs4::FileExt::lock(&file)?;
    Ok(EntryLock { _file: file })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use tempfile::TempDir;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn id_is_stable_and_separates_case_from_function() {
        let a = asset_id("sine_wav", "a440_6s");
        let same = asset_id("sine_wav", "a440_6s");
        let other_case = asset_id("sine_wav", "a440_2s");
        let other_func = asset_id("sine_mp3", "a440_6s");
        let swapped = asset_id("a440_6s", "sine_wav");

        assert_eq!(a, same);
        assert_ne!(a, other_case);
        assert_ne!(a, other_func);
        assert_ne!(a, swapped, "the separator must keep the halves apart");
        assert_eq!(a.len(), ASSET_ID_BYTES * 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[kithara::test(native, flash(false))]
    fn entry_path_lives_under_the_fingerprint_namespace() {
        let root = Path::new("root");
        let namespace = namespace(root, "0123456789abcdef");
        let path = entry_path(&namespace, "deadbeef", "wav");

        assert!(path.starts_with(root.join("0123456789abcdef")));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("deadbeef.wav"),
        );
    }

    #[kithara::test(native, flash(false))]
    fn roundtrip_write_then_read() {
        let dir = TempDir::new().expect("temp dir");
        let namespace = dir.path().join("fingerprint");

        assert!(read_entry(&namespace, "id", "wav").is_none());
        let written = write_entry(&namespace, "id", "wav", b"payload").expect("write entry");
        assert_eq!(written, entry_path(&namespace, "id", "wav"));
        assert!(has_entry(&namespace, "id", "wav"));
        assert_eq!(
            read_entry(&namespace, "id", "wav").as_deref(),
            Some(b"payload".as_slice()),
        );
    }

    #[kithara::test(native, flash(false))]
    fn empty_entry_is_a_miss() {
        let dir = TempDir::new().expect("temp dir");
        let namespace = dir.path().join("fingerprint");
        write_entry(&namespace, "id", "wav", b"").expect("write empty entry");

        assert!(!has_entry(&namespace, "id", "wav"));
        assert!(read_entry(&namespace, "id", "wav").is_none());
    }

    #[kithara::test(native, flash(false))]
    fn write_leaves_no_temporary_behind() {
        let dir = TempDir::new().expect("temp dir");
        let namespace = dir.path().join("fingerprint");
        write_entry(&namespace, "id", "wav", b"payload").expect("write entry");

        let leftovers: Vec<_> = fs::read_dir(&namespace)
            .expect("read namespace")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect();

        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[kithara::test(native, flash(false))]
    fn entry_lock_is_exclusive_and_released_on_drop() {
        let dir = TempDir::new().expect("temp dir");
        let namespace = dir.path().join("fingerprint");
        let held = lock_entry(&namespace, "id").expect("take entry lock");
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(namespace.join("id.lock"))
            .expect("open the lock file a second time");

        assert!(
            matches!(
                fs4::FileExt::try_lock(&contender),
                Err(fs4::TryLockError::WouldBlock)
            ),
            "the entry lock must exclude a second producer",
        );

        drop(held);
        fs4::FileExt::try_lock(&contender)
            .expect("the entry lock must release with its file handle");
    }

    #[kithara::test(native, flash(false))]
    fn configured_root_is_absolute() {
        let root = root_from_env().expect("configured persistent fixture root");
        assert!(root.is_absolute());
        assert_eq!(Some(root.into_os_string()), std::env::var_os(STORE_ENV));
    }

    #[kithara::test(native, flash(false))]
    fn manifest_covers_nested_payloads_and_rejects_unsafe_paths() {
        let root = TempDir::new().expect("temporary store");
        let namespace = namespace(root.path(), CACHE_VERSION.trim());
        fs::create_dir_all(namespace.join("hls")).expect("HLS directory");
        fs::write(namespace.join("z.wav"), b"abc").expect("fixture body");
        fs::write(namespace.join("hls/segment.m4s"), b"hello").expect("HLS segment");
        fs::write(namespace.join("z.wav.lock"), b"lock").expect("producer lock");
        fs::write(namespace.join("z.wav.tmp.17"), b"partial").expect("temporary write");

        let manifest = manifest_at(root.path()).expect("store manifest");
        assert_eq!(manifest.root, root.path());
        assert_eq!(manifest.revision, CACHE_VERSION.trim());
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(
            manifest.files[0].path,
            Path::new(CACHE_VERSION.trim()).join("hls/segment.m4s")
        );
        assert_eq!(manifest.files[0].bytes, 5);
        assert_eq!(
            manifest.files[0].sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            manifest.files[1].path,
            Path::new(CACHE_VERSION.trim()).join("z.wav")
        );
        assert_eq!(manifest.files[1].bytes, 3);
        assert_eq!(
            manifest.files[1].sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(manifest_at(&root.path().join("..")).is_err());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(namespace.join("z.wav"), namespace.join("redirect"))
                .expect("symlink fixture");
            assert!(manifest_at(root.path()).is_err());
        }
    }
}
