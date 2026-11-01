#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    process,
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use fs4::TryLockError;
use kithara_devtools::{lease, lock::FileLock};
use tracing::{info, warn};

use super::{
    build_dir::{Claim, claim_beside_alias},
    environment::CacheTrust,
};
use crate::consts;

/// One build directory in a build root: a lane's, a stress run's, xtask's own.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CacheEntry {
    path: PathBuf,
    size_bytes: u64,
    /// When a job last used the entry's lease. A directory no job ever leased
    /// is left from a layout no lane builds in any more, and dates from the
    /// epoch, before every directory a lane has used.
    used: SystemTime,
}

struct CacheContents {
    entries: Vec<CacheEntry>,
    /// Entries a live job holds. They are charged against the ceiling and
    /// never evicted.
    held: Vec<CacheEntry>,
}

/// The least recently used entries, oldest first, breaking ties by path.
fn eviction_order(mut entries: Vec<CacheEntry>) -> Vec<CacheEntry> {
    entries.sort_by(|left, right| {
        left.used
            .cmp(&right.used)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries
}

/// The budget is what the host can afford in total, not what one checkout may
/// keep.
///
/// Applied per directory it never fires on a machine that is running out:
/// three checkouts holding 14, 7 and 22 GB were each under a 25 GB budget, so
/// every hourly pass reported `bytes_freed=0` while the volume they share sat
/// at `Aggressive` and jobs were already being refused. A directory an active
/// job holds cannot be evicted, but the room it occupies is still spent, so it
/// is charged against the ceiling rather than excused from it.
pub(crate) fn enforce_budget(target_dirs: &[PathBuf], budget_bytes: u64) -> Result<()> {
    let (candidates, held_bytes) = collect(target_dirs)?;
    evict_to_budget(candidates, held_bytes, budget_bytes);
    Ok(())
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
    let (candidates, held_bytes) = collect(target_dirs)?;
    let keep = total_bytes(&candidates).saturating_sub(bytes_needed);
    evict_to_budget(candidates, held_bytes, keep.saturating_add(held_bytes));
    Ok(())
}

fn collect(target_dirs: &[PathBuf]) -> Result<(Vec<CacheEntry>, u64)> {
    let mut target_dirs = target_dirs.to_vec();
    target_dirs.sort();
    let mut candidates = Vec::new();
    let mut held_bytes = 0_u64;
    for target_dir in &target_dirs {
        let contents = candidate_entries(target_dir)?;
        let held = total_bytes(&contents.held);
        held_bytes = held_bytes.saturating_add(held);
        if held > 0 {
            info!(
                path = %target_dir.display(),
                held_bytes = held,
                "keeping the build directories live jobs hold"
            );
        }
        candidates.extend(contents.entries);
    }
    Ok((candidates, held_bytes))
}

fn total_bytes(entries: &[CacheEntry]) -> u64 {
    entries
        .iter()
        .map(|entry| entry.size_bytes)
        .fold(0_u64, u64::saturating_add)
}

/// Evicts the oldest candidates until what is left fits `budget_bytes`.
///
/// A directory that cannot be removed is that directory's problem: it is
/// reported and the others still go. Stopping on it left the host's budget
/// unenforced and refused every job its room for as long as it stood.
fn evict_to_budget(candidates: Vec<CacheEntry>, held_bytes: u64, budget_bytes: u64) {
    let candidate_bytes = total_bytes(&candidates);
    let bytes_before = candidate_bytes.saturating_add(held_bytes);
    let bytes_needed = candidate_bytes.saturating_sub(budget_bytes.saturating_sub(held_bytes));
    let mut bytes_freed = 0_u64;
    for entry in eviction_order(candidates) {
        if bytes_freed >= bytes_needed {
            break;
        }
        match evict(&entry) {
            Ok(bytes) => bytes_freed = bytes_freed.saturating_add(bytes),
            Err(error) => warn!(
                path = %entry.path.display(),
                "{error:#}; the build directory waits for a later pass"
            ),
        }
    }
    info!(
        bytes_before,
        bytes_freed, held_bytes, budget_bytes, "build cache budget enforced"
    );
}

/// Removes one entry and returns the bytes it held, or nothing when a job took
/// it after the scan.
///
/// The entry is moved aside under the eviction's exclusive lease before
/// anything in it is removed, so a job that enters it meanwhile finds its path
/// free and builds in a new directory instead of one being emptied under it.
fn evict(entry: &CacheEntry) -> Result<u64> {
    let _slot = match try_hold(&lock_beside(&entry.path))? {
        Hold::Busy => {
            info!(
                path = %entry.path.display(),
                "keeping the lane slot a job took after the scan"
            );
            return Ok(0);
        }
        Hold::Absent => None,
        Hold::Taken(lock) => Some(lock),
    };
    let _eviction = match lease::evict(&entry.path) {
        Ok(Some(eviction)) => eviction,
        Ok(None) => {
            info!(
                path = %entry.path.display(),
                "keeping the build directory a job took after the scan"
            );
            return Ok(0);
        }
        // Another eviction moved it aside first.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("fencing build directory {}", entry.path.display()));
        }
    };
    let Some(_cargo) = hold_cargo_locks(&entry.path)? else {
        info!(
            path = %entry.path.display(),
            "keeping the build directory Cargo started building in after the scan"
        );
        return Ok(0);
    };
    if taken_below(&entry.path)? {
        info!(
            path = %entry.path.display(),
            "keeping the build directory a job took after the scan"
        );
        return Ok(0);
    }
    let aside = aside(&entry.path)?;
    fs::rename(&entry.path, &aside).with_context(|| {
        format!(
            "moving build directory {} aside to {}",
            entry.path.display(),
            aside.display()
        )
    })?;
    info!(path = %entry.path.display(), bytes = entry.size_bytes, "evicting build cache");
    remove_aside(&aside)?;
    Ok(entry.size_bytes)
}

/// The `.cargo-lock` of every profile in `entry`, held so Cargo cannot start
/// building in one while it is removed, or `None` when Cargo already holds one.
pub(super) fn hold_cargo_locks(entry: &Path) -> Result<Option<Vec<FileLock>>> {
    let mut held = Vec::new();
    for dir in levels(entry, consts::CARGO_LOCK_DEPTH)? {
        match try_hold(&dir.join(consts::CARGO_LOCK))? {
            Hold::Busy => return Ok(None),
            Hold::Absent => {}
            Hold::Taken(lock) => held.push(lock),
        }
    }
    Ok(Some(held))
}

/// Whether a job has taken `entry` since the scan where the fence on its top
/// level cannot see it: a fresh heartbeat at any level, or a lease held below
/// the top.
fn taken_below(entry: &Path) -> Result<bool> {
    let levels = levels(entry, consts::LEASE_DEPTH)?;
    Ok(levels
        .iter()
        .any(|dir| heartbeat_is_fresh(&dir.join(lease::HEARTBEAT)))
        || levels[1..]
            .iter()
            .any(|dir| lock_is_held(&dir.join(lease::FILE))))
}

/// Where `entry` goes before it is removed: a hidden name in the same root,
/// which no build takes, unique to this move, and swept by the next pass when
/// whoever moved it there does not remove it.
pub(crate) fn aside(entry: &Path) -> Result<PathBuf> {
    let (Some(root), Some(name)) = (entry.parent(), entry.file_name()) else {
        bail!("build directory {} names no root", entry.display());
    };
    let at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("reading the clock")?
        .as_nanos();
    let mut aside = OsString::from(consts::EVICTING_PREFIX);
    aside.push(name);
    aside.push(format!("-{}-{at}", process::id()));
    Ok(root.join(aside))
}

/// Removes a directory an eviction moved aside, its lease last: a pass that
/// finds the lease finds the eviction holding it, and one that finds no lease
/// finds only the empty directory left to remove.
fn remove_aside(aside: &Path) -> Result<()> {
    let listing =
        fs::read_dir(aside).with_context(|| format!("reading build cache {}", aside.display()))?;
    for child in listing {
        let child = child
            .with_context(|| format!("reading an entry in build cache {}", aside.display()))?;
        if child.file_name() == lease::FILE {
            continue;
        }
        let path = child.path();
        let removed = if child.file_type()?.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.with_context(|| format!("removing build cache {}", path.display()))?;
    }
    still_there(fs::remove_file(aside.join(lease::FILE)))
        .with_context(|| format!("removing the lease of {}", aside.display()))?;
    still_there(fs::remove_dir(aside))
        .with_context(|| format!("removing build cache {}", aside.display()))?;
    Ok(())
}

/// Removes what an eviction that died midway moved aside, unless the eviction
/// is still running and holds its lease.
fn remove_leftover(aside: &Path) -> Result<()> {
    let lease_file = aside.join(lease::FILE);
    let file = match OpenOptions::new().read(true).write(true).open(&lease_file) {
        Ok(file) => file,
        // The lease goes last, so what is left is the empty directory, which
        // an eviction may be removing right now.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            still_there(fs::remove_dir_all(aside))
                .with_context(|| format!("removing build cache {}", aside.display()))?;
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("opening {}", lease_file.display()));
        }
    };
    match FileLock::try_exclusive(file) {
        Ok(_eviction) => remove_aside(aside),
        Err(TryLockError::WouldBlock) => Ok(()),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", lease_file.display()))
        }
    }
}

