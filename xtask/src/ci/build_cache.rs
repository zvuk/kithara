#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use fs4::TryLockError;
use kithara_devtools::{lease, lock::FileLock};
use tracing::info;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CacheEntry {
    path: PathBuf,
    size_bytes: u64,
    modified: SystemTime,
}

struct CacheContents {
    entries: Vec<CacheEntry>,
    active: bool,
    locks: Vec<FileLock>,
}

struct DirectoryScan {
    bytes: u64,
    active: bool,
    locks: Vec<FileLock>,
}

pub(crate) fn select_evictions(mut entries: Vec<CacheEntry>, budget_bytes: u64) -> Vec<CacheEntry> {
    let mut remaining_bytes: u128 = entries
        .iter()
        .map(|entry| u128::from(entry.size_bytes))
        .sum();
    let budget_bytes = u128::from(budget_bytes);
    if remaining_bytes <= budget_bytes {
        return Vec::new();
    }

    entries.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut evictions = Vec::new();
    for entry in entries {
        if remaining_bytes <= budget_bytes {
            break;
        }
        remaining_bytes = remaining_bytes.saturating_sub(u128::from(entry.size_bytes));
        evictions.push(entry);
    }
    evictions
}

/// The budget is what the host can afford in total, not what one checkout may
/// keep.
///
/// Applied per directory it never fires on a machine that is running out:
/// three checkouts holding 14, 7 and 22 GB were each under a 25 GB budget, so
/// every hourly pass reported `bytes_freed=0` while the volume they share sat
/// at `Aggressive` and jobs were already being refused. A checkout an active
/// job holds cannot be evicted, but the room it occupies is still spent, so it
/// is charged against the ceiling rather than excused from it.
pub(crate) fn enforce_budget(target_dirs: &[PathBuf], budget_bytes: u64) -> Result<()> {
    let (candidates, held_bytes, _locks) = collect(target_dirs, budget_bytes)?;
    evict_to_budget(candidates, held_bytes, budget_bytes)
}

/// Evict until at least `bytes_needed` is gone, whatever the ceiling says.
///
/// A job that cannot fit asks a different question than the hourly pass does.
/// The ceiling answers "may the host keep this much"; caches under it are left
/// alone, which is right for a scheduled sweep and useless for a job standing
/// in front of a full volume — 15.7 GB of caches under a 25 GB ceiling freed
/// nothing while the job needed one more gigabyte and was refused. Here the
/// shortfall is the budget: the oldest entries go until it is covered, or until
/// nothing evictable is left and the caller reports the volume as it is.
pub(crate) fn reclaim_at_least(target_dirs: &[PathBuf], bytes_needed: u64) -> Result<()> {
    let (candidates, held_bytes, _locks) = collect(target_dirs, bytes_needed)?;
    let keep = total_bytes(&candidates).saturating_sub(bytes_needed);
    evict_to_budget(candidates, held_bytes, keep.saturating_add(held_bytes))
}

fn collect(
    target_dirs: &[PathBuf],
    budget_bytes: u64,
) -> Result<(Vec<CacheEntry>, u64, Vec<FileLock>)> {
    let mut target_dirs = target_dirs.to_vec();
    target_dirs.sort();
    let mut candidates = Vec::new();
    let mut held_bytes = 0_u64;
    let mut locks = Vec::new();
    for target_dir in &target_dirs {
        let contents = candidate_entries(target_dir)?;
        locks.extend(contents.locks);
        let bytes = total_bytes(&contents.entries);
        if contents.active {
            held_bytes = held_bytes.saturating_add(bytes);
            info!(
                path = %target_dir.display(),
                bytes_before = bytes,
                bytes_freed = 0,
                budget_bytes,
                "keeping active build cache"
            );
            continue;
        }
        candidates.extend(contents.entries);
    }
    Ok((candidates, held_bytes, locks))
}

fn total_bytes(entries: &[CacheEntry]) -> u64 {
    entries
        .iter()
        .map(|entry| entry.size_bytes)
        .fold(0_u64, u64::saturating_add)
}

