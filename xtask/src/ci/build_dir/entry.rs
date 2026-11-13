use std::{
    ffi::OsStr,
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
    process,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use fs4::TryLockError;
use kithara_devtools::{
    lease::{self, Lease},
    lock::FileLock,
};
use tracing::{info, warn};

use super::{
    sources::{Claim, claim},
    timings,
};
use crate::{ci::build_cache, consts};

/// A job's hold on its build directory: a slot of its lane, taken, leased,
/// with the alias naming it and the checkout claimed for it, for as long as
/// this lives. The claim settles before the lease goes, and the lease before
/// the slot.
#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, deref = false)]
pub(crate) struct BuildDir {
    #[field(get, vis = "pub(crate)")]
    path: PathBuf,
    _sources: Claim,
    _lease: Lease,
    _slot: FileLock,
}

impl BuildDir {
    /// Enters a build of `lane` under `slots`: takes the lane's first slot no
    /// other job holds, leases it, points `alias` at it, removes the units its
    /// builds no longer ask for, by `window` (see [`super::garbage`]), removes
    /// the timing reports earlier jobs left, and claims `checkout` for it
    /// (see [`super::sources`]).
    ///
    /// # Errors
    ///
    /// When `alias` is not named `build`, `lane` is not one plain name that is
    /// not hidden, or the slot, the lease, the link or the claim cannot be
    /// made, or earlier timing reports cannot be removed. A garbage pass that
    /// fails is logged: it only saves disk.
    pub(crate) fn enter(
        checkout: &Path,
        alias: &Path,
        slots: &Path,
        lane: &str,
        window: Duration,
    ) -> Result<Self> {
        ensure!(
            alias.file_name() == Some(OsStr::new(consts::BUILD_ALIAS)),
            "{} names no build alias: an executor names `<root>/{}` and the lane builds behind it",
            alias.display(),
            consts::BUILD_ALIAS
        );
        validate(lane)?;
        let (path, slot) = take(slots, lane)?;
        let lease =
            lease::hold(&path).with_context(|| format!("leasing build {}", path.display()))?;
        point(alias, &path)?;
        if let Err(error) = super::garbage::collect(&path, window) {
            warn!(
                "{error:#}; build {} keeps every unit it holds",
                path.display()
            );
        }
        timings::clear(&path)?;
        let sources = claim(checkout, &path)?;
        Ok(Self {
            path,
            _sources: sources,
            _lease: lease,
            _slot: slot,
        })
    }

    /// Copies this job's timing reports out of its held slot, returning
    /// whether it wrote any.
    ///
    /// # Errors
    ///
    /// When the destination cannot be removed, the slot's targets cannot be
    /// listed, or a report cannot be read or copied.
    pub(crate) fn copy_timings(&self, to: &Path) -> Result<bool> {
        timings::copy(&self.path, to)
    }
}

/// A lane is one plain name, not hidden: an eviction in progress and a staged
/// link are.
fn validate(lane: &str) -> Result<()> {
    let plain = matches!(
        Path::new(lane).components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(name)] if *name == OsStr::new(lane)
    );
    ensure!(
        plain && !lane.starts_with('.'),
        "`{lane}` is not a lane: one plain name, not hidden"
    );
    Ok(())
}

/// The first slot of `lane` under `slots` no job holds, with the lock beside
/// it held. Slot `n` is `<lane>-<n>`, and its lock is never removed, so no job
/// locks a file the budget already deleted. When every slot is held, the next
/// number is a new slot: a job never waits for another's build.
fn take(slots: &Path, lane: &str) -> Result<(PathBuf, FileLock)> {
    fs::create_dir_all(slots)
        .with_context(|| format!("creating lane slots in {}", slots.display()))?;
    for index in 0..usize::MAX {
        let slot = slots.join(format!("{lane}-{index}"));
        let lock = build_cache::lock_beside(&slot);
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock)
            .with_context(|| format!("opening lane slot lock {}", lock.display()))?;
        match FileLock::try_exclusive(file) {
            Ok(held) => {
                info!("building in lane slot {}", slot.display());
                return Ok((slot, held));
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("locking lane slot {}", slot.display()));
            }
        }
    }
    bail!("every slot of lane {lane} in {} is held", slots.display())
}

