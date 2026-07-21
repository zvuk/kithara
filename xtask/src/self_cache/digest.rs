use std::{
    fs::{self, ReadDir},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest as _, Sha256};

const BUFFER_SIZE: usize = 64 * 1024;

pub(super) struct TreeDigest {
    entries: u64,
    sums: [u64; 4],
    xors: [u64; 4],
}

impl TreeDigest {
    pub(super) fn new() -> Self {
        Self {
            entries: 0,
            sums: [0; 4],
            xors: [0; 4],
        }
    }

    pub(super) fn add_bytes(&mut self, domain: &[u8], label: &Path, value: &[u8]) {
        let mut hasher = Sha256::new();
        update_field(&mut hasher, domain);
        update_field(&mut hasher, label.as_os_str().as_encoded_bytes());
        update_field(&mut hasher, value);
        self.fold(hasher.finalize().into());
    }

    pub(super) fn add_path(&mut self, path: &Path, label: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("read cache input metadata {}", path.display()))?;
        let mut stack = Vec::new();
        self.add_node(path, label, &metadata, &mut stack)?;
        while let Some(frame) = stack.last_mut() {
            let Some(entry) = frame.entries.next() else {
                stack.pop();
                continue;
            };
            let entry = entry.with_context(|| {
                format!("read cache input directory {}", frame.physical.display())
            })?;
            let physical = entry.path();
            let logical = frame.logical.join(entry.file_name());
            let metadata = fs::symlink_metadata(&physical)
                .with_context(|| format!("read cache input metadata {}", physical.display()))?;
            if excluded(path, &physical, metadata.file_type()) {
                continue;
            }
            self.add_node(&physical, &logical, &metadata, &mut stack)?;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"xtask-self-cache-tree-v1\0");
        hasher.update(self.entries.to_be_bytes());
        for value in self.sums {
            hasher.update(value.to_be_bytes());
        }
        for value in self.xors {
            hasher.update(value.to_be_bytes());
        }
        encode_hex(&hasher.finalize())
    }

    fn add_node(
        &mut self,
        physical: &Path,
        logical: &Path,
        metadata: &fs::Metadata,
        stack: &mut Vec<Frame>,
    ) -> Result<()> {
        let file_type = metadata.file_type();
        if file_type.is_file() {
            self.add_file(physical, logical)
        } else if file_type.is_dir() {
            self.add_bytes(b"directory", logical, b"");
            stack.push(Frame {
                entries: fs::read_dir(physical).with_context(|| {
                    format!("read cache input directory {}", physical.display())
                })?,
                logical: logical.to_path_buf(),
                physical: physical.to_path_buf(),
            });
            Ok(())
        } else if file_type.is_symlink() {
            let target = fs::metadata(physical)
                .with_context(|| format!("resolve cache input symlink {}", physical.display()))?;
            if target.file_type().is_file() {
                self.add_file(physical, logical)
            } else {
                bail!("unsupported cache input symlink: {}", physical.display())
            }
        } else {
            bail!("unsupported cache input type: {}", physical.display())
        }
    }

    fn add_file(&mut self, physical: &Path, logical: &Path) -> Result<()> {
        let file = fs::File::open(physical)
            .with_context(|| format!("read cache input {}", physical.display()))?;
        let mut reader = BufReader::with_capacity(BUFFER_SIZE, file);
        let mut buffer = [0_u8; BUFFER_SIZE];
        let mut hasher = Sha256::new();
        update_field(&mut hasher, b"file");
        update_field(&mut hasher, logical.as_os_str().as_encoded_bytes());
        loop {
            let read = reader
                .read(&mut buffer)
                .with_context(|| format!("read cache input {}", physical.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        self.fold(hasher.finalize().into());
        Ok(())
    }

    fn fold(&mut self, digest: [u8; 32]) {
        const ROTATIONS: [u32; 4] = [7, 20, 33, 46];
        self.entries = self.entries.wrapping_add(1);
        for (index, chunk) in digest.chunks_exact(8).enumerate() {
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(chunk);
            let value = u64::from_be_bytes(bytes);
            self.sums[index] = self.sums[index].wrapping_add(value);
            self.xors[index] ^= value.rotate_left(ROTATIONS[index]);
        }
    }
}

struct Frame {
    entries: ReadDir,
    logical: PathBuf,
    physical: PathBuf,
}

fn update_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn excluded(root: &Path, path: &Path, file_type: fs::FileType) -> bool {
    if path.parent() != Some(root) {
        return false;
    }
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.as_encoded_bytes();
    match name {
        b".git" => file_type.is_dir() || file_type.is_file(),
        b".hg" | b".svn" | b"target" => file_type.is_dir(),
        b".xtask-cache" => file_type.is_file(),
        _ => name.starts_with(b".xtask-cache.") && file_type.is_file(),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
pub(super) fn tree(path: &Path, label: &Path) -> Result<String> {
    let mut digest = TreeDigest::new();
    digest.add_path(path, label)?;
    Ok(digest.finish())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};
    #[cfg(unix)]
    use std::{os::unix::fs::symlink, os::unix::net::UnixListener};

    use anyhow::Result;

    use super::{BUFFER_SIZE, tree};

    const _: () = assert!(BUFFER_SIZE <= 1024 * 1024);

    #[test]
    fn digest_is_independent_of_creation_order() -> Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        fs::create_dir(first.path().join("nested"))?;
        fs::write(first.path().join("alpha"), b"a")?;
        fs::write(first.path().join("nested/beta"), b"b")?;
        fs::create_dir(second.path().join("nested"))?;
        fs::write(second.path().join("nested/beta"), b"b")?;
        fs::write(second.path().join("alpha"), b"a")?;

        assert_eq!(
            tree(first.path(), Path::new("fixture"))?,
            tree(second.path(), Path::new("fixture"))?
        );
        Ok(())
    }

    #[test]
    fn digest_changes_with_file_content() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("source.rs"), b"old")?;
        let before = tree(root.path(), Path::new("fixture"))?;
        fs::write(root.path().join("source.rs"), b"new")?;

        assert_ne!(before, tree(root.path(), Path::new("fixture"))?);
        Ok(())
    }

    #[test]
    fn insertion_deletion_and_rename_change_the_digest() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source.rs");
        fs::write(&source, b"source")?;
        let original = tree(root.path(), Path::new("fixture"))?;

        let added = root.path().join("added.rs");
        fs::write(&added, b"added")?;
        assert_ne!(original, tree(root.path(), Path::new("fixture"))?);
        fs::remove_file(&added)?;
        assert_eq!(original, tree(root.path(), Path::new("fixture"))?);
        fs::rename(&source, root.path().join("renamed.rs"))?;
        assert_ne!(original, tree(root.path(), Path::new("fixture"))?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_content_changes_the_digest() -> Result<()> {
        let root = tempfile::tempdir()?;
        let external = tempfile::NamedTempFile::new()?;
        fs::write(external.path(), b"old")?;
        symlink(external.path(), root.path().join("link"))?;
        let before = tree(root.path(), Path::new("fixture"))?;

        fs::write(external.path(), b"changed")?;

        assert_ne!(before, tree(root.path(), Path::new("fixture"))?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_rejected() -> Result<()> {
        let root = tempfile::tempdir()?;
        let external = tempfile::tempdir()?;
        symlink(external.path(), root.path().join("link"))?;

        assert!(tree(root.path(), Path::new("fixture")).is_err());
        Ok(())
    }

    #[test]
    fn generated_paths_are_excluded_only_at_the_input_root() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("target"))?;
        fs::write(root.path().join("target/generated"), b"old")?;
        fs::write(root.path().join(".git"), b"old")?;
        fs::write(root.path().join(".xtask-cache"), b"old")?;
        fs::write(root.path().join(".xtask-cache.temporary"), b"old")?;
        fs::create_dir_all(root.path().join("nested/target"))?;
        fs::write(root.path().join("nested/target/source.rs"), b"old")?;
        fs::write(root.path().join("nested/.xtask-cache"), b"old")?;
        fs::write(root.path().join("nested/.xtask-cache.temporary"), b"old")?;
        let before = tree(root.path(), Path::new("fixture"))?;

        fs::write(root.path().join("target/generated"), b"changed")?;
        fs::write(root.path().join(".git"), b"changed")?;
        fs::write(root.path().join(".xtask-cache"), b"changed")?;
        fs::write(root.path().join(".xtask-cache.temporary"), b"changed")?;
        assert_eq!(before, tree(root.path(), Path::new("fixture"))?);

        fs::write(root.path().join("nested/target/source.rs"), b"changed")?;
        assert_ne!(before, tree(root.path(), Path::new("fixture"))?);
        fs::write(root.path().join("nested/target/source.rs"), b"old")?;
        fs::write(root.path().join("nested/.xtask-cache"), b"changed")?;
        assert_ne!(before, tree(root.path(), Path::new("fixture"))?);
        fs::write(root.path().join("nested/.xtask-cache"), b"old")?;
        fs::write(
            root.path().join("nested/.xtask-cache.temporary"),
            b"changed",
        )?;
        assert_ne!(before, tree(root.path(), Path::new("fixture"))?);
        Ok(())
    }

    #[test]
    fn generated_names_with_unexpected_types_are_hashed() -> Result<()> {
        let target = tempfile::tempdir()?;
        fs::write(target.path().join("target"), b"old")?;
        let before = tree(target.path(), Path::new("fixture"))?;
        fs::write(target.path().join("target"), b"changed")?;
        assert_ne!(before, tree(target.path(), Path::new("fixture"))?);

        let locator = tempfile::tempdir()?;
        fs::create_dir(locator.path().join(".xtask-cache"))?;
        fs::write(locator.path().join(".xtask-cache/source"), b"old")?;
        let before = tree(locator.path(), Path::new("fixture"))?;
        fs::write(locator.path().join(".xtask-cache/source"), b"changed")?;
        assert_ne!(before, tree(locator.path(), Path::new("fixture"))?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn generated_names_do_not_hide_special_files() -> Result<()> {
        for name in [
            ".git",
            ".hg",
            ".svn",
            "target",
            ".xtask-cache",
            ".xtask-cache.temporary",
        ] {
            let root = tempfile::tempdir()?;
            let _listener = UnixListener::bind(root.path().join(name))?;

            assert!(tree(root.path(), Path::new("fixture")).is_err());
        }
        Ok(())
    }
}
