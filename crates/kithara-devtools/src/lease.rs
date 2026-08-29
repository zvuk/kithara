use std::{
    fs::{self, OpenOptions},
    path::{self, Path},
};

use crate::lock::FileLock;

/// The file a process locks for as long as it works in a build directory, so a
/// reclaim running elsewhere can tell "in use" from "left behind".
///
/// A build directory needs protecting for longer than it is being written to.
/// A stress run that had finished compiling and was merely executing its own
/// binaries looked idle, the host's build-cache budget deleted them out from
/// under it, and the repetitions that then failed to exec read as the product
/// breaking rather than the CI eating itself.
pub const FILE: &str = ".kithara-job-lease";

/// A live claim on a build directory, released when this drops or the process
/// dies.
pub struct Lease {
    _lock: FileLock,
}

/// Claims `directory` until the returned guard drops.
///
/// The lock is shared, so several holders coexist: a run and the harness
/// invocations it spawns. A reclaim asks for the same file exclusively, which
/// is the only request a shared holder refuses, and backs off while one is
/// alive.
///
/// Returns `None` when the claim cannot be made. A lease that cannot be taken
/// only restores the behaviour from before there was one, which is no reason to
/// refuse the work it would have protected.
///
/// The result has to be bound: dropping it on the spot releases the claim
/// immediately, which reads at the call site as holding one.
#[must_use]
pub fn hold(directory: &Path) -> Option<Lease> {
    // The directory a lane builds into does not exist before the build starts,
    // and the build is the longest stretch that needs the claim.
    fs::create_dir_all(directory).ok()?;
    // Absolute, because a reclaim asks about an absolute path. A relative
    // `CARGO_TARGET_DIR` would claim a file under whatever directory the lane
    // happened to start in, and a claim the reclaim never reads protects
    // nothing.
    let directory = path::absolute(directory).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(FILE))
        .ok()?;
    let lock = FileLock::try_shared(file).ok()?;
    Some(Lease { _lock: lock })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{FILE, hold};

    #[test]
    fn a_lease_names_a_directory_that_did_not_exist_yet() {
        let temp = TempDir::new().unwrap();
        let build = temp.path().join("target-stress");

        let lease = hold(&build).expect("claim a build directory before it is built into");

        assert!(build.join(FILE).is_file());
        drop(lease);
    }

    #[test]
    fn two_holders_of_one_directory_coexist() {
        let temp = TempDir::new().unwrap();

        let first = hold(temp.path()).expect("first holder");
        let second = hold(temp.path()).expect("a spawned harness run must not be refused");

        drop((first, second));
    }
}