fn evict_to_budget(candidates: Vec<CacheEntry>, held_bytes: u64, budget_bytes: u64) -> Result<()> {
    let bytes_before = total_bytes(&candidates).saturating_add(held_bytes);
    let evictions = select_evictions(candidates, budget_bytes.saturating_sub(held_bytes));
    let bytes_freed = total_bytes(&evictions);

    // Cargo fingerprints describe a complete profile tree, so removing files
    // within one can leave its dependency artifacts inconsistent.
    for entry in evictions {
        fs::remove_dir_all(&entry.path)
            .with_context(|| format!("removing build cache entry {}", entry.path.display()))?;
    }
    info!(
        bytes_before,
        bytes_freed, held_bytes, budget_bytes, "build cache budget enforced"
    );
    Ok(())
}

fn candidate_entries(target_dir: &Path) -> Result<CacheContents> {
    if !target_dir.is_absolute() || target_dir.parent().is_none() {
        bail!(
            "refusing to inspect unsafe build cache path {}",
            target_dir.display()
        );
    }
    let metadata = fs::symlink_metadata(target_dir)
        .with_context(|| format!("reading build cache metadata for {}", target_dir.display()))?;
    if !metadata.file_type().is_dir() {
        bail!(
            "build cache path is not a directory: {}",
            target_dir.display()
        );
    }
    let entries = fs::read_dir(target_dir)
        .with_context(|| format!("reading build cache {}", target_dir.display()))?;
    let mut contents = CacheContents {
        entries: Vec::new(),
        // A target a live job leased is active even between compilations,
        // when no `.cargo-lock` is held.
        active: lease_is_held(target_dir, FileLock::try_exclusive),
        locks: Vec::new(),
    };
    for entry in entries {
        let entry = entry
            .with_context(|| format!("reading an entry in build cache {}", target_dir.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("reading build cache metadata for {}", path.display()))?;
        if !metadata.file_type().is_dir() {
            continue;
        }
        let modified = metadata
            .modified()
            .with_context(|| format!("reading modification time for {}", path.display()))?;
        let scan = scan_directory(&path)?;
        contents.active |= scan.active;
        contents.locks.extend(scan.locks);
        contents.entries.push(CacheEntry {
            path,
            size_bytes: scan.bytes,
            modified,
        });
    }
    Ok(contents)
}

fn scan_directory(path: &Path) -> Result<DirectoryScan> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading build cache metadata for {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        if metadata.file_type().is_file() && path.file_name() == Some(OsStr::new(".cargo-lock")) {
            let (active, lock) = cargo_lock(path)?;
            return Ok(DirectoryScan {
                bytes: allocated_bytes(&metadata),
                active,
                locks: lock.into_iter().collect(),
            });
        }
        return Ok(DirectoryScan {
            bytes: allocated_bytes(&metadata),
            active: false,
            locks: Vec::new(),
        });
    }

    let entries = fs::read_dir(path)
        .with_context(|| format!("reading build cache directory {}", path.display()))?;
    let mut bytes = allocated_bytes(&metadata);
    let mut active = false;
    let mut locks = Vec::new();
    for entry in entries {
        let entry =
            entry.with_context(|| format!("reading an entry in build cache {}", path.display()))?;
        let scan = scan_directory(&entry.path())?;
        bytes = bytes.saturating_add(scan.bytes);
        active |= scan.active;
        locks.extend(scan.locks);
    }
    Ok(DirectoryScan {
        bytes,
        active,
        locks,
    })
}

fn cargo_lock(path: &Path) -> Result<(bool, Option<FileLock>)> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening Cargo build lock {}", path.display()))?;
    match FileLock::try_exclusive(file) {
        Ok(lock) => Ok((false, Some(lock))),
        Err(TryLockError::WouldBlock) => Ok((true, None)),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("checking Cargo build lock {}", path.display()))
        }
    }
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

