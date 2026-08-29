use std::{fs::File, io};

use fs4::{FileExt, TryLockError};

/// A `flock` held on an open file, released when this drops.
///
/// The release is explicit. The lock lives on the open file description, which
/// every process forked while the descriptor was open holds a duplicate of
/// until it execs, so closing frees nothing while one of those is alive.
/// Unlocking acts on the description and is seen by every duplicate at once.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Takes `file`'s shared lock, waiting out an exclusive holder.
    ///
    /// # Errors
    ///
    /// When the lock cannot be taken.
    pub fn shared(file: File) -> io::Result<Self> {
        FileExt::lock_shared(&file)?;
        Ok(Self { file })
    }

    /// Takes `file`'s exclusive lock, waiting out every other holder.
    ///
    /// # Errors
    ///
    /// When the lock cannot be taken.
    pub fn exclusive(file: File) -> io::Result<Self> {
        FileExt::lock(&file)?;
        Ok(Self { file })
    }

    /// Takes `file`'s shared lock unless an exclusive holder has it.
    ///
    /// # Errors
    ///
    /// [`TryLockError::WouldBlock`] when a holder has the file.
    pub fn try_shared(file: File) -> Result<Self, TryLockError> {
        FileExt::try_lock_shared(&file)?;
        Ok(Self { file })
    }

    /// Takes `file`'s exclusive lock unless another holder has it.
    ///
    /// # Errors
    ///
    /// [`TryLockError::WouldBlock`] when a holder has the file.
    pub fn try_exclusive(file: File) -> Result<Self, TryLockError> {
        FileExt::try_lock(&file)?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Taken on this owned descriptor, so nothing is left to report.
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    #[cfg(unix)]
    use std::{
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    use fs4::TryLockError;
    use tempfile::TempDir;

    use super::FileLock;

    fn open(directory: &TempDir) -> File {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.path().join("lock"))
            .expect("open the lock file")
    }

    #[test]
    fn a_holder_refuses_a_second_exclusive_request() {
        let directory = TempDir::new().expect("temporary directory");
        let held = FileLock::try_shared(open(&directory)).expect("take the lock");

        assert!(matches!(
            FileLock::try_exclusive(open(&directory)),
            Err(TryLockError::WouldBlock)
        ));
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn a_released_lock_is_free_while_children_are_being_spawned() {
        let directory = TempDir::new().expect("temporary directory");
        let stop = Arc::new(AtomicBool::new(false));
        let spawner = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let mut child = Command::new("/bin/sh")
                        .args(["-c", "exit 0"])
                        .spawn()
                        .expect("spawn a child");
                    child.wait().expect("reap the child");
                }
            })
        };

        let mut still_held = 0;
        for _ in 0..2_000 {
            match FileLock::try_shared(open(&directory)) {
                Ok(held) => drop(held),
                Err(_) => still_held += 1,
            }
            match FileLock::try_exclusive(open(&directory)) {
                Ok(held) => drop(held),
                Err(_) => still_held += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        spawner.join().expect("join the spawner");

        assert_eq!(
            still_held, 0,
            "a lock nobody holds must not read as held to the next asker"
        );
    }
}