/// Points `alias` at `build`. The new link is staged under a hidden name and
/// renamed over the old one, so a reader sees one link or the other.
fn point(alias: &Path, build: &Path) -> Result<()> {
    match fs::symlink_metadata(alias) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        // Cargo made a build here for a job that named the alias as its
        // directory without entering one. The root's directories are the
        // build cache's, so it leaves the way an evicted build does.
        Ok(_) => {
            let aside = build_cache::aside(alias)?;
            fs::rename(alias, &aside)
                .with_context(|| format!("moving the build at {} aside", alias.display()))?;
            warn!(
                "{} was a build, not a link; moved aside to {} for the build cache to remove",
                alias.display(),
                aside.display()
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", alias.display()));
        }
    }
    let staged = alias.with_file_name(format!(".{}-{}", consts::BUILD_ALIAS, process::id()));
    match fs::remove_file(&staged) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("removing {}", staged.display()));
        }
    }
    link(build, &staged).with_context(|| format!("linking {}", staged.display()))?;
    fs::rename(&staged, alias)
        .with_context(|| format!("pointing {} at {}", alias.display(), build.display()))
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, at)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use kithara_devtools::lease;
    use tempfile::TempDir;

    use super::BuildDir;
    use crate::{
        ci::{build_cache, build_dir::fixture::git_checkout},
        consts,
    };

    fn alias(root: &Path) -> PathBuf {
        root.join("build")
    }

    /// Enters `lane`'s build through `alias`, in a slot under `slots`, for a
    /// checkout of its own.
    fn enter_in(alias: &Path, slots: &Path, lane: &str) -> anyhow::Result<(BuildDir, TempDir)> {
        let checkout = git_checkout(&[]);
        BuildDir::enter(checkout.path(), alias, slots, lane, consts::GARBAGE_WINDOW)
            .map(|build| (build, checkout))
    }

    /// Enters `lane`'s build in a slot beside `alias`.
    fn enter(alias: &Path, lane: &str) -> anyhow::Result<(BuildDir, TempDir)> {
        enter_in(alias, alias.parent().unwrap(), lane)
    }

    #[test]
    fn entering_points_the_alias_at_the_build_directory() {
        let root = tempfile::tempdir().unwrap();

        let (build, _checkout) = enter(&alias(root.path()), "lint").unwrap();

        assert_eq!(build.path(), &root.path().join("lint-0"));
        assert_eq!(fs::read_link(alias(root.path())).unwrap(), *build.path());
        assert!(build.path().join(lease::FILE).is_file());
    }

    /// Slot entry removes earlier reports so only this job's can be uploaded,
    /// while keeping build files for reuse.
    #[test]
    fn entering_a_slot_removes_the_timing_reports_an_earlier_job_left() {
        let root = tempfile::tempdir().unwrap();
        let slot = root.path().join("lint-0");
        let timings = slot.join("cargo-timings");
        let nested_timings = slot.join("nested/cargo-timings");
        fs::create_dir_all(&timings).unwrap();
        fs::create_dir_all(&nested_timings).unwrap();
        fs::write(timings.join("cargo-timing.html"), "earlier report").unwrap();
        fs::write(
            nested_timings.join("cargo-timing.html"),
            "earlier nested report",
        )
        .unwrap();
        fs::create_dir_all(slot.join("debug")).unwrap();
        let keep = slot.join("debug/keep");
        fs::write(&keep, "build file").unwrap();

        let (_build, _checkout) = enter(&alias(root.path()), "lint").unwrap();

        assert!(
            !timings.exists(),
            "the earlier job's timing directory must go"
        );
        assert!(
            !nested_timings.exists(),
            "the earlier job's nested timing directory must go"
        );
        assert!(keep.is_file(), "slot entry must keep the build file");
    }

    #[test]
    fn the_next_build_re_points_the_alias_and_keeps_the_last_one() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("lint-0/debug")).unwrap();
        drop(enter(&alias(root.path()), "lint").unwrap());

        let (usdt, _checkout) = enter(&alias(root.path()), "usdt").unwrap();

        assert_eq!(fs::read_link(alias(root.path())).unwrap(), *usdt.path());
        assert!(root.path().join("lint-0/debug").is_dir());
    }

    /// Runners that share one directory of lane builds each name theirs through
    /// an alias of their own. A build one job holds is not another's to build
    /// in, so the next job of the lane takes the next slot rather than wait,
    /// and a slot let go is the first the job after takes.
    #[test]
    fn a_held_slot_sends_the_next_job_of_the_lane_to_the_next_slot() {
        let slots = tempfile::tempdir().unwrap();
        let [first, second, third] = [(); 3].map(|()| tempfile::tempdir().unwrap());

        let (held, _checkout) = enter_in(&alias(first.path()), slots.path(), "lint").unwrap();
        let (beside, _beside) = enter_in(&alias(second.path()), slots.path(), "lint").unwrap();

        assert_eq!(held.path(), &slots.path().join("lint-0"));
        assert_eq!(beside.path(), &slots.path().join("lint-1"));
        assert_eq!(fs::read_link(alias(first.path())).unwrap(), *held.path());
        assert_eq!(fs::read_link(alias(second.path())).unwrap(), *beside.path());

        drop(held);
        let (again, _again) = enter_in(&alias(third.path()), slots.path(), "lint").unwrap();
        assert_eq!(again.path(), &slots.path().join("lint-0"));
    }

    #[test]
    fn an_entered_build_directory_is_leased() {
        let root = tempfile::tempdir().unwrap();

        let (build, _checkout) = enter(&alias(root.path()), "lint").unwrap();

        assert!(lease::evict(build.path()).unwrap().is_none());
    }

    /// A directory standing where the alias goes is a build Cargo made for a
    /// job that named the alias as its directory without entering a build.
    /// The root's directories are the build cache's, so it leaves the way an
    /// evicted build does, and the next lane still enters its own.
    #[test]
    fn a_directory_at_the_alias_is_moved_aside_for_the_build_cache_to_remove() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(alias(root.path()).join("debug")).unwrap();

        let (build, _checkout) = enter(&alias(root.path()), "lint").unwrap();

        assert_eq!(fs::read_link(alias(root.path())).unwrap(), *build.path());
        let moved: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.join("debug").is_dir())
            .collect();
        let [moved] = moved.as_slice() else {
            panic!("the build at the alias is moved, not copied or lost: {moved:?}");
        };
        drop(build);
        build_cache::enforce_budget(&[root.path().to_path_buf()], u64::MAX).unwrap();
        assert!(!moved.exists(), "{} is left behind", moved.display());
    }

    #[test]
    fn a_lane_is_one_plain_name_that_is_not_hidden() {
        let root = tempfile::tempdir().unwrap();
        for lane in ["", ".evicting-lint", "lint/flash-off", ".."] {
            assert!(
                enter(&alias(root.path()), lane).is_err(),
                "`{lane}` is not a lane"
            );
        }
    }

    #[test]
    fn an_alias_is_named_build() {
        let root = tempfile::tempdir().unwrap();

        let error = enter(&root.path().join("target"), "lint").unwrap_err();

        assert!(format!("{error:#}").contains("build"), "{error:#}");
    }
}