/// Whether a live job holds `directory`, asked with the request that sees the
/// kind of holder in question: `try_lock_shared` for a checkout, `try_lock`
/// for a Cargo target.
///
/// `.cargo-lock` cannot answer that: Cargo holds it only while it compiles, so
/// a checkout or shared target whose tests are already running looks
/// abandoned. A reclaim that believed it deleted a live `target` out from
/// under a sibling job, and the 1869 tests that then failed to exec their own
/// binaries looked like the product breaking rather than the CI eating itself.
///
/// A checkout owner takes the lease exclusively, so a shared request already
/// cannot be granted while it holds, and it leaves concurrent observers alone.
/// Asking exclusively made the question itself exclusive, so two observers
/// answered "held" about each other while no job held anything — measured on
/// Linux with four observers and no owner, 37694 of 80000 answers were that
/// invention. Target holders take the lease shared so several coexist, so only
/// an exclusive request sees them at all.
fn lease_is_held(directory: &Path, ask: fn(File) -> Result<FileLock, TryLockError>) -> bool {
    let path = directory.join(lease::FILE);
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    // Held by a live job, or unreadable — either way, not ours to remove.
    ask(file).is_err()
}

/// Claims `CARGO_TARGET_DIR` for the life of this process, if one is named.
///
/// The checkout lease cannot protect it: on Linux runners the target is a
/// per-runner Docker volume the host budgets directly, and a lease on the
/// checkout says nothing about a directory outside it. A lane that builds into
/// a directory of its own claims that one where it names it, so both claims are
/// the one protocol [`lease`] owns and [`lease_is_held`] asks about.
pub(crate) fn hold_target_lease() -> Option<lease::Lease> {
    lease::hold(&PathBuf::from(env::var_os("CARGO_TARGET_DIR")?))
}