/// What a listed path still is, or nothing when it is already gone.
///
/// The cache being measured is one a job may be building in, and a compiler
/// writes a temporary file and removes it again. A name the listing returned
/// and the build has since deleted is that race, not a broken cache: there is
/// nothing left to count or to reclaim. Any other failure is a real one, and
/// stopping the sweep on it is why the budget went unenforced.
fn still_there<T>(found: io::Result<T>) -> io::Result<Option<T>> {
    match found {
        Ok(found) => Ok(Some(found)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn candidate_entries(target_dir: &Path) -> Result<CacheContents> {
    if target_dir.parent().is_none() || !target_dir.is_absolute() {
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
    if is_leased_cargo_target(target_dir) {
        let (entry, held) = inspect_entry(target_dir.to_path_buf())?;
        return Ok(if held {
            CacheContents {
                entries: Vec::new(),
                held: vec![entry],
            }
        } else {
            CacheContents {
                entries: vec![entry],
                held: Vec::new(),
            }
        });
    }
    let listing = fs::read_dir(target_dir)
        .with_context(|| format!("reading build cache {}", target_dir.display()))?;
    let mut contents = CacheContents {
        entries: Vec::new(),
        held: Vec::new(),
    };
    for entry in listing {
        let entry = entry
            .with_context(|| format!("reading an entry in build cache {}", target_dir.display()))?;
        let path = entry.path();
        let Some(metadata) = still_there(fs::symlink_metadata(&path))
            .with_context(|| format!("reading build cache metadata for {}", path.display()))?
        else {
            continue;
        };
        // The build alias is a link to an entry listed beside it.
        if !metadata.file_type().is_dir() {
            continue;
        }
        if is_leftover(&path) {
            // A leftover that cannot be removed is its own problem, as an
            // entry that cannot be evicted is.
            if let Err(error) = remove_leftover(&path) {
                warn!(path = %path.display(), "{error:#}; it waits for a later pass");
            }
            continue;
        }
        if is_hidden(&path) {
            continue;
        }
        let (entry, held) = inspect_entry(path)?;
        if held {
            contents.held.push(entry);
        } else {
            contents.entries.push(entry);
        }
    }
    Ok(contents)
}

fn inspect_entry(path: PathBuf) -> Result<(CacheEntry, bool)> {
    let mut used = SystemTime::UNIX_EPOCH;
    let mut held = lock_is_held(&lock_beside(&path));
    for dir in levels(&path, consts::LEASE_DEPTH)? {
        for mark in [lease::FILE, lease::HEARTBEAT] {
            let mark = dir.join(mark);
            if let Some(found) = still_there(fs::metadata(&mark))
                .with_context(|| format!("reading {}", mark.display()))?
            {
                let date = found
                    .modified()
                    .with_context(|| format!("reading the date of {}", mark.display()))?;
                used = used.max(date);
            }
        }
        held = held
            || lock_is_held(&dir.join(lease::FILE))
            || heartbeat_is_fresh(&dir.join(lease::HEARTBEAT));
    }
    held = held
        || levels(&path, consts::CARGO_LOCK_DEPTH)?
            .iter()
            .any(|dir| lock_is_held(&dir.join(consts::CARGO_LOCK)));
    let entry = CacheEntry {
        size_bytes: directory_bytes(&path)?,
        used,
        path,
    };
    Ok((entry, held))
}

/// Cargo writes no hidden directory at the top of a build directory, so one
/// there belongs to something else. On the Linux fleet it is the CI cache root,
/// Cargo home included, which the per-runner build directory carries: evicted
/// as the oldest entry, it took every running job's registry sources and git
/// checkouts with it on each two-hourly pass.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
}

fn is_leftover(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.as_encoded_bytes()
            .starts_with(consts::EVICTING_PREFIX.as_bytes())
    })
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let Some(metadata) = still_there(fs::symlink_metadata(path))
        .with_context(|| format!("reading build cache metadata for {}", path.display()))?
    else {
        return Ok(0);
    };
    let mut bytes = allocated_bytes(&metadata);
    if !metadata.file_type().is_dir() {
        return Ok(bytes);
    }
    let Some(listing) = still_there(fs::read_dir(path))
        .with_context(|| format!("reading build cache directory {}", path.display()))?
    else {
        return Ok(bytes);
    };
    for entry in listing {
        let entry =
            entry.with_context(|| format!("reading an entry in build cache {}", path.display()))?;
        bytes = bytes.saturating_add(directory_bytes(&entry.path())?);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

/// Whether a live job holds the lock file at `path`: a lease, a lane slot's
/// lock, or Cargo's.
///
/// `.cargo-lock` alone cannot answer for a job: Cargo holds it only while it
/// compiles, so a directory whose tests are already running looks abandoned. A
/// reclaim that believed it deleted a live `target` out from under a sibling
/// job, and the 1869 tests that then failed to exec their own binaries looked
/// like the product breaking rather than the CI eating itself. It answers only
/// for a build no lease names. Lease holders take the lease shared so several
/// coexist, so only an exclusive request sees them. The file is only opened,
/// never made: a directory the scan merely looked at is not one a job took.
fn lock_is_held(path: &Path) -> bool {
    // Held by a live job, or unreadable — either way, not ours to remove.
    !matches!(try_hold(path), Ok(Hold::Absent | Hold::Taken(_)))
}

/// A lock file a job may hold its build by, as an eviction finds it.
enum Hold {
    /// There is no such file, so nothing holds the build by it.
    Absent,
    /// A job holds it.
    Busy,
    /// The eviction holds it, until it drops this.
    Taken(FileLock),
}

/// Takes the lock file at `path` when it exists and no job holds it.
fn try_hold(path: &Path) -> Result<Hold> {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Hold::Absent),
        Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
    };
    match FileLock::try_exclusive(file) {
        Ok(lock) => Ok(Hold::Taken(lock)),
        Err(TryLockError::WouldBlock) => Ok(Hold::Busy),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// The lock a job takes a lane slot by: a file beside the slot, named after
/// it. `production/main` takes its own slots the same way.
pub(crate) fn lock_beside(entry: &Path) -> PathBuf {
    let mut lock = entry.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// `entry` and every directory below it down to `depth` levels, nearest
/// first. A directory gone before it is read was a build's temporary one.
fn levels(entry: &Path, depth: usize) -> Result<Vec<PathBuf>> {
    let mut found = vec![entry.to_path_buf()];
    let mut level = 0..1;
    for _ in 0..depth {
        let start = found.len();
        for index in level {
            let dir = found[index].clone();
            let Some(listing) = still_there(fs::read_dir(&dir))
                .with_context(|| format!("reading build cache {}", dir.display()))?
            else {
                continue;
            };
            for child in listing {
                let child = child.with_context(|| {
                    format!("reading an entry in build cache {}", dir.display())
                })?;
                if child
                    .file_type()
                    .with_context(|| format!("reading the type of {}", child.path().display()))?
                    .is_dir()
                {
                    found.push(child.path());
                }
            }
        }
        level = start..found.len();
    }
    Ok(found)
}

fn heartbeat_is_fresh(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map_or(true, |age| age <= consts::HEARTBEAT_MAX_AGE)
}

/// The directory `CARGO_TARGET_DIR` names, held for the life of this process.
/// The claim is settled before the lease lets the directory go.
#[derive(Debug)]
pub(crate) struct HeldTarget {
    _sources: Option<Claim>,
    _lease: lease::Lease,
}

/// Holds the directory `var` names as `CARGO_TARGET_DIR`, if one is named,
/// for `checkout`'s builds.
///
/// The checkout lease cannot protect it: on Linux runners the target is a
/// per-runner Docker volume the host budgets directly, and a lease on the
/// checkout says nothing about a directory outside it. A lane that builds into
/// a directory of its own claims that one where it names it, so both claims are
/// the one protocol [`lease`] owns and [`lock_is_held`] asks about.
///
/// A CI job claims the directory for `checkout` too where its lanes claim
/// theirs, as [`claim_beside_alias`] says.
///
/// A build alias is skipped: it is a link `ci lane` points at the lane's own
/// directory, and holds that directory itself once it knows the lane.
pub(crate) fn hold_target(
    checkout: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<HeldTarget>> {
    var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .filter(|target| target.file_name() != Some(OsStr::new(consts::BUILD_ALIAS)))
        .map(|target| {
            let lease =
                lease::hold(&target).with_context(|| format!("lease {}", target.display()))?;
            Ok(HeldTarget {
                _sources: claim_beside_alias(checkout, &target, var)?,
                _lease: lease,
            })
        })
        .transpose()
}

/// Every build root under `root`, so a caller can hand them to
/// [`enforce_budget`]: a directory holding the build alias or xtask's own
/// build, as the one an executor names beside each checkout does. Shared with
/// the environment gate: refusing a job is only honest once these have been
/// reclaimed.
///
/// The walk inspects only direct children of a checkout for leased Cargo
/// targets, leaving its sources and report directories alone. It stops
/// at a root, whose entries are the builds the budget weighs. A root a job
/// is building in is listed like any other: the lease on the build it entered
/// keeps that one, and its bytes are charged against the ceiling, which is
/// what leaves the idle builds beside it payable.
pub(crate) fn build_roots(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut pending = vec![root.to_path_buf()];
    let mut roots = Vec::new();
    while let Some(directory) = pending.pop() {
        if is_leased_cargo_target(&directory) {
            roots.push(directory);
            continue;
        }
        let checkout = directory.join("Cargo.toml").is_file();
        if !checkout && holds_a_build(&directory) {
            roots.push(directory);
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
            let path = entry.path();
            if checkout && !is_leased_cargo_target(&path) {
                continue;
            }
            // A hidden directory is walked only as the root an executor
            // names: anything else hidden there is not this walk's.
            if is_hidden(&path) && !holds_a_build(&path) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("reading CI workspace metadata for {}", path.display()))?;
            if metadata.file_type().is_dir() {
                pending.push(path);
            }
        }
    }
    roots.sort();
    Ok(roots)
}

/// Only a leased Cargo output directory may be reclaimed as a whole.
fn is_leased_cargo_target(directory: &Path) -> bool {
    fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_dir())
        && !directory.join("Cargo.toml").exists()
        && !holds_a_build(directory)
        && [".rustc_info.json", lease::FILE].iter().all(|name| {
            fs::symlink_metadata(directory.join(name))
                .is_ok_and(|metadata| metadata.file_type().is_file())
        })
}

/// Every build root a host's budget weighs: those beside the checkouts under
/// `workspaces`, and those `production/main` keeps under `cache`, the root its
/// jobs are told.
pub(crate) fn budget_roots(workspaces: &Path, cache: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = build_roots(workspaces)?;
    roots.extend(previous_build_roots(cache)?);
    Ok(roots)
}

/// The build roots `production/main` keeps under its cache root while a host
/// deployed from this branch serves it: its lane slots, where it keeps them
/// there, and one directory per trust it bootstraps xtask for.
pub(crate) fn previous_build_roots(cache: &Path) -> Result<Vec<PathBuf>> {
    let slots = cache.join(consts::PREVIOUS_TARGET_SLOTS);
    let mut roots: Vec<PathBuf> = slots.is_dir().then_some(slots).into_iter().collect();
    let bootstrap = cache.join(consts::PREVIOUS_BOOTSTRAP);
    let listing = match fs::read_dir(&bootstrap) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(roots),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", bootstrap.display()));
        }
    };
    for entry in listing {
        let entry = entry.with_context(|| format!("reading {}", bootstrap.display()))?;
        if CacheTrust::ALL
            .iter()
            .any(|trust| entry.file_name() == trust.as_str())
            && entry.file_type()?.is_dir()
        {
            roots.push(entry.path());
        }
    }
    Ok(roots)
}

/// Whether `directory` is a build root: it holds the build alias or xtask's
/// own build.
fn holds_a_build(directory: &Path) -> bool {
    [consts::BUILD_ALIAS, consts::XTASK_BUILD]
        .iter()
        .any(|name| fs::symlink_metadata(directory.join(name)).is_ok())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{File, FileTimes},
        time::{Duration, UNIX_EPOCH},
    };

    use super::*;
    use crate::ci::build_dir::fixture::git_checkout;

    fn entry(path: &str, size_bytes: u64, age: u64) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path),
            size_bytes,
            used: UNIX_EPOCH + Duration::from_secs(age),
        }
    }

    fn evicted(evictions: Vec<CacheEntry>) -> Vec<PathBuf> {
        evictions.into_iter().map(|entry| entry.path).collect()
    }

    /// A build directory with `bytes` of artifacts in it, named `id` in
    /// `root`.
    fn build_dir(root: &Path, id: &str, bytes: usize) -> PathBuf {
        let dir = root.join(id);
        let profile = dir.join("debug");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("artifact"), vec![0_u8; bytes]).unwrap();
        dir
    }

    #[test]
    fn checkout_builds_share_the_host_budget_without_removing_sources_or_reports() {
        let workspace = tempfile::tempdir().unwrap();
        let checkout = workspace.path().join("runner/repository/subject");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(checkout.join("source.rs"), "source").unwrap();
        let reports = checkout.join("target/nextest");
        fs::create_dir_all(&reports).unwrap();
        fs::write(reports.join("junit.xml"), "report").unwrap();
        let nested_source = checkout.join("nested-source");
        fs::create_dir_all(&nested_source).unwrap();
        for name in ["Cargo.toml", ".rustc_info.json", lease::FILE] {
            fs::write(nested_source.join(name), "source metadata").unwrap();
        }
        let build = build_dir(&checkout, "target-stress", 100);
        fs::write(build.join(".rustc_info.json"), "{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&build, checkout.join("target-link")).unwrap();
        let held = lease::hold(&build).unwrap();

        let roots = build_roots(workspace.path()).unwrap();
        assert_eq!(roots, std::slice::from_ref(&build));
        enforce_budget(&roots, 0).unwrap();
        assert!(build.join("debug/artifact").is_file());
        drop(held);
        enforce_budget(&roots, 0).unwrap();

        assert!(!build.exists());
        assert!(checkout.join("Cargo.toml").is_file());
        assert!(nested_source.join("Cargo.toml").is_file());
        assert_eq!(
            fs::read_to_string(checkout.join("source.rs")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_to_string(reports.join("junit.xml")).unwrap(),
            "report"
        );
    }

    fn occupied(root: &Path) -> u64 {
        total_bytes(&candidate_entries(root).unwrap().entries)
    }

    #[test]
    fn cargo_budget_preserves_the_bootstrap_compiler_store() {
        let cache = tempfile::tempdir().unwrap();
        let bootstrap = cache.path().join(consts::PREVIOUS_BOOTSTRAP);
        let store = bootstrap.join("macos-aarch64/kache/store");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("index"), b"compiler index").unwrap();
        fs::write(store.join("blob"), b"compiler artifact").unwrap();
        let build = build_dir(&bootstrap.join("review"), "target-macos-aarch64-0", 4096);
        let slot = build_dir(
            &cache.path().join(consts::PREVIOUS_TARGET_SLOTS),
            "lint-0",
            4096,
        );

        let roots = previous_build_roots(cache.path()).unwrap();
        enforce_budget(&roots, 0).unwrap();

        assert_eq!(fs::read(store.join("index")).unwrap(), b"compiler index");
        assert_eq!(fs::read(store.join("blob")).unwrap(), b"compiler artifact");
        assert!(!build.exists(), "idle bootstrap build was retained");
        assert!(!slot.exists(), "idle lane build was retained");
    }

    /// A runner always has some job, so a root held whole was 1.4 TB the
    /// ceiling could never reclaim, and every pass answered the shortfall by
    /// evicting every warm lane directory instead. The scan has to see the
    /// lease where the lane takes it, and charge that one directory only. The
    /// alias beside it is a link to it, and is neither followed nor counted.
    #[cfg(unix)]
    #[test]
    fn a_lease_held_in_an_entry_keeps_that_entry_and_not_its_siblings() {
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("lint");
        let idle = root.path().join("usdt");
        fs::create_dir_all(&idle).unwrap();
        let lease = lease::hold(&build).expect("claim the directory the lane builds into");
        std::os::unix::fs::symlink("lint", root.path().join(consts::BUILD_ALIAS)).unwrap();

        let contents = candidate_entries(root.path()).unwrap();

        let paths = |entries: &[CacheEntry]| -> Vec<PathBuf> {
            entries.iter().map(|entry| entry.path.clone()).collect()
        };
        assert_eq!(paths(&contents.held), [build]);
        assert_eq!(paths(&contents.entries), [idle]);
        drop(lease);
    }

    /// Holds `path`'s lock the way the job that owns it does, creating it.
    fn locked(path: &Path) -> FileLock {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        FileLock::try_exclusive(file).unwrap()
    }

    /// `production/main` takes a lane slot by locking the file beside it, and
    /// builds in the slot before it leases it. A host deployed from this
    /// branch serves main too, so its budget leaves a taken slot alone, takes
    /// an idle one, and leaves both locks where main's next claim opens them.
    #[test]
    fn a_slot_whose_lock_is_held_beside_it_survives_the_budget() {
        let root = tempfile::tempdir().unwrap();
        let taken = build_dir(root.path(), "review-lane-lint-0", 4096);
        let idle = build_dir(root.path(), "review-lane-lint-1", 4096);
        let _claim = locked(&root.path().join("review-lane-lint-0.lock"));
        drop(locked(&root.path().join("review-lane-lint-1.lock")));

        enforce_budget(&[root.path().to_path_buf()], 0).unwrap();

        assert!(taken.exists(), "a taken slot was evicted");
        assert!(!idle.exists(), "an idle slot was kept");
        assert!(root.path().join("review-lane-lint-0.lock").is_file());
        assert!(root.path().join("review-lane-lint-1.lock").is_file());
    }

    /// A build no lease names, the network lane's or a bootstrap of xtask, is
    /// held by Cargo itself while it runs: its lock sits in each profile it
    /// builds, with or without a target triple.
    #[test]
    fn a_build_cargo_is_running_in_survives_the_budget() {
        let root = tempfile::tempdir().unwrap();
        let network = build_dir(root.path(), "lane-network", 4096);
        let bootstrap = root.path().join("target-macos-aarch64-local");
        let triple = bootstrap.join("aarch64-apple-darwin/debug");
        fs::create_dir_all(&triple).unwrap();
        let idle = build_dir(root.path(), "lane-idle", 4096);
        let _network = locked(&network.join("debug/.cargo-lock"));
        let _bootstrap = locked(&triple.join(".cargo-lock"));
        drop(locked(&idle.join("debug/.cargo-lock")));

        enforce_budget(&[root.path().to_path_buf()], 0).unwrap();

        assert!(network.exists(), "a build Cargo runs in was evicted");
        assert!(
            bootstrap.exists(),
            "a triple build Cargo runs in was evicted"
        );
        assert!(!idle.exists(), "a build Cargo left was kept");
    }

    /// `production/main`'s executor slots lease the build inside them, one
    /// level down. That lease dates the slot, so the slot used last goes last.
    #[cfg(unix)]
    #[test]
    fn a_lease_one_level_down_dates_its_entry() {
        let root = tempfile::tempdir().unwrap();
        let recent = build_dir(root.path(), "a-recent", 4096);
        let stale = build_dir(root.path(), "b-stale", 4096);
        for (slot, age) in [(&recent, 20), (&stale, 10)] {
            drop(lease::hold(&slot.join("cargo")).unwrap());
            File::options()
                .write(true)
                .open(slot.join("cargo").join(lease::FILE))
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(age))
                .unwrap();
        }

        enforce_budget(&[root.path().to_path_buf()], occupied(root.path()) - 1).unwrap();

        assert!(recent.exists(), "the slot used last was evicted");
        assert!(!stale.exists(), "the slot used first was kept");
    }

    /// A lease one level down that a job holds keeps its entry.
    #[test]
    fn a_lease_held_one_level_down_keeps_its_entry() {
        let root = tempfile::tempdir().unwrap();
        let slot = build_dir(root.path(), "review-lane-lint-0", 4096);
        let _lease = lease::hold(&slot.join("cargo")).unwrap();

        enforce_budget(&[root.path().to_path_buf()], 0).unwrap();

        assert!(slot.exists(), "a slot a job builds in was evicted");
    }

    /// The same tree without a holder: the guard above must not answer "held"
    /// about a directory that was merely left behind, or nothing is ever
    /// reclaimed.
    #[test]
    fn an_unheld_lease_leaves_its_entry_evictable() {
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("lint");
        drop(lease::hold(&build).expect("claim and release"));

        let contents = candidate_entries(root.path()).unwrap();

        assert!(
            contents.held.is_empty(),
            "an abandoned lease kept its entry"
        );
        assert!(contents.entries.iter().any(|entry| entry.path == build));
    }

    #[cfg(unix)]
    #[test]
    fn using_an_old_cache_refreshes_its_eviction_order() {
        let root = tempfile::tempdir().unwrap();
        let used = root.path().join("old-but-used");
        let idle = root.path().join("newer-but-idle");
        fs::create_dir_all(&idle).unwrap();
        drop(lease::hold(&idle).unwrap());
        drop(lease::hold(&used).unwrap());
        for (dir, age) in [(&used, 10), (&idle, 20)] {
            File::options()
                .write(true)
                .open(dir.join(lease::FILE))
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(age))
                .unwrap();
        }

        drop(lease::hold(&used).unwrap());

        let contents = candidate_entries(root.path()).unwrap();
        let date = |path: &Path| {
            contents
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| entry.used)
                .unwrap()
        };
        assert!(date(&used) > date(&idle));
    }

    #[test]
    fn a_long_job_finishing_last_keeps_its_cache_over_a_later_short_job() {
        for lease_dir in [".", "cargo"] {
            let root = tempfile::tempdir().unwrap();
            let long = build_dir(root.path(), "long", 4096);
            let short = build_dir(root.path(), "short", 4096);
            let running = lease::hold(&long.join(lease_dir)).unwrap();
            let long_mark = File::options()
                .write(true)
                .open(long.join(lease_dir).join(lease::FILE))
                .unwrap();
            let long_started = UNIX_EPOCH + Duration::from_secs(10);
            long_mark.set_modified(long_started).unwrap();
            drop(lease::hold(&short.join(lease_dir)).unwrap());
            let short_mark = File::options()
                .write(true)
                .open(short.join(lease_dir).join(lease::FILE))
                .unwrap();
            let short_finished = UNIX_EPOCH + Duration::from_secs(20);
            short_mark.set_modified(short_finished).unwrap();
            assert_eq!(
                long_mark.metadata().unwrap().modified().unwrap(),
                long_started
            );
            assert_eq!(
                short_mark.metadata().unwrap().modified().unwrap(),
                short_finished
            );
            assert!(lease::evict(&long.join(lease_dir)).unwrap().is_none());

            drop(running);

            let contents = candidate_entries(root.path()).unwrap();
            assert!(contents.held.is_empty());
            assert_eq!(contents.entries.len(), 2);
            assert_eq!(
                fs::metadata(long.join("debug/artifact")).unwrap().len(),
                fs::metadata(short.join("debug/artifact")).unwrap().len()
            );
            enforce_budget(&[root.path().to_path_buf()], occupied(root.path()) - 1).unwrap();

            assert!(long.exists(), "the job finishing last lost its cache");
            assert!(!short.exists(), "the earlier completed cache survived");
        }
    }

    /// Only a job's lease dates an entry. A directory no job ever leased is
    /// left from a layout no lane builds in any more, so it goes before any
    /// directory a lane has used, however recently something wrote into it.
    #[test]
    fn an_entry_no_job_ever_leased_goes_before_any_leased_one() {
        let root = tempfile::tempdir().unwrap();
        let leased = root.path().join("lint");
        let legacy = root.path().join("debug");
        drop(lease::hold(&leased).unwrap());
        File::options()
            .write(true)
            .open(leased.join(lease::FILE))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(10))
            .unwrap();
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("artifact"), b"built just now").unwrap();

        let contents = candidate_entries(root.path()).unwrap();

        let date = |path: &Path| {
            contents
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| entry.used)
                .unwrap()
        };
        assert!(date(&legacy) < date(&leased));
        assert!(
            !legacy.join(lease::FILE).exists(),
            "the scan leased a directory it only looked at"
        );
    }

    /// A job inside a virtual machine leases its build directory where the
    /// host cannot see the lock, so the heartbeat it beats into the directory
    /// is what tells the host evictor the job lives.
    #[test]
    fn a_fresh_heartbeat_inside_an_entry_keeps_it() {
        let root = tempfile::tempdir().unwrap();
        let lane = root.path().join("lint");
        fs::create_dir_all(&lane).unwrap();
        fs::write(lane.join(lease::HEARTBEAT), b"").unwrap();

        let contents = candidate_entries(root.path()).unwrap();

        assert!(
            contents.held.iter().any(|entry| entry.path == lane),
            "the directory a VM job beats into was not charged as live"
        );
        assert!(
            contents.entries.is_empty(),
            "the directory a VM job beats into was offered for eviction"
        );
    }

    /// A job that died in its virtual machine stops beating.
    #[test]
    fn a_stale_heartbeat_leaves_its_entry_evictable() {
        let root = tempfile::tempdir().unwrap();
        let lane = root.path().join("lint");
        fs::create_dir_all(&lane).unwrap();
        File::create(lane.join(lease::HEARTBEAT))
            .unwrap()
            .set_times(FileTimes::new().set_modified(
                SystemTime::now() - consts::HEARTBEAT_MAX_AGE - Duration::from_secs(1),
            ))
            .unwrap();

        let contents = candidate_entries(root.path()).unwrap();

        assert!(contents.entries.iter().any(|entry| entry.path == lane));
    }

    #[test]
    fn under_budget_deletes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let lint = build_dir(root.path(), "lint", 4096);
        let usdt = build_dir(root.path(), "usdt", 4096);

        enforce_budget(&[root.path().to_path_buf()], occupied(root.path())).unwrap();

        assert!(lint.exists());
        assert!(usdt.exists());
    }

    #[test]
    fn over_budget_deletes_oldest_first() {
        let entries = vec![
            entry("newest", 10, 3),
            entry("oldest", 10, 1),
            entry("middle", 10, 2),
        ];
        let paths = evicted(eviction_order(entries));

        assert_eq!(paths, ["oldest", "middle", "newest"].map(PathBuf::from));
    }

    #[test]
    fn deletion_stops_as_soon_as_the_budget_is_met() {
        let root = tempfile::tempdir().unwrap();
        let oldest = build_dir(root.path(), "a-oldest", 4096);
        let newest = build_dir(root.path(), "b-newest", 4096);

        enforce_budget(&[root.path().to_path_buf()], occupied(root.path()) - 1).unwrap();

        assert!(!oldest.exists());
        assert!(newest.exists());
    }

    #[test]
    fn equal_timestamps_break_ties_by_path() {
        let entries = vec![entry("b", 1, 1), entry("a", 1, 1)];
        let paths = evicted(eviction_order(entries));

        assert_eq!(paths, ["a", "b"].map(PathBuf::from));
    }

    #[test]
    fn a_cargo_home_inside_a_build_root_is_never_evicted() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let sources = target.join(".kithara-ci/review/linux-x86_64/cargo/registry/src");
        fs::create_dir_all(&sources).unwrap();
        fs::write(sources.join("lib.rs"), vec![0_u8; 100_000]).unwrap();
        let lane = build_dir(&target, "lint", 100_000);

        enforce_budget(std::slice::from_ref(&target), 0).unwrap();

        assert!(sources.join("lib.rs").is_file());
        assert!(!lane.exists());
    }

    /// Each checkout under the budget while the host they share is out of room
    /// is the state that produced `bytes_freed=0` on every pass for hours.
    #[test]
    fn the_budget_is_a_ceiling_over_every_root_together() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("one");
        let second = root.path().join("two");
        build_dir(&first, "lint", 100_000);
        build_dir(&second, "lint", 100_000);
        let budget = 150_000;
        assert!(occupied(&first) < budget, "each root is under the budget");

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
        let first = root.path().join("one");
        let second = root.path().join("two");
        build_dir(&first, "lint", 100_000);
        build_dir(&second, "lint", 100_000);
        let roots = [first.clone(), second.clone()];
        let before = occupied(&first) + occupied(&second);
        enforce_budget(&roots, before * 2).unwrap();
        assert_eq!(
            occupied(&first) + occupied(&second),
            before,
            "under the ceiling the hourly pass has nothing to do"
        );
        let shortfall = before / 4;

        reclaim_at_least(&roots, shortfall).unwrap();

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

    /// What the ceiling does with a live entry and an idle one: the live one
    /// survives, and its bytes are still spent, so the idle one is what pays.
    #[test]
    fn the_directory_a_lane_runs_from_survives_and_the_idle_one_pays() {
        let root = tempfile::tempdir().unwrap();
        let running = build_dir(root.path(), "lint", 400_000);
        let idle = build_dir(root.path(), "usdt", 400_000);
        let lane = lease::hold(&running).expect("the lane claims what it builds");

        let budget =
            occupied(root.path()) + total_bytes(&candidate_entries(root.path()).unwrap().held);
        enforce_budget(&[root.path().to_path_buf()], budget - 1).unwrap();

        assert!(
            running.join("debug/artifact").exists(),
            "the directory a lane is executing from must survive the ceiling"
        );
        assert!(
            !idle.exists(),
            "the live directory is charged against the ceiling, so the idle one is evicted"
        );
        drop(lane);
    }

    /// The scan only reads; a job that leases an entry the scan offered keeps
    /// it.
    #[test]
    fn an_entry_a_job_leases_after_the_scan_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let lane = build_dir(root.path(), "lint", 1);
        let scanned = candidate_entries(root.path()).unwrap().entries;

        let job = lease::hold(&lane).unwrap();

        assert_eq!(evict(&scanned[0]).unwrap(), 0);
        assert!(lane.join("debug/artifact").exists());
        drop(job);
    }

    #[test]
    fn a_late_lease_does_not_leave_the_budget_unpaid() {
        let root = tempfile::tempdir().unwrap();
        let oldest = build_dir(root.path(), "a-oldest", 4096);
        let idle = build_dir(root.path(), "b-idle", 4096);
        let (candidates, held_bytes) = collect(&[root.path().to_path_buf()]).unwrap();
        assert_eq!(held_bytes, 0);
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|entry| entry.size_bytes > 0 && entry.used == UNIX_EPOCH)
        );
        assert!(oldest < idle);
        let budget = total_bytes(&candidates) - 1;

        let _job = lease::hold(&oldest).unwrap();
        assert!(lease::evict(&oldest).unwrap().is_none());

        evict_to_budget(candidates, held_bytes, budget);

        assert!(oldest.join("debug/artifact").is_file());
        assert!(
            !idle.exists(),
            "the next idle candidate must pay the shortfall"
        );
    }

    /// An evicted entry is moved aside before it is removed, and nothing of
    /// either is left.
    #[test]
    fn an_evicted_entry_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        build_dir(root.path(), "lint", 1);
        let scanned = candidate_entries(root.path()).unwrap().entries;

        assert!(evict(&scanned[0]).unwrap() > 0);

        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    /// An eviction moves an entry aside before it deletes it. One that died
    /// midway leaves the moved directory behind, and no job ever builds there
    /// to make it a candidate.
    #[test]
    fn what_an_interrupted_eviction_left_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join(".evicting-lint-1");
        fs::create_dir_all(left.join("debug")).unwrap();
        fs::write(left.join("debug/artifact"), b"half removed").unwrap();
        fs::write(left.join(lease::FILE), b"").unwrap();

        enforce_budget(&[root.path().to_path_buf()], u64::MAX).unwrap();

        assert!(!left.exists());
    }

    /// A directory a pass cannot remove - a build left part of it read-only,
    /// say - is that directory's problem. The pass still evicts the others,
    /// and so does every later pass that meets it again as a leftover: one
    /// stuck directory must not leave the host's budget unenforced or refuse
    /// every job its room.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_removed_holds_back_no_other() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_path_buf()];
        let stuck = build_dir(root.path(), "stuck", 1);
        let sealed = stuck.join("debug").join("sealed");
        fs::create_dir_all(&sealed).unwrap();
        fs::write(sealed.join("artifact"), b"kept").unwrap();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o555)).unwrap();
        // Leased, so it goes after the stuck one, which no job ever leased.
        let lint = build_dir(root.path(), "lint", 1);
        drop(lease::hold(&lint).unwrap());

        enforce_budget(&roots, 0).unwrap();
        let usdt = build_dir(root.path(), "usdt", 1);
        enforce_budget(&roots, 0).unwrap();

        assert!(!lint.exists(), "{} outlived the pass", lint.display());
        assert!(!usdt.exists(), "{} outlived the pass", usdt.display());
        for left in fs::read_dir(root.path()).unwrap() {
            let sealed = left.unwrap().path().join("debug").join("sealed");
            if sealed.exists() {
                fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    /// The eviction that moved this entry aside is still removing it.
    #[test]
    fn what_another_eviction_is_removing_is_left_to_it() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join(".evicting-lint-1");
        fs::create_dir_all(left.join("debug")).unwrap();
        let removing = lease::evict(&left).unwrap().expect("nobody holds it");

        enforce_budget(&[root.path().to_path_buf()], u64::MAX).unwrap();

        assert!(left.join("debug").exists());
        drop(removing);
    }

    /// The roots are what an executor names beside each checkout; the
    /// checkouts themselves, and the trees inside them, are never read.
    #[cfg(unix)]
    #[test]
    fn the_build_roots_are_the_directories_beside_the_checkouts() {
        let root = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for slot in ["runner-a/0", "runner-b/1"] {
            let checkout = root.path().join(slot).join("disrupt/kithara");
            fs::create_dir_all(checkout.join("target/debug")).unwrap();
            fs::write(checkout.join("Cargo.toml"), b"").unwrap();
            let build_root = checkout.with_file_name("kithara.target");
            build_dir(&build_root, "lint", 1);
            expected.push(build_root);
        }
        std::os::unix::fs::symlink("lint", expected[0].join(consts::BUILD_ALIAS)).unwrap();
        build_dir(&expected[1], consts::XTASK_BUILD, 1);
        fs::create_dir_all(root.path().join("runner-c/0/disrupt/unrelated/debug")).unwrap();

        assert_eq!(build_roots(root.path()).unwrap(), expected);
    }

    /// The build root a GitLab job is told sits beside its checkout under a
    /// hidden name. A host serves `production/main` until this layout merges,
    /// and main's room gate walks every directory a builds tree holds that is
    /// not hidden: in a root a job is building in, that walk meets names gone
    /// before it reads them and refuses main's job. The budget still finds the
    /// root.
    #[cfg(unix)]
    #[test]
    fn a_gitlab_jobs_build_root_is_hidden_beside_its_checkout_and_budgeted() {
        let workspace = crate::ci::config::workspace_root();
        let common: serde_yaml_ng::Value = serde_yaml_ng::from_str(
            &fs::read_to_string(workspace.join(".gitlab/ci/common.yml")).unwrap(),
        )
        .unwrap();
        let told = common[".builds-beside-checkout"]["variables"]["CARGO_TARGET_DIR"]
            .as_str()
            .expect("a GitLab job is told a build root");
        let builds = tempfile::tempdir().unwrap();
        let checkout = builds.path().join("runner/0/disrupt/kithara");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), b"").unwrap();
        let root = PathBuf::from(
            told.replace("${CI_PROJECT_DIR}", checkout.to_str().unwrap())
                .replace("${CI_PROJECT_NAME}", "kithara"),
        );
        build_dir(&root, "lint", 1);
        std::os::unix::fs::symlink("lint", root.join(consts::BUILD_ALIAS)).unwrap();
        let root = fs::canonicalize(&root).unwrap();

        assert_eq!(root.parent(), fs::canonicalize(&checkout).unwrap().parent());
        assert!(is_hidden(&root), "{} is not hidden", root.display());
        let found: Vec<PathBuf> = build_roots(builds.path())
            .unwrap()
            .iter()
            .map(|found| fs::canonicalize(found).unwrap())
            .collect();
        assert_eq!(found, [root]);
    }

    /// A build alias is a link `ci lane` points at the lane's own directory
    /// once it knows the lane. Leased through before that, it claimed the last
    /// lane's directory for this job, or became a directory where the link
    /// belongs.
    #[test]
    fn a_build_alias_is_not_leased_through() {
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join(consts::BUILD_ALIAS);
        let checkout = git_checkout(&[]);
        let told = alias.clone().into_os_string();

        let held = hold_target(checkout.path(), &|name| {
            (name == "CARGO_TARGET_DIR").then(|| told.clone())
        })
        .unwrap();

        assert!(held.is_none());
        assert!(
            fs::symlink_metadata(&alias).is_err(),
            "nothing stands where the alias goes"
        );
    }

    /// A job that names its own build directory in the build root, beside
    /// the alias the lanes build through, claims it as a lane claims its own:
    /// left unclaimed, it would build against the stamps the last lane's claim
    /// gave the checkout, which say nothing about what it built.
    #[cfg(unix)]
    #[test]
    fn a_ci_job_claims_the_directory_it_names_beside_the_alias() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("lint", root.path().join(consts::BUILD_ALIAS)).unwrap();
        let named = root.path().join("network");
        let checkout = git_checkout(&[("src/lib.rs", "")]);
        let told = named.clone().into_os_string();

        let held = hold_target(checkout.path(), &|name| match name {
            "CARGO_TARGET_DIR" => Some(told.clone()),
            "CI" => Some(OsString::from("true")),
            _ => None,
        })
        .unwrap();

        assert!(held.is_some());
        assert!(
            named.join(consts::SOURCES_RECORD).is_file(),
            "the job built in its directory without claiming it"
        );
    }

    /// A sweep runs over a cache a job is building in, so a name the listing
    /// returned can be gone before it is read. Treating that as a failure
    /// aborted the whole sweep, and the budget it exists to enforce was never
    /// applied: the disk kept growing while the timer reported a failed unit.
    #[test]
    fn an_entry_a_live_build_removed_mid_sweep_is_skipped() {
        let gone = still_there::<()>(Err(io::Error::from(io::ErrorKind::NotFound)))
            .expect("a vanished entry is not a failure");
        assert!(gone.is_none(), "a vanished entry is counted as nothing");

        let refused = still_there::<()>(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(
            refused.is_err(),
            "a cache this sweep cannot read is a real failure, not a race"
        );

        let directory = tempfile::tempdir().expect("temporary directory");
        let present = still_there(fs::symlink_metadata(directory.path()))
            .expect("reading an entry that is there")
            .expect("an entry that is there is measured");
        assert!(present.file_type().is_dir());
    }
}