/// Every `target` directory under `root`, so a caller can hand them to
/// [`enforce_budget`]. Shared with the environment gate: refusing a job is only
/// honest once these have been reclaimed.
///
/// A checkout a job is working in is listed like any other. Protection belongs
/// one level down, on the build directory the lane itself claims: skipping the
/// whole checkout took its caches out of the budget's sight before it could
/// weigh them, so on a host whose only checkout is the live one the ceiling had
/// nothing at all to act on and never reclaimed the space it exists to hold.
/// Listed here, the cache a lane is running from is kept by
/// [`candidate_entries`] and its bytes are charged against the ceiling, which is
/// what leaves the idle caches beside it payable.
pub(crate) fn persistent_target_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut pending = vec![root.to_path_buf()];
    let mut targets = Vec::new();
    while let Some(directory) = pending.pop() {
        if directory.join("Cargo.toml").is_file() {
            // `target-stress` belongs here too: it is target-dir sized, the
            // repo's own tooling creates it, and no other pass owns it. It is
            // only safe to reclaim because the lane that builds into it holds
            // the lease `candidate_entries` asks about.
            for name in ["target", "target-flash-off", "target-stress"] {
                let path = directory.join(name);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error).with_context(|| format!("reading {}", path.display()));
                    }
                };
                if metadata.file_type().is_dir() {
                    targets.push(path);
                }
            }
            continue;
        }

        let entries = fs::read_dir(&directory)
            .with_context(|| format!("reading CI workspace directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| {
                format!(
                    "reading an entry in CI workspace directory {}",
                    directory.display()
                )
            })?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("reading CI workspace metadata for {}", path.display()))?;
            if metadata.file_type().is_dir() {
                pending.push(path);
            }
        }
    }
    targets.sort();
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::{Duration, UNIX_EPOCH},
    };

    use super::*;

    fn entry(path: &str, size_bytes: u64, age: u64) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path),
            size_bytes,
            modified: UNIX_EPOCH + Duration::from_secs(age),
        }
    }

    #[test]
    fn concurrent_observers_do_not_invent_a_checkout_holder() {
        let directory = tempfile::tempdir().unwrap();
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.path().join(lease::FILE))
            .unwrap();
        let checkout = Arc::new(directory.path().to_path_buf());
        let invented = Arc::new(AtomicU64::new(0));

        let observers: Vec<_> = (0..4)
            .map(|_| {
                let checkout = Arc::clone(&checkout);
                let invented = Arc::clone(&invented);
                thread::spawn(move || {
                    for _ in 0..2_000 {
                        if lease_is_held(&checkout, FileLock::try_shared) {
                            invented.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        for observer in observers {
            observer.join().unwrap();
        }

        assert_eq!(
            invented.load(Ordering::Relaxed),
            0,
            "an observer answered for a job that never took the lease"
        );
    }

    #[test]
    fn under_budget_deletes_nothing() {
        let entries = vec![entry("debug", 10, 1), entry("release", 20, 2)];

        assert!(select_evictions(entries, 30).is_empty());
    }

    #[test]
    fn over_budget_deletes_oldest_first() {
        let entries = vec![
            entry("newest", 10, 3),
            entry("oldest", 10, 1),
            entry("middle", 10, 2),
        ];
        let paths: Vec<PathBuf> = select_evictions(entries, 0)
            .into_iter()
            .map(|entry| entry.path)
            .collect();

        assert_eq!(paths, ["oldest", "middle", "newest"].map(PathBuf::from));
    }

    #[test]
    fn deletion_stops_as_soon_as_the_budget_is_met() {
        let entries = vec![entry("oldest", 5, 1), entry("newest", 7, 2)];

        assert_eq!(select_evictions(entries, 7).len(), 1);
    }

    #[test]
    fn equal_timestamps_break_ties_by_path() {
        let entries = vec![entry("b", 1, 1), entry("a", 1, 1)];
        let paths: Vec<PathBuf> = select_evictions(entries, 0)
            .into_iter()
            .map(|entry| entry.path)
            .collect();

        assert_eq!(paths, ["a", "b"].map(PathBuf::from));
    }

    /// Each checkout under the budget while the host they share is out of room
    /// is the state that produced `bytes_freed=0` on every pass for hours.
    #[test]
    fn the_budget_is_a_ceiling_over_every_checkout_together() {
        let root = tempfile::tempdir().unwrap();
        let first = checkout_target(root.path(), "one", 100_000);
        let second = checkout_target(root.path(), "two", 100_000);
        let budget = 150_000;
        assert!(
            occupied(&first) < budget,
            "each checkout is under the budget"
        );

        enforce_budget(&[first.clone(), second.clone()], budget).unwrap();

        assert!(occupied(&first) + occupied(&second) <= budget);
    }

    /// The refused job's state, which the ceiling cannot see: caches sit under
    /// it, so an `enforce_budget` pass frees nothing, while the volume is short
    /// of what one job needs because the rest of it is spent on checkouts,
    /// toolchains and guests. That is how a job was refused over one missing
    /// gigabyte with 15.7 GB of evictable cache beside it.
    #[test]
    fn a_shortfall_is_reclaimed_even_though_the_caches_are_under_the_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let first = checkout_target(root.path(), "one", 100_000);
        let second = checkout_target(root.path(), "two", 100_000);
        let targets = [first.clone(), second.clone()];
        let before = occupied(&first) + occupied(&second);
        enforce_budget(&targets, before * 2).unwrap();
        assert_eq!(
            occupied(&first) + occupied(&second),
            before,
            "under the ceiling the hourly pass has nothing to do"
        );
        let shortfall = before / 4;

        reclaim_at_least(&targets, shortfall).unwrap();

        let left = occupied(&first) + occupied(&second);
        assert!(
            before - left >= shortfall,
            "the shortfall of {shortfall} must be freed, {left} bytes left of {before}"
        );
        assert!(
            left > 0,
            "only the shortfall is owed; the rest of the cache is worth keeping"
        );
    }

    fn checkout_target(root: &Path, name: &str, bytes: usize) -> PathBuf {
        let target = root.join(name);
        let profile = target.join("debug");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("artifact"), vec![0_u8; bytes]).unwrap();
        target
    }

    fn occupied(target: &Path) -> u64 {
        total_bytes(&candidate_entries(target).unwrap().entries)
    }

    #[test]
    fn an_active_cargo_profile_defers_eviction() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("debug");
        fs::create_dir(&profile).unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(profile.join(".cargo-lock"))
            .unwrap();
        let lock = FileLock::exclusive(file).unwrap();

        assert!(candidate_entries(directory.path()).unwrap().active);
        drop(lock);
    }

    /// A run that is only executing its built tests holds no
    /// `.cargo-lock`; the root lease is what says a job still lives in this
    /// target between compilations.
    #[test]
    fn a_leased_target_defers_eviction() {
        let directory = tempfile::tempdir().unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.path().join(lease::FILE))
            .unwrap();
        let held = FileLock::try_shared(file).unwrap();

        assert!(candidate_entries(directory.path()).unwrap().active);
        drop(held);
    }

    /// A lease file nobody holds is a leftover, not a claim.
    #[test]
    fn a_released_lease_leaves_the_target_evictable() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(lease::FILE), b"").unwrap();

        assert!(!candidate_entries(directory.path()).unwrap().active);
    }

    /// The claim a lane takes and the question a reclaim asks are one protocol,
    /// so the holder has to be the real one, not a lock this test rolled itself.
    #[test]
    fn a_lane_holding_its_build_directory_keeps_it() {
        let directory = tempfile::tempdir().unwrap();
        let held = lease::hold(directory.path()).expect("hold the build directory");

        assert!(candidate_entries(directory.path()).unwrap().active);

        drop(held);
        assert!(!candidate_entries(directory.path()).unwrap().active);
    }

    /// The stress lane builds into a directory of its own, and a build cache no
    /// budget names is one the host can never get the space back from.
    #[test]
    fn the_stress_build_directory_is_a_cache_the_budget_owns() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("disrupt/kithara");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), b"").unwrap();
        for name in ["target", "target-flash-off", "target-stress"] {
            checkout_target(&checkout, name, 1);
        }

        let targets = persistent_target_dirs(root.path()).unwrap();

        assert_eq!(
            targets,
            vec![
                checkout.join("target"),
                checkout.join("target-flash-off"),
                checkout.join("target-stress"),
            ]
        );
    }

    /// A checkout a job holds still has to answer to the ceiling.
    ///
    /// The lane's own build directory is the one it must not lose, and its own
    /// claim says so. The idle siblings beside it belong to lanes that have
    /// finished, and on a host whose only checkout is this one they are the only
    /// space the budget can ever get back — so they have to reach
    /// [`enforce_budget`], which only ever sees what this returns.
    #[test]
    fn a_leased_checkout_still_offers_the_caches_no_lane_is_using() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("disrupt/kithara");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), b"").unwrap();
        for name in ["target", "target-flash-off"] {
            checkout_target(&checkout, name, 1);
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(checkout.join(lease::FILE))
            .unwrap();
        let job = FileLock::try_exclusive(file).unwrap();
        let lane = lease::hold(&checkout.join("target")).expect("the lane claims what it builds");

        let targets = persistent_target_dirs(root.path()).unwrap();

        assert_eq!(
            targets,
            vec![checkout.join("target"), checkout.join("target-flash-off")],
            "a job holding its checkout must not take its caches out of the budget's sight"
        );
        drop((job, lane));
    }

    /// What the ceiling does with the two once it can see them: the cache a lane
    /// is running from survives, and its bytes are still spent, so the idle one
    /// beside it is what pays.
    #[test]
    fn the_cache_a_lane_runs_from_survives_and_the_idle_one_pays() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("disrupt/kithara");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), b"").unwrap();
        let running = checkout_target(&checkout, "target", 400_000);
        let idle = checkout_target(&checkout, "target-flash-off", 400_000);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(checkout.join(lease::FILE))
            .unwrap();
        let job = FileLock::try_exclusive(file).unwrap();
        let lane = lease::hold(&running).expect("the lane claims what it builds");

        let targets = persistent_target_dirs(root.path()).unwrap();
        enforce_budget(&targets, occupied(&running)).unwrap();

        assert!(
            running.join("debug/artifact").exists(),
            "the cache a lane is executing from must survive the ceiling"
        );
        assert!(
            !idle.join("debug/artifact").exists(),
            "the live cache is charged against the ceiling, so the idle one is evicted"
        );
        drop((job, lane));
    }
}
