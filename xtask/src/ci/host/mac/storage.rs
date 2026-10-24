#[cfg(test)]
use std::{cell::RefCell, collections::VecDeque};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Output,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::runners::{docker_host, docker_socket};
#[cfg(test)]
use crate::ci::process::Recording;
use crate::{
    ci::{
        build_cache,
        config::CiConfig,
        environment::{CacheTrust, scratch_root},
        process::Process,
    },
    consts,
};

/// Whether a stale tree still has to be asked if anything is using it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Liveness {
    /// A workspace is leased and handed to job after job, so its timestamp says
    /// nothing about whether one is running in it right now.
    Ask,
    /// A scratch entry belongs to one job and is never handed to another, so
    /// its age is the whole answer. Asking anyway costs an `lsof` walk per
    /// entry, and the backlog this was written for had reached sixty-eight
    /// thousand of them: hours inside a pass that runs every five minutes,
    /// with every other step in the ladder waiting behind it.
    Age,
}

/// Ordered least to most urgent: the watchdog reports the worst volume, not
/// the total, so that a roomy one cannot hide a full one.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Pressure {
    Normal,
    Soft,
    Aggressive,
    Reject,
}

pub(super) struct HostStorage<'a> {
    host_root: PathBuf,
    build_root: PathBuf,
    /// Where the lanes put their temporaries. Absolute and shared by every job
    /// on the machine, so the test constructor points it inside its own fixture
    /// rather than let a test sweep the real one.
    scratch_root: PathBuf,
    config: &'a CiConfig,
    process: &'a Process,
    /// What the volume reports as free, so a test states one world rather than
    /// a pressure and a free-space reading that can contradict each other.
    #[cfg(test)]
    available_sequence: RefCell<VecDeque<u64>>,
}

#[derive(Serialize)]
struct Health<'a> {
    /// What is left where there is least of it. The thresholds are free-space
    /// floors, so this is the number `pressure` was read from — reporting bytes
    /// spent instead described a quantity no decision uses.
    free_bytes: u64,
    pressure: Pressure,
    /// Named so an operator can see which volume is under pressure without
    /// running `df` by hand.
    volumes: Vec<VolumeHealth>,
    /// Every agent that has to be running for work to reach this host, by label
    /// suffix. Watching only the `gitlab-runner` agent missed the one whose death
    /// stops the work without stopping anything else.
    agents: BTreeMap<&'a str, &'static str>,
    timestamp: u64,
}

#[derive(Serialize)]
struct VolumeHealth {
    path: String,
    free_bytes: u64,
    total_bytes: u64,
    pressure: Pressure,
}

struct Volume {
    path: PathBuf,
    total: u64,
    available: u64,
}

/// One entry of `tart list --format json`. Only liveness is read here; the
/// sizes it also reports are apparent and say nothing about what the volume
/// pays for a sparse disk image.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TartVm {
    name: String,
    running: bool,
}

impl Volume {
    fn read(path: &Path) -> Result<Self> {
        let total = fs4::total_space(path)
            .with_context(|| format!("reading total space for {}", path.display()))?;
        let available = fs4::available_space(path)
            .with_context(|| format!("reading available space for {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            total,
            available,
        })
    }
}

impl<'a> HostStorage<'a> {
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    pub(super) fn new(config: &'a CiConfig, process: &'a Process) -> Result<Self> {
        let host_root = config.host.host_root.clone();
        let build_root = config.host.build_root().to_path_buf();
        validate_root(&host_root)?;
        validate_root(&build_root)?;
        Ok(Self {
            host_root,
            build_root,
            scratch_root: scratch_root(),
            config,
            process,
            #[cfg(test)]
            available_sequence: RefCell::new(VecDeque::new()),
        })
    }

    #[cfg(test)]
    fn for_test(config: &'a CiConfig, process: &'a Process) -> Result<Self> {
        let host_root = config.host.host_root.clone();
        let build_root = config.host.build_root().to_path_buf();
        validate_root(&host_root)?;
        validate_root(&build_root)?;
        Ok(Self {
            scratch_root: host_root.join("scratch"),
            host_root,
            build_root,
            config,
            process,
            available_sequence: RefCell::new(VecDeque::new()),
        })
    }

    #[cfg(test)]
    fn set_available_sequence(&mut self, sequence: impl IntoIterator<Item = u64>) {
        self.available_sequence = RefCell::new(sequence.into_iter().collect());
    }

    pub(super) fn preflight(&self) -> Result<()> {
        let free = self.free_bytes()?;
        let (pressure, volume) = self.worst_pressure()?;
        match pressure {
            Pressure::Reject => bail!("{} is full; new jobs stop here", volume.path.display()),
            Pressure::Soft | Pressure::Aggressive => {
                warn!(volume = %volume.path.display(), ?pressure, "CI volume is under pressure");
            }
            Pressure::Normal => {}
        }

        for name in ["cache", "logs", "toolchains", "vm", "workspaces"] {
            let directory = self.host_root.join(name);
            if !directory.is_dir() {
                bail!("missing CI directory: {}", directory.display());
            }
            writable_probe(&directory)?;
        }
        let workspaces = self.build_root.join("workspaces");
        if !workspaces.is_dir() {
            bail!("missing CI directory: {}", workspaces.display());
        }
        writable_probe(&workspaces)?;
        self.process.require_tools(&["git", "kache", "sccache"])?;
        info!(free_bytes = free, ?pressure, "host preflight passed");
        Ok(())
    }

    pub(super) fn cleanup(&self) -> Result<()> {
        // The worst single volume, not the sum: adding a second volume's bytes
        // to the first and comparing that against thresholds calibrated for the
        // first reads every machine with a guest volume as full, and the branch
        // it reaches for throws away the compiler caches that were never the
        // problem. `preflight` already decides this way.
        //
        // Free space is read off that same volume rather than measured again:
        // the volume with the worst pressure is the one with the least left, so
        // a second reading can only disagree with the verdict it annotates.
        let (pressure, volume) = self.worst_pressure()?;
        let initial = volume.available;
        info!(free_bytes = initial, ?pressure, volume = %volume.path.display(), "cleanup started");

        self.prune_host_trees("workspaces/tmp", Self::DAY)?;
        self.prune_scratch_trees(Self::DAY)?;
        self.prune_host_trees("workspaces/builds", Self::DAY)?;
        self.prune_host_trees("vm/overlays", Self::DAY)?;
        self.prune_host_trees("vm/android/avd", Self::DAY)?;
        self.prune_abandoned_job_vms(Self::DAY);
        self.prune_host_files("logs", 14 * Self::DAY)?;
        self.rotate_logs()?;
        self.prune_retired_caches(7 * Self::DAY)?;

        let build_roots = build_cache::budget_roots(
            &self.build_root.join("workspaces/gitlab"),
            &self.host_root.join("cache"),
        )?;
        build_cache::enforce_budget(&build_roots, self.config.host.build_cache_budget_bytes()?)?;

        // Cargo targets are the largest reproducible caches and already have a
        // bounded owner. Re-read pressure after enforcing that budget so a
        // successful trim does not throw away review compiler artifacts too.
        let (cache_pressure, _) = self.worst_pressure()?;
        match cache_pressure {
            Pressure::Soft => {
                self.prune_host_trees("cache/quarantine", 7 * Self::DAY)?;
                self.prune_host_trees("cache/review", 30 * Self::DAY)?;
            }
            Pressure::Aggressive | Pressure::Reject => {
                self.prune_host_trees("cache/quarantine", Duration::ZERO)?;
                self.prune_host_trees("cache/review", Duration::ZERO)?;
                self.prune_host_trees("cache/trusted", 7 * Self::DAY)?;
                self.prune_host_trees("vm/tart/cache", 7 * Self::DAY)?;
            }
            Pressure::Normal => {}
        }

        self.prune_docker_build_cache(self.config.host.docker_build_cache_budget_bytes()?);
        // Unconditional, and after the pruning above, because the guest frees
        // blocks on its own schedule and holds them until asked. Its root
        // filesystem is mounted `discard` and stays at a gigabyte, but the data
        // disk carrying `/var/lib/docker` is not, so every layer Docker deletes
        // stays allocated in a file this volume pays for. One trim on a machine
        // that had drifted to 44 GB free returned 63 of them in seconds — too
        // cheap to hold back for a threshold, and holding it back is what let
        // the drift reach refusal five times.
        self.trim_linux_guest();

        // The warm builds go last. Every step above takes stale state, a
        // compiler cache or Docker's; an evicted build costs the next job of
        // its lane a cold build of the workspace. Spent first, they went on every
        // `Aggressive` pass while the steps after them would have covered the
        // shortfall alone.
        let (mut final_pressure, mut final_volume) = self.worst_pressure()?;
        if final_pressure >= Pressure::Aggressive {
            build_cache::reclaim_at_least(
                &build_roots,
                self.shortfall_to_the_floor(&final_volume),
            )?;
            (final_pressure, final_volume) = self.worst_pressure()?;
        }
        if final_pressure == Pressure::Reject {
            self.prune_host_trees("cache/trusted", Duration::ZERO)?;
            self.prune_retired_caches(Duration::ZERO)?;
            (final_pressure, final_volume) = self.worst_pressure()?;
        }
        let free = final_volume.available;
        info!(
            free_bytes = free,
            freed_bytes = free.saturating_sub(initial),
            ?final_pressure,
            "cleanup completed"
        );
        // A pass that cannot reach the threshold has to say what is holding the
        // space. Saying only that every step reached its floor is what made this
        // recur: the message rules out the caches and names nothing, so each
        // recurrence starts from nothing and the trees no step owns are never
        // even looked at. Measured only on the way to a failure, so a healthy
        // pass pays nothing for it.
        let holders = if final_pressure >= Pressure::Aggressive {
            let holders = self.largest_trees(10);
            for (path, bytes) in &holders {
                info!(path = %path.display(), apparent_bytes = bytes, "CI space holder");
            }
            holders
        } else {
            Vec::new()
        };
        if final_pressure == Pressure::Reject {
            bail!(
                "CI volume remains above the new-job threshold after cleanup; largest trees: {}",
                name_holders(&holders)
            );
        }
        // A pass that runs every step and moves nothing is the failure this
        // host spent hours in: `Aggressive` in, `Aggressive` out, `bytes_freed=0`
        // from each budget, and `cleanup completed` in the log every hour while
        // jobs were already being refused. Reporting success there is what made
        // it invisible — the steps ran, so nothing looked broken, and the space
        // was held by things no step owns. Relief is the result this is for;
        // performing the steps is not.
        if final_pressure >= Pressure::Aggressive && final_pressure >= pressure {
            bail!(
                "cleanup left {} at {final_pressure:?} with {free} bytes free, unchanged from \
                 {pressure:?}: every step this owns is already at its floor, so the space is held \
                 by {}",
                volume.path.display(),
                name_holders(&holders)
            );
        }
        Ok(())
    }

    pub(super) fn health(&self) -> Result<()> {
        let free = self.free_bytes()?;
        let volumes: Vec<VolumeHealth> = self
            .volumes()?
            .into_iter()
            .map(|volume| VolumeHealth {
                path: volume.path.display().to_string(),
                free_bytes: volume.available,
                total_bytes: volume.total,
                pressure: self.pressure_of(&volume),
            })
            .collect();
        let (pressure, worst) = self.worst_pressure()?;
        let agents = self.agent_states();
        let down: Vec<&str> = agents
            .iter()
            .filter(|(_, state)| ![consts::AGENT_RUNNING, consts::AGENT_ABSENT].contains(*state))
            .map(|(name, _)| *name)
            .collect();
        serde_json::to_writer(
            io::stdout().lock(),
            &Health {
                free_bytes: free,
                pressure,
                volumes,
                agents,
                timestamp: unix_time()?,
            },
        )
        .context("writing host health JSON")?;
        io::stdout()
            .write_all(b"\n")
            .context("terminating host health JSON")?;
        if pressure == Pressure::Reject {
            bail!("{} is above the new-job threshold", worst.path.display());
        }
        if !down.is_empty() {
            bail!("CI agents are not running: {}", down.join(", "));
        }
        Ok(())
    }

    /// What launchd says about each agent work depends on.
    ///
    /// On a host with no launchd there is nothing to say, and this must not
    /// invent a fault: the Linux executor runs the same command.
    fn agent_states(&self) -> BTreeMap<&'a str, &'static str> {
        if !Path::new(consts::LAUNCHCTL).is_file() {
            return self
                .config
                .host
                .always_on_agents
                .iter()
                .map(|name| (name.as_str(), consts::AGENT_ABSENT))
                .collect();
        }
        let listing = self
            .process
            .capture(consts::LAUNCHCTL, &["list"], "launchd agent listing")
            .unwrap_or_default();
        agent_states_from(&listing, &self.config.host.always_on_agents)
    }

    /// What is left on the volume with the least to spare — the one the pressure
    /// verdict comes from, since every volume is judged against the same floors.
    ///
    /// Summing bytes spent across volumes measured neither: on a shared APFS
    /// container a volume's used space counts what its neighbours hold, so the
    /// total ran past the quota it was being compared with. This host reported
    /// 470 GB spent on a 300 GB quota while it had 44 GB free.
    fn free_bytes(&self) -> Result<u64> {
        self.volumes()?
            .into_iter()
            .map(|volume| volume.available)
            .min()
            .context("no CI volume to measure")
    }

    /// Every volume CI storage sits on, in the order they should be reported.
    ///
    /// The guest images can be given a volume of their own, so that one guest
    /// growing cannot refuse work for lanes that never touch it. `df` on the
    /// checkout root then stops seeing them entirely, and a watchdog blind to
    /// the largest consumer on the machine is worse than one watching a single
    /// shared volume. Anything reached through `vm` that turns out to live on
    /// another filesystem is measured as well.
    fn volumes(&self) -> Result<Vec<Volume>> {
        let mut volumes = vec![Volume::read(&self.host_root)?];
        if self.build_root != self.host_root
            && let Ok(build_root) = self.build_root.canonicalize()
            && !build_root.starts_with(&self.host_root)
        {
            volumes.push(Volume::read(&build_root)?);
        }
        let guests = self.host_root.join("vm");
        if guests.is_dir()
            && let Ok(guests) = guests.canonicalize()
            && !guests.starts_with(&self.host_root)
            && !volumes
                .iter()
                .any(|volume| guests.starts_with(&volume.path))
        {
            volumes.push(Volume::read(&guests)?);
        }
        Ok(volumes)
    }

    /// The worst any single volume is doing, and which one that is.
    ///
    /// Summing them would let a roomy volume hide a full one, which is the
    /// failure this split exists to prevent.
    fn worst_pressure(&self) -> Result<(Pressure, Volume)> {
        #[cfg(test)]
        if let Some(available) = self.available_sequence.borrow_mut().pop_front() {
            let volume = Volume {
                path: self.host_root.clone(),
                total: self.config.host.quota_bytes,
                available,
            };
            return Ok((self.pressure_of(&volume), volume));
        }
        let mut volumes = self.volumes()?.into_iter();
        let first = volumes.next().context("no CI volume to measure")?;
        let mut worst = (self.pressure_of(&first), first);
        for volume in volumes {
            let pressure = self.pressure_of(&volume);
            if pressure > worst.0 {
                worst = (pressure, volume);
            }
        }
        Ok(worst)
    }

    /// Pressure is what is left, not what was spent.
    ///
    /// The thresholds are written as bytes used against `quota_bytes`, and this
    /// reads them as the free space each one intends to keep: a volume at the
    /// reject threshold is one with `quota - reject` bytes to spare. On an APFS
    /// container the volume shares, the two are not the same question. This one
    /// measured 279 GB with 170 used — never within 100 GB of a 285 GB reject
    /// threshold, so cleanup stayed `Normal` and never recycled the guest —
    /// while jobs were already being refused for having 10 GB free where the
    /// preflight wants 15.
    ///
    /// Read this way the ladder and the refusal agree by construction:
    /// `quota - reject` is exactly the free space a job is required to find.
    fn pressure_of(&self, volume: &Volume) -> Pressure {
        pressure_for(
            volume.available,
            self.floor(self.config.host.soft_cleanup_bytes),
            self.floor(self.config.host.aggressive_cleanup_bytes),
            self.floor(self.config.host.reject_bytes),
        )
    }

    /// The free space a used-bytes threshold was asking for.
    fn floor(&self, threshold: u64) -> u64 {
        self.config.host.quota_bytes.saturating_sub(threshold)
    }

    /// What a volume is short of the floor the soft threshold asks for.
    ///
    /// The build cache budget is a ceiling, and a ceiling answers a question no
    /// job asks: it caps one cache without knowing whether the volume has room.
    /// Two checkouts holding 88 and 95 GB were each under the 100 GB ceiling, so
    /// every hourly pass reported `bytes_freed=0` while the volume they share
    /// sat below the floor and jobs were already being refused for space.
    fn shortfall_to_the_floor(&self, volume: &Volume) -> u64 {
        self.floor(self.config.host.soft_cleanup_bytes)
            .saturating_sub(volume.available)
    }

    /// Cache namespaces nothing writes to any more, once they have gone quiet
    /// for a week.
    fn prune_retired_caches(&self, age: Duration) -> Result<()> {
        let directory = self.host_root.join("cache");
        if !directory.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("reading cache directory {}", directory.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if self
                .config
                .host
                .cache_namespaces
                .iter()
                .map(String::as_str)
                .chain(consts::PREVIOUS_CACHE_NAMESPACES)
                .any(|namespace| namespace == name.as_ref())
            {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_dir() || !older_than(&metadata, age)? {
                continue;
            }
            if self.active(&path) {
                info!(path = %path.display(), "keeping active CI path");
                continue;
            }
            info!(path = %path.display(), "removing retired cache namespace");
            self.remove_path(&path)?;
        }
        Ok(())
    }

    fn prune_host_trees(&self, relative: &str, age: Duration) -> Result<()> {
        self.prune_old_trees(&self.host_root, relative, age, Liveness::Ask)
    }

    /// Prune what the lanes leave behind in their scratch root.
    ///
    /// `CiEnvironment` points every lane's `TMPDIR` at a namespace under
    /// [`scratch_root`], which sits outside both CI roots for three reasons of
    /// its own — see its doc comment — so no step in this ladder could reach it
    /// and nothing ever came back for it. A job killed before its temporaries
    /// drop leaves them where they are, and cancellation is routine for fork
    /// branches, so the leak is steady rather than exceptional.
    ///
    /// Measured on the CI host 2026-08-22: sixty-eight thousand directories
    /// holding 3.5 gibibytes across the three namespaces, the oldest from
    /// 18 August, growing by roughly 660 mebibytes a day — on the same APFS
    /// container whose free space every pressure threshold is read from.
    ///
    /// Age is the whole signal here, unlike the workspaces beside it: a
    /// namespace entry belongs to one job and is never handed to another, so
    /// its timestamp is its creation, and a day is more than ten times the
    /// longest lane. The namespaces themselves are never candidates, because
    /// `prune_old_trees` only ever offers their children.
    fn prune_scratch_trees(&self, age: Duration) -> Result<()> {
        for trust in CacheTrust::ALL {
            self.prune_old_trees(&self.scratch_root, trust.as_str(), age, Liveness::Age)?;
        }
        Ok(())
    }

    fn prune_old_trees(
        &self,
        root: &Path,
        relative: &str,
        age: Duration,
        liveness: Liveness,
    ) -> Result<()> {
        let directory = root.join(relative);
        if !directory.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("reading cleanup directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_dir() || !older_than(&metadata, age)? {
                continue;
            }
            if liveness == Liveness::Ask && self.active(&path) {
                info!(path = %path.display(), "keeping active CI path");
                continue;
            }
            self.remove_path(&path)?;
        }
        Ok(())
    }

    fn prune_host_files(&self, relative: &str, age: Duration) -> Result<()> {
        let directory = self.host_root.join(relative);
        if !directory.is_dir() {
            return Ok(());
        }
        self.prune_old_files_recursive(&directory, age)
    }

    fn prune_old_files_recursive(&self, directory: &Path, age: Duration) -> Result<()> {
        let mut subdirectories = Vec::new();
        for entry in fs::read_dir(directory)
            .with_context(|| format!("reading cleanup directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                if older_than(&metadata, age)? {
                    self.remove_path(&path)?;
                }
            } else if metadata.is_dir() {
                self.prune_old_files_recursive(&path, age)?;
                subdirectories.push(path);
            } else if metadata.is_file() && older_than(&metadata, age)? {
                self.remove_path(&path)?;
            }
        }
        for directory in subdirectories {
            if fs::read_dir(&directory)?.next().is_none() {
                self.remove_path(&directory)?;
            }
        }
        Ok(())
    }

    fn rotate_logs(&self) -> Result<()> {
        let directory = self.host_root.join("logs");
        if !directory.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("reading log directory {}", directory.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("log")
                || fs::symlink_metadata(&path)?.len() <= self.config.host.log_limit_bytes
            {
                continue;
            }
            let numbered = |number| PathBuf::from(format!("{}.{number}", path.display()));
            self.remove_path(&numbered(5))?;
            for number in (1..=4).rev() {
                let source = numbered(number);
                if source.exists() {
                    fs::rename(&source, numbered(number + 1))
                        .with_context(|| format!("rotating log {}", source.display()))?;
                }
            }
            fs::copy(&path, numbered(1))
                .with_context(|| format!("copying rotated log {}", path.display()))?;
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .with_context(|| format!("truncating rotated log {}", path.display()))?;
        }
        Ok(())
    }

    fn active(&self, path: &Path) -> bool {
        let lease_active = self.lease_directory_active(&path.join(".kithara-ci-leases"));
        let legacy_active = self.legacy_marker_active(&path.join(".kithara-ci-active"));
        if lease_active || legacy_active {
            return true;
        }
        self.process
            .command("/usr/sbin/lsof")
            .arg("-t")
            .arg("+D")
            .arg(path)
            .output()
            .as_ref()
            .is_ok_and(reports_open_files)
    }

    fn lease_directory_active(&self, leases: &Path) -> bool {
        match fs::symlink_metadata(leases) {
            Ok(metadata) if metadata.file_type().is_dir() => self.owner_lease_active(leases),
            Ok(_) => {
                warn!(path = %leases.display(), "unsupported CI cache lease directory type");
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                warn!(path = %leases.display(), %error, "could not inspect CI cache leases");
                true
            }
        }
    }

    fn legacy_marker_active(&self, legacy: &Path) -> bool {
        match fs::symlink_metadata(legacy) {
            Ok(metadata) if metadata.file_type().is_file() => {
                self.lease_is_fresh(legacy, &metadata)
            }
            Ok(_) => {
                warn!(path = %legacy.display(), "unsupported legacy CI cache lease type");
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                warn!(path = %legacy.display(), %error, "could not inspect legacy CI cache lease");
                true
            }
        }
    }

    fn owner_lease_active(&self, directory: &Path) -> bool {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(path = %directory.display(), %error, "could not read CI cache leases");
                return true;
            }
        };
        let mut active = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warn!(path = %directory.display(), %error, "could not read a CI cache lease");
                    active = true;
                    continue;
                }
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => {
                    warn!(path = %path.display(), "unsupported CI cache lease type");
                    active = true;
                    continue;
                }
                Err(error) => {
                    warn!(path = %path.display(), %error, "could not inspect CI cache lease");
                    active = true;
                    continue;
                }
            };
            active |= self.lease_is_fresh(&path, &metadata);
        }
        active
    }

    fn lease_is_fresh(&self, marker: &Path, metadata: &fs::Metadata) -> bool {
        let lease = Duration::from_secs(self.config.host.active_lease_hours * 60 * 60);
        match older_than(metadata, lease) {
            Ok(false) => true,
            Ok(true) => {
                warn!(path = %marker.display(), "removing stale CI cache lease");
                if let Err(error) = fs::remove_file(marker) {
                    warn!(path = %marker.display(), %error, "could not remove stale CI cache lease");
                }
                false
            }
            Err(error) => {
                warn!(path = %marker.display(), %error, "could not read CI cache lease age");
                true
            }
        }
    }

    fn remove_path(&self, path: &Path) -> Result<()> {
        if !self.is_removable(path) {
            bail!("refusing to remove unsafe CI path: {}", path.display());
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
        } else {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
        }
    }

    /// Delete the macOS job VM once no runner is serving from it.
    ///
    /// The macOS lane clones [`JOB_VM_NAME`](consts::JOB_VM_NAME) from the base bundle and destroys
    /// it at both ends of every runner loop, so the clone is disposable by
    /// construction: a runner that finds it missing makes another. When the
    /// runner dies between those ends the clone outlives it, and nothing ever
    /// comes back for it. `vm/tart/cache` is pruned by age; the bundle
    /// directory beside it is not, because pruning that directory by age would
    /// take the base bundle with it, and the base is only rebuildable by hand
    /// with `tart create --from-ipsw`.
    ///
    /// Seen on the CI host 2026-08-22: a stopped clone from 12 August, ten days
    /// after the runner that made it was last loaded, while cleanup reported
    /// `Normal` and freed nothing every five minutes.
    ///
    /// What that clone returned is the measure to trust, and it was almost
    /// nothing: `tart clone` copies on write, so a clone shares its blocks with
    /// the base until it diverges, and that one had never been booted. A
    /// directory walk counted its 38 gibibytes twice over — once in each bundle
    /// — because both files hold the same blocks. So this reclaims what the
    /// dead runner wrote, not what the bundle appears to weigh: nothing for a
    /// clone that never ran, and the whole divergence for one that served jobs
    /// before its runner died. It is worth doing for the second case, and no
    /// apparent size should be read as the space it will return.
    ///
    /// Named rather than swept, so the base bundle is never a candidate, and
    /// taken apart through `tart` rather than the filesystem, so the tool that
    /// owns the bundle is the one that dismantles it. Both signals are
    /// required, because neither can tell an idle runner from a dead one on its
    /// own: the loop leaves the clone stopped for as long as a boot takes,
    /// which is longer than the interval this runs on, and a booted guest
    /// waiting for work writes nothing to its bundle for hours.
    fn prune_abandoned_job_vms(&self, age: Duration) {
        let tart = self.config.host.brew_tool("tart");
        let Ok(home) = self.config.host.tart_home() else {
            return;
        };
        let bundle = home.join("vms").join(consts::JOB_VM_NAME);
        if !tart.is_file() || !bundle.is_dir() {
            return;
        }
        let Some(touched) = newest_part_mtime(&bundle) else {
            return;
        };
        if !older_than_time(touched, age) || self.job_vm_is_running(&tart) {
            return;
        }
        info!(vm = consts::JOB_VM_NAME, bundle = %bundle.display(), "removing abandoned macOS job VM");
        let mut command = self.process.command(&tart);
        command.args(["delete", consts::JOB_VM_NAME]);
        if let Err(error) = self
            .process
            .run_command(&mut command, "delete abandoned CI macOS VM")
        {
            warn!(%error, "abandoned macOS job VM cleanup failed");
        }
    }

    /// Whether `tart` reports the job VM as running.
    ///
    /// A question that cannot be answered answers `true`: an unreadable listing
    /// must not become permission to delete a guest that may be serving jobs.
    fn job_vm_is_running(&self, tart: &Path) -> bool {
        let Ok(output) = self
            .process
            .command(tart)
            .args(["list", "--format", "json"])
            .output()
        else {
            return true;
        };
        if !output.status.success() {
            return true;
        }
        let Ok(listed) = serde_json::from_slice::<Vec<TartVm>>(&output.stdout) else {
            return true;
        };
        listed
            .iter()
            .any(|vm| vm.name == consts::JOB_VM_NAME && vm.running)
    }

    /// Hold the guest's build cache to `budget` bytes, least recently used
    /// first. Its disk is a sparse file this volume pays for, so a cache that
    /// grows unbounded between passes is space no other step can reach.
    fn prune_docker_build_cache(&self, budget: u64) {
        let home = self.host_root.join("home").join(&self.config.host.ci_user);
        let socket = docker_socket(&home, &self.config.host.colima_profile);
        let docker = self.config.host.brew_tool("docker");
        if !socket.exists() || !docker.is_file() {
            return;
        }
        let mut command = self.process.command(docker);
        command
            .env(
                "DOCKER_HOST",
                docker_host(&home, &self.config.host.colima_profile),
            )
            .args(["builder", "prune", "--force", "--max-used-space"])
            .arg(budget.to_string());
        if let Err(error) = self
            .process
            .run_command(&mut command, "Docker build cache cleanup")
        {
            warn!(%error, "Docker cache cleanup failed");
        }
    }

    /// Hand the guest's freed blocks back to the volume.
    ///
    /// Pruning inside the guest returns nothing here on its own: the data disk
    /// is a sparse file that grows to its high-water mark and keeps every block
    /// it has ever written. Measured on 2026-08-11 it held 77 GB for 25 GB of
    /// images and build cache — fifty gigabytes of blocks the guest had already
    /// released. A discard returns these blocks without destroying the runtime.
    fn trim_linux_guest(&self) {
        let colima = self.config.host.brew_tool("colima");
        if !colima.is_file() {
            return;
        }
        let home = self.host_root.join("home").join(&self.config.host.ci_user);
        let mut command = self.process.command(colima);
        command.env("COLIMA_HOME", home.join(".colima")).args([
            "ssh",
            "--profile",
            self.config.host.colima_profile.as_str(),
            "--",
            "sudo",
            "fstrim",
            "-a",
        ]);
        if let Err(error) = self
            .process
            .run_command(&mut command, "return the Linux guest's freed blocks")
        {
            warn!(%error, "could not trim the Linux guest's disk");
        }
    }

    fn is_removable(&self, target: &Path) -> bool {
        is_removable_under(&self.host_root, target, &self.config.host.removable_roots)
            || is_removable_under(&self.build_root, target, &["workspaces"])
            || is_removable_under(
                &self.scratch_root,
                target,
                &CacheTrust::ALL.map(CacheTrust::as_str),
            )
    }

    /// The largest trees under the CI roots, largest first.
    ///
    /// Measured two levels down, so an answer names a subsystem — `vm/tart`,
    /// `cache/trusted`, one runner's workspaces — rather than the volume it is
    /// already known to be on. A first-level directory with nothing measured
    /// below it stands for itself.
    ///
    /// One `du` over every root, with its duration in the log: the walk this
    /// replaced ran file by file in this process after `cleanup completed`, so
    /// a pass killed at its deadline left no trace of the step it was in. Sizes
    /// are allocated, what the volume pays: a sparse disk image counts what it
    /// holds, not what it could grow to. What `du` cannot read it skips and
    /// says so, exiting non-zero; the rest of its answer still stands, because
    /// this runs while jobs are being refused and a tree vanishing under a
    /// running job is no reason to name nothing.
    fn largest_trees(&self, count: usize) -> Vec<(PathBuf, u64)> {
        let mut roots = vec![self.host_root.clone()];
        if !self.build_root.starts_with(&self.host_root) {
            roots.push(self.build_root.clone());
        }
        let label = "measure the largest CI trees";
        info!(step = label, "starting");
        let started = Instant::now();
        let mut command = self.process.command("du");
        command.args(["-k", "-d", "2"]).args(&roots);
        let output = crate::child::output(&mut command, None, consts::CONTROL_TIMEOUT);
        info!(
            step = label,
            seconds = started.elapsed().as_secs_f64(),
            "done"
        );
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                warn!(%error, "could not measure the CI trees");
                return Vec::new();
            }
        };
        let mut sizes = deepest_trees(&String::from_utf8_lossy(&output.stdout), &roots);
        sizes.sort_unstable_by_key(|(_, bytes)| Reverse(*bytes));
        sizes.truncate(count);
        sizes
    }
}

/// The holders as `path=bytes`, largest first, for a message that is read once
/// and acted on.
fn name_holders(holders: &[(PathBuf, u64)]) -> String {
    if holders.is_empty() {
        return "trees this pass could not measure".to_string();
    }
    holders
        .iter()
        .take(5)
        .map(|(path, bytes)| format!("{}={bytes}", path.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The second-level trees of a `du -k -d 2` listing over `roots`, and the
/// first-level ones with nothing listed below them, in bytes.
fn deepest_trees(listing: &str, roots: &[PathBuf]) -> Vec<(PathBuf, u64)> {
    let entries: Vec<(PathBuf, usize, u64)> = listing
        .lines()
        .filter_map(|line| {
            let (kib, path) = line.split_once('\t')?;
            let bytes = kib.parse::<u64>().ok()?.saturating_mul(1024);
            let path = PathBuf::from(path);
            let depth = roots
                .iter()
                .find_map(|root| path.strip_prefix(root).ok())?
                .components()
                .count();
            Some((path, depth, bytes))
        })
        .collect();
    let parents: BTreeSet<&Path> = entries
        .iter()
        .filter(|(_, depth, _)| *depth == 2)
        .filter_map(|(path, _, _)| path.parent())
        .collect();
    entries
        .iter()
        .filter(|(path, depth, _)| {
            *depth == 2 || (*depth == 1 && !parents.contains(path.as_path()))
        })
        .map(|(path, _, bytes)| (path.clone(), *bytes))
        .collect()
}

fn is_removable_under(root: &Path, target: &Path, removable_roots: &[impl AsRef<str>]) -> bool {
    if !root.is_absolute() || !target.is_absolute() {
        return false;
    }
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    removable_roots
        .iter()
        .any(|allowed| first == std::ffi::OsStr::new(allowed.as_ref()))
        && components.next().is_some()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Floors are free-space limits, so a smaller number is a tighter one: at or
/// below `reject` the volume refuses work.
pub(super) fn pressure_for(available: u64, soft: u64, aggressive: u64, reject: u64) -> Pressure {
    if available <= reject {
        Pressure::Reject
    } else if available <= aggressive {
        Pressure::Aggressive
    } else if available <= soft {
        Pressure::Soft
    } else {
        Pressure::Normal
    }
}

/// `launchctl list` prints `PID  status  label`, and the dash in the PID column
/// is the point: an agent restarted by `KeepAlive` stays loaded while holding no
/// process, which is what a crash loop looks like from outside.
fn agent_states_from<'a>(
    listing: &str,
    always_on: &'a [String],
) -> BTreeMap<&'a str, &'static str> {
    always_on
        .iter()
        .map(|name| {
            let label = format!("com.zvuk.kithara-ci.{name}");
            let state = listing
                .lines()
                .find_map(|line| {
                    let mut columns = line.split_whitespace();
                    let pid = columns.next()?;
                    columns.next()?;
                    (columns.next()? == label).then(|| {
                        if pid == "-" {
                            "stopped"
                        } else {
                            consts::AGENT_RUNNING
                        }
                    })
                })
                .unwrap_or("not-loaded");
            (name.as_str(), state)
        })
        .collect()
}

fn validate_root(root: &Path) -> Result<()> {
    if !root.is_absolute() || !root.is_dir() {
        bail!("CI root is not mounted: {}", root.display());
    }
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        bail!("CI root must not be a symlink: {}", root.display());
    }
    Ok(())
}

fn older_than(metadata: &fs::Metadata, age: Duration) -> Result<bool> {
    let modified = metadata.modified().context("reading modification time")?;
    Ok(older_than_time(modified, age))
}

fn older_than_time(modified: SystemTime, age: Duration) -> bool {
    SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        > age
}

/// When the parts of a VM bundle were last written, newest first.
///
/// The bundle's own directory only changes when `tart` adds or removes a part,
/// which happens when it clones and never again, so its timestamp reads as old
/// throughout a run that writes gigabytes to the disk image inside it. The
/// newest part is what says when the guest last did anything. A directory with
/// no parts is not a bundle, and answers `None` rather than "infinitely old".
fn newest_part_mtime(bundle: &Path) -> Option<SystemTime> {
    fs::read_dir(bundle)
        .ok()?
        .filter_map(|entry| entry.ok()?.metadata().ok()?.modified().ok())
        .max()
}

fn writable_probe(directory: &Path) -> Result<()> {
    let path = directory.join(format!(".kithara-write-probe-{}", std::process::id()));
    let result = fs::write(&path, b"probe")
        .with_context(|| format!("CI directory is not writable: {}", directory.display()));
    let cleanup =
        fs::remove_file(&path).with_context(|| format!("removing write probe {}", path.display()));
    result.and(cleanup)
}

fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

/// `lsof -t` can report owners and still exit with a search error. Those PIDs
/// keep the directory live regardless of the exit status.
fn reports_open_files(output: &Output) -> bool {
    !output.stdout.is_empty()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsString, fs::FileTimes, time::SystemTime};

    use kithara_devtools::lock::FileLock;

    use super::*;
    #[cfg(unix)]
    use crate::testing::install_script;
    use crate::{
        ci::{config::fixture, previous_layout::previous_layout},
        testing::install_double,
    };

    /// Lane `lint`'s build, holding one artifact, in the root an executor
    /// names beside a checkout under `build_root`.
    #[cfg(unix)]
    fn a_build_beside_a_checkout(build_root: &Path) -> PathBuf {
        let root = build_root.join("workspaces/gitlab/runner/0/disrupt/kithara.target");
        let build = root.join("lint");
        fs::create_dir_all(build.join("debug")).unwrap();
        fs::write(build.join("debug/artifact.bin"), vec![0_u8; 200]).unwrap();
        std::os::unix::fs::symlink("lint", root.join(consts::BUILD_ALIAS)).unwrap();
        build
    }

    fn config(root: &Path) -> CiConfig {
        let mut config = fixture();
        config.host.host_root = root.to_path_buf();
        config.host.cache_root_macos = root.join("cache");
        config.host.cache_root_linux = root.join("cache");
        config.host.cache_root_windows = root.join("cache");
        config.host.quota_bytes = 300;
        config.host.reject_bytes = 285;
        config.host.aggressive_cleanup_bytes = 270;
        config.host.soft_cleanup_bytes = 240;
        config
    }

    #[test]
    fn reported_open_files_survive_lsof_search_errors() {
        let directory = tempfile::tempdir().unwrap();
        let lsof = install_double(directory.path(), "lsof");
        let process = Process::new(directory.path(), BTreeMap::new());
        for (stdout, active) in [("86343\n", true), ("", false)] {
            let output = process
                .command(&lsof)
                .env("KITHARA_TEST_RULES", "*:*:*=1")
                .env("KITHARA_TEST_STDOUT", stdout)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(reports_open_files(&output), active);
        }
    }

    #[test]
    fn pressure_thresholds_are_exact() {
        // 300 GB quota with 240/270/285 used-byte thresholds keeps 60/30/15
        // free.
        assert_eq!(pressure_for(61, 60, 30, 15), Pressure::Normal);
        assert_eq!(pressure_for(60, 60, 30, 15), Pressure::Soft);
        assert_eq!(pressure_for(30, 60, 30, 15), Pressure::Aggressive);
        assert_eq!(pressure_for(15, 60, 30, 15), Pressure::Reject);
    }

    #[test]
    fn incident_target_trims_crossed_from_aggressive_to_normal() {
        const BEFORE: u64 = 24_852_602_880;
        const FIRST_TARGET_TRIM: u64 = 21_944_082_432;
        const SECOND_TARGET_TRIM: u64 = 17_979_826_176;
        const AFTER_TARGETS: u64 = 64_776_511_488;
        const SOFT_FREE: u64 = 60_000_000_000;
        const AGGRESSIVE_FREE: u64 = 30_000_000_000;
        const REJECT_FREE: u64 = 15_000_000_000;

        assert_eq!(
            BEFORE + FIRST_TARGET_TRIM + SECOND_TARGET_TRIM,
            AFTER_TARGETS
        );
        assert_eq!(
            pressure_for(BEFORE, SOFT_FREE, AGGRESSIVE_FREE, REJECT_FREE),
            Pressure::Aggressive
        );
        assert_eq!(
            pressure_for(AFTER_TARGETS, SOFT_FREE, AGGRESSIVE_FREE, REJECT_FREE),
            Pressure::Normal
        );
    }

    #[test]
    fn a_volume_smaller_than_the_quota_still_reaches_reject() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();
        // The volume this replaces a proportional rule for: 279 GB total with
        // 170 used never came within a hundred of a 285 reject threshold, so
        // cleanup stayed `Normal`, while jobs were
        // already being refused for free space. Judged by what is left, a
        // volume half the quota's size reaches every step.
        let half = |available| Volume {
            path: PathBuf::from("/elsewhere"),
            total: 150,
            available,
        };

        assert_eq!(storage.pressure_of(&half(61)), Pressure::Normal);
        assert_eq!(storage.pressure_of(&half(60)), Pressure::Soft);
        assert_eq!(storage.pressure_of(&half(30)), Pressure::Aggressive);
        assert_eq!(storage.pressure_of(&half(15)), Pressure::Reject);
    }

    /// A tempdir has more than the sixty bytes this config calls a floor, so the
    /// run below is `Normal` — the pressure that used to skip the trim entirely.
    #[test]
    fn the_guest_is_trimmed_even_with_nothing_under_pressure() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        install_double(&cfg.host.brew_root.join("bin"), "colima");
        let process = Process::recording(directory.path(), Recording::default());
        {
            let storage = HostStorage::for_test(&cfg, &process).unwrap();
            assert_eq!(storage.worst_pressure().unwrap().0, Pressure::Normal);
            storage.cleanup().unwrap();
        }

        let recording = process.recorded().expect("a recording process records");
        let arguments = recording
            .steps()
            .iter()
            .find(|step| step.label == "return the Linux guest's freed blocks")
            .map(|step| step.args.join(" "))
            .expect("the guest was never asked for anything");
        assert!(
            arguments.contains("fstrim"),
            "cleanup asked the guest for {arguments} instead of a trim"
        );
    }

    /// Held to a size on every pass, and before the trim that hands what it
    /// frees back to the volume. An age keeps whatever one busy week wrote,
    /// however large, and was only asked for once the volume was already short.
    #[test]
    fn every_pass_holds_the_docker_build_cache_to_its_size_before_the_trim() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        install_double(&cfg.host.brew_root.join("bin"), "docker");
        install_double(&cfg.host.brew_root.join("bin"), "colima");
        let home = directory.path().join("home").join(&cfg.host.ci_user);
        let socket = docker_socket(&home, &cfg.host.colima_profile);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        fs::write(&socket, b"").unwrap();
        let process = Process::recording(directory.path(), Recording::default());
        {
            let storage = HostStorage::for_test(&cfg, &process).unwrap();
            assert_eq!(storage.worst_pressure().unwrap().0, Pressure::Normal);
            storage.cleanup().unwrap();
        }

        let recording = process.recorded().expect("a recording process records");
        let steps = recording.steps();
        let position = |label: &str| steps.iter().position(|step| step.label == label);
        let prune = position("Docker build cache cleanup")
            .expect("a pass with nothing under pressure left the Docker build cache unbounded");
        let budget = cfg
            .host
            .docker_build_cache_budget_bytes()
            .unwrap()
            .to_string();
        assert_eq!(
            steps[prune].args,
            ["builder", "prune", "--force", "--max-used-space", &budget]
        );
        let trim =
            position("return the Linux guest's freed blocks").expect("the guest was trimmed");
        assert!(prune < trim, "the trim ran before the prune it hands back");
    }

    #[cfg(unix)]
    #[test]
    fn reject_cleanup_preserves_shared_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let bin = cfg.host.brew_root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let runtime = directory.path().join("shared-runtime");
        fs::write(&runtime, b"cache service and CI images").unwrap();
        for tool in ["colima", "limactl"] {
            install_script(
                &bin.join(tool),
                "#!/bin/sh\ncase \" $* \" in *\" delete \"*) rm -- \"$RUNTIME_MARKER\";; esac\n",
            );
        }
        let process = Process::new(
            directory.path(),
            BTreeMap::from([(
                OsString::from("RUNTIME_MARKER"),
                runtime.as_os_str().to_owned(),
            )]),
        );
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([0; 8]);

        let result = storage.cleanup();

        assert_eq!(
            fs::read(&runtime).unwrap(),
            b"cache service and CI images",
            "disk pressure must not destroy the shared runtime"
        );
        assert!(result.is_err(), "unresolved disk pressure must be reported");
    }

    #[test]
    fn legacy_macos_runner_is_not_health_owned() {
        let host = fixture().host;

        assert!(
            !agent_states_from(consts::CRASH_LOOP_LISTING, &host.always_on_agents)
                .contains_key("macos-runner")
        );
    }

    #[test]
    fn the_agents_still_holding_a_process_read_as_running() {
        let host = fixture().host;

        let states = agent_states_from(consts::CRASH_LOOP_LISTING, &host.always_on_agents);

        assert_eq!(states.get("gitlab-runner"), Some(&"running"));
        assert_eq!(states.get("colima"), Some(&"running"));
    }

    /// An agent nobody ever loaded is as unable to take work as one that keeps
    /// dying, and reads differently so an operator knows which to fix.
    #[test]
    fn an_agent_missing_from_the_listing_reads_as_not_loaded() {
        let host = fixture().host;

        assert_eq!(
            agent_states_from(
                "82778\t0\tcom.zvuk.kithara-ci.gitlab-runner\n",
                &host.always_on_agents
            )
            .get("colima"),
            Some(&"not-loaded")
        );
    }

    #[test]
    fn a_full_volume_is_not_hidden_by_a_roomy_one() {
        assert!(Pressure::Reject > Pressure::Normal);
        assert!(Pressure::Aggressive > Pressure::Soft);
    }

    #[test]
    fn a_distinct_checkout_volume_is_monitored() {
        let directory = tempfile::tempdir().unwrap();
        let host_root = directory.path().join("host");
        let build_root = directory.path().join("builds");
        fs::create_dir_all(&host_root).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        let mut cfg = config(&host_root);
        cfg.host.build_root = Some(build_root.clone());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        let volumes = storage.volumes().unwrap();

        assert!(volumes.iter().any(|volume| volume.path == host_root));
        assert!(
            volumes
                .iter()
                .any(|volume| volume.path == build_root.canonicalize().unwrap())
        );
    }

    #[test]
    fn a_namespace_nothing_writes_to_is_pruned_and_the_live_ones_are_not() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        for name in [
            "trusted",
            "review",
            "gitlab-runner",
            "reapi",
            "reapi-sccache",
        ] {
            fs::create_dir_all(cache.join(name)).unwrap();
        }
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_retired_caches(Duration::ZERO).unwrap();

        for name in ["trusted", "review", "gitlab-runner"] {
            assert!(cache.join(name).is_dir(), "{name} is still written to");
        }
        for name in ["reapi", "reapi-sccache"] {
            assert!(!cache.join(name).exists(), "{name} is retired");
        }
    }

    /// A host deployed from a branch serves `production/main` until the branch
    /// merges, and main keeps its builds and its compiler-cache slot locks in
    /// namespaces of its own. Taken whole as retired, they send every main job
    /// back to a cold build.
    #[test]
    fn a_namespace_the_previous_layout_keeps_survives_whatever_the_profile_lists() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        let namespaces = previous_layout().mac.cache_namespaces;
        for name in &namespaces {
            fs::create_dir_all(cache.join(name)).unwrap();
        }
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_retired_caches(Duration::ZERO).unwrap();

        for name in &namespaces {
            assert!(cache.join(name).is_dir(), "main still writes to {name}");
        }
    }

    /// The container's Cargo home sits on the cache share. Taking it whole as
    /// a retired namespace sends every Linux job back to the registry.
    #[test]
    fn zero_age_prune_preserves_the_containers_cargo_home() {
        let directory = tempfile::tempdir().unwrap();
        let registry = directory
            .path()
            .join("cache")
            .join(consts::CARGO_HOME_DIR)
            .join("registry");
        fs::create_dir_all(&registry).unwrap();
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_retired_caches(Duration::ZERO).unwrap();

        assert!(registry.is_dir());
    }

    #[test]
    fn a_retired_namespace_survives_until_it_has_gone_quiet() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        fs::create_dir_all(cache.join("reapi")).unwrap();
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_retired_caches(HostStorage::DAY).unwrap();

        assert!(cache.join("reapi").is_dir());
    }

    #[test]
    fn cleanup_never_leaves_the_ci_root() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = config(directory.path());
        for name in &cfg.host.removable_roots {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();
        let safe = directory.path().join("workspaces/tmp/old");
        fs::create_dir_all(&safe).unwrap();
        storage.remove_path(&safe).unwrap();
        assert!(!safe.exists());

        let outside = directory.path().parent().unwrap().join("outside");
        assert!(storage.remove_path(&outside).is_err());
    }

    #[test]
    fn cleanup_allows_only_workspace_descendants_on_the_checkout_root() {
        let directory = tempfile::tempdir().unwrap();
        let host_root = directory.path().join("host");
        let build_root = directory.path().join("builds");
        fs::create_dir_all(&host_root).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        let mut cfg = config(&host_root);
        cfg.host.build_root = Some(build_root.clone());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        let workspace = build_root.join("workspaces/gitlab/old");
        fs::create_dir_all(&workspace).unwrap();
        storage.remove_path(&workspace).unwrap();
        assert!(!workspace.exists());

        assert!(storage.remove_path(&build_root).is_err());
        let cache = build_root.join("cache/old");
        fs::create_dir_all(&cache).unwrap();
        assert!(storage.remove_path(&cache).is_err());
    }

    /// One entry in a scratch namespace, aged as a job that died before its
    /// temporaries dropped leaves it.
    fn scratch_entry(root: &Path, namespace: &str, age: Duration) -> PathBuf {
        let entry = root.join("scratch").join(namespace).join(".tmpJ8kQ2v");
        fs::create_dir_all(entry.join("fixture")).unwrap();
        fs::File::open(&entry)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - age))
            .unwrap();
        entry
    }

    /// What the CI host held on 2026-08-22: sixty-eight thousand directories
    /// and 3.5 gibibytes under the lanes' `TMPDIR`, the oldest four days old,
    /// on the same container every pressure threshold is read from. Every
    /// namespace is checked, because a lane runs under whichever one it is
    /// given and a namespace left out of the sweep leaks exactly as this one
    /// did. Driven through `cleanup`, because a step nothing calls frees
    /// nothing.
    #[test]
    fn scratch_no_job_is_using_is_pruned() {
        let directory = tempfile::tempdir().unwrap();
        let stale = CacheTrust::ALL
            .map(|trust| scratch_entry(directory.path(), trust.as_str(), 4 * HostStorage::DAY));
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        for entry in &stale {
            assert!(!entry.exists(), "{} outlived the sweep", entry.display());
        }
    }

    /// A running job owns its scratch and is writing into it, so the sweep must
    /// not reach anything a lane could still be using. The day it waits is more
    /// than ten times the longest lane.
    #[test]
    fn scratch_a_running_job_owns_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let live = scratch_entry(directory.path(), "trusted", Duration::from_secs(60));
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(live.is_dir());
    }

    /// The namespace directories are what `CiEnvironment::prepare` hands out as
    /// `TMPDIR`, and the root above them is shared by every job on the machine.
    /// Only their contents are ever a candidate.
    #[test]
    fn a_scratch_namespace_is_never_removable() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();
        let root = directory.path().join("scratch");
        fs::create_dir_all(root.join("trusted")).unwrap();

        assert!(storage.remove_path(&root.join("trusted")).is_err());
        assert!(storage.remove_path(&root).is_err());
    }

    #[test]
    fn cleanup_leaves_gitlab_workspaces_to_the_runner() {
        let directory = tempfile::tempdir().unwrap();
        let host_root = directory.path().join("host");
        let build_root = directory.path().join("builds");
        fs::create_dir_all(&host_root).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        let mut cfg = config(&host_root);
        cfg.host.build_root = Some(build_root.clone());
        let runner = build_root.join("workspaces/gitlab/runner");
        let checkout = runner.join("0/disrupt/kithara");
        fs::create_dir_all(checkout.join("xtask")).unwrap();
        fs::write(checkout.join("justfile"), "").unwrap();
        fs::write(checkout.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            checkout.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        fs::File::open(&runner)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - 2 * HostStorage::DAY))
            .unwrap();
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(checkout.join("justfile").is_file());
        assert!(checkout.join("xtask/Cargo.toml").is_file());
    }

    #[test]
    fn active_marker_pins_a_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = config(directory.path());
        for name in &cfg.host.removable_roots {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();
        let workspace = directory.path().join("workspaces/tmp/current");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(".kithara-ci-active"), b"").unwrap();
        assert!(storage.active(&workspace));
    }

    #[test]
    fn aggressive_cleanup_keeps_any_fresh_job_lease_and_removes_stale_peers() {
        let directory = tempfile::tempdir().unwrap();
        let namespace = directory.path().join("cache/review/macos-aarch64");
        let leases = namespace.join(".kithara-ci-leases");
        fs::create_dir_all(&leases).unwrap();
        let fresh = leases.join("job-29");
        let stale = leases.join("job-28");
        fs::write(&fresh, b"").unwrap();
        fs::write(&stale, b"").unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let lease = Duration::from_secs(cfg.host.active_lease_hours * 60 * 60);
        let stale_time = SystemTime::now() - lease - Duration::from_secs(1);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(FileTimes::new().set_modified(stale_time))
            .unwrap();
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(namespace.is_dir());
        assert!(fresh.is_file());
        assert!(!stale.exists());
    }

    #[test]
    fn target_budget_relieving_pressure_preserves_the_review_compiler_cache() {
        let directory = tempfile::tempdir().unwrap();
        let review = directory.path().join("cache/review/macos-aarch64");
        fs::create_dir_all(review.join("sccache")).unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(review.is_dir());
    }

    /// The hourly pass this host ran for a day: two checkouts held 88 and 95 GB
    /// under a 100 GB ceiling, so enforcing the budget freed nothing on every
    /// pass while the volume they share sat below the floor and jobs were
    /// already being refused for space.
    ///
    /// A ceiling answers "is any one cache too big"; the volume asks "is there
    /// room", and only the second question has a job waiting on it.
    #[cfg(unix)]
    #[test]
    fn a_volume_under_the_floor_reclaims_past_the_build_cache_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        cfg.host.build_root = Some(directory.path().join("builds"));
        let build = a_build_beside_a_checkout(cfg.host.build_root());
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(
            !build.join("debug").exists(),
            "the pass stopped at the build cache ceiling the caches were already under"
        );
    }

    /// A host deployed from a branch serves `production/main` until the branch
    /// merges, and main builds in lane slots and xtask bootstraps under the
    /// cache root. Out of the budget's sight they grow without bound; weighed,
    /// an idle one is reclaimed and a slot main has taken, by locking the file
    /// beside it, is not.
    #[test]
    fn a_volume_under_the_floor_reclaims_the_builds_the_previous_layout_keeps() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        // A floor past both idle builds, so the pass has to reach each of them.
        cfg.host.quota_bytes = 1 << 30;
        cfg.host.reject_bytes = cfg.host.quota_bytes - 15;
        cfg.host.aggressive_cleanup_bytes = cfg.host.quota_bytes - 30;
        cfg.host.soft_cleanup_bytes = 0;
        let cache = directory.path().join("cache");
        let slots = cache.join(consts::PREVIOUS_TARGET_SLOTS);
        let idle = slots.join("macos-aarch64-lane-lint-0");
        let taken = slots.join("macos-aarch64-lane-lint-1");
        let bootstrap = cache
            .join(consts::PREVIOUS_BOOTSTRAP)
            .join("review/target-macos-aarch64-0");
        for build in [idle.join("cargo"), taken.join("cargo"), bootstrap.clone()] {
            fs::create_dir_all(build.join("debug")).unwrap();
            fs::write(build.join("debug/artifact.bin"), vec![0_u8; 200]).unwrap();
        }
        let lock = fs::File::create(slots.join("macos-aarch64-lane-lint-1.lock")).unwrap();
        let _claim = FileLock::try_exclusive(lock).unwrap();
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(!idle.join("cargo/debug").exists(), "an idle slot is kept");
        assert!(
            !bootstrap.join("debug").exists(),
            "an idle bootstrap is kept"
        );
        assert!(
            taken.join("cargo/debug/artifact.bin").is_file(),
            "the pass evicted a slot main has taken"
        );
    }

    /// Warm builds are the last rung: a pass whose cheaper steps lift the volume
    /// out of `Aggressive` evicts none of them.
    ///
    /// Spent first, they went on every `Aggressive` pass, and every job after it
    /// built the workspace cold, while the stale trees, the Docker cache and the
    /// guest's unreturned blocks reached for afterwards would have covered the
    /// shortfall on their own.
    #[cfg(unix)]
    #[test]
    fn an_aggressive_pass_the_cheaper_steps_relieve_keeps_every_build() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let build = a_build_beside_a_checkout(cfg.host.build_root());
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(
            build.join("debug/artifact.bin").is_file(),
            "the pass evicted a warm build before the steps that relieved the volume"
        );
    }

    /// The hourly pass this host actually ran: in at `Aggressive`, every step
    /// executed, out at `Aggressive`, logged as completed. Reported as success
    /// it hid a machine that was already refusing jobs.
    #[test]
    fn a_pass_that_leaves_the_pressure_where_it_found_it_is_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
        ]);

        assert!(storage.cleanup().is_err());
    }

    /// A pass that cannot reach the threshold names the tree holding the space.
    ///
    /// Reporting only that every owned step reached its floor is what let this
    /// recur: it rules out the caches and names nothing, so the trees no step
    /// owns are never the thing anyone looks at next.
    #[cfg(unix)]
    #[test]
    fn a_pass_that_cannot_reach_the_threshold_names_what_holds_the_space() {
        let directory = tempfile::tempdir().unwrap();
        let heavy = directory.path().join("vm/tart/vms");
        fs::create_dir_all(&heavy).unwrap();
        fs::write(heavy.join("disk.img"), vec![0_u8; 400_000]).unwrap();
        let light = directory.path().join("logs/bridge");
        fs::create_dir_all(&light).unwrap();
        fs::write(light.join("bridge.log"), b"quiet").unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
        ]);

        let error = storage.cleanup().unwrap_err().to_string();

        let heavy = error
            .find("vm/tart=")
            .unwrap_or_else(|| panic!("the failure must name the tree holding the space: {error}"));
        assert!(
            error.find("logs/bridge=").is_none_or(|light| heavy < light),
            "the tree holding the space must come first: {error}"
        );
        assert!(
            !error.contains("not build caches"),
            "the old message ruled the caches out and named nothing: {error}"
        );
    }

    /// The holders are one `du` over the CI roots, in the allocated bytes the
    /// volume pays. A first-level tree stands for itself only with nothing
    /// measured below it, and a walk that hit a vanished file still answers.
    #[cfg(unix)]
    #[test]
    fn the_holders_come_from_one_du_over_the_ci_roots() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let asked = root.join("du-asked");
        let listing = format!(
            "1\t{r}/logs/bridge\n4\t{r}/logs\n500\t{r}/home/kithara-ci\n502\t{r}/home\n7\t{r}/vm\n\
             513\t{r}\n",
            r = root.display()
        );
        install_script(
            &bin.join("du"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{asked}'\nprintf '%s' '{listing}'\n\
                 echo 'du: gone: No such file or directory' >&2\nexit 1\n",
                asked = asked.display()
            ),
        );
        let mut cfg = config(root);
        cfg.host.brew_root = root.join("brew");
        let mut search = OsString::from(&bin);
        search.push(":");
        search.push(std::env::var_os("PATH").unwrap_or_default());
        let process = Process::new(root, BTreeMap::from([(OsString::from("PATH"), search)]));
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([consts::FREE_AGGRESSIVE; 4]);

        let error = storage.cleanup().unwrap_err().to_string();

        let named = |tree: &str, kib: u64| format!("{}={}", root.join(tree).display(), kib * 1024);
        let holders = [
            named("home/kithara-ci", 500),
            named("vm", 7),
            named("logs/bridge", 1),
        ];
        let at: Vec<usize> = holders
            .iter()
            .map(|holder| {
                error
                    .find(holder.as_str())
                    .unwrap_or_else(|| panic!("{holder} is missing from: {error}"))
            })
            .collect();
        assert!(at.is_sorted(), "holders must come largest first: {error}");
        for summed in [named("home", 502), named("logs", 4)] {
            assert!(
                !error.contains(&summed),
                "{summed} sums trees already named: {error}"
            );
        }
        assert_eq!(
            fs::read_to_string(&asked).unwrap(),
            format!("-k\n-d\n2\n{}\n", root.display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_blocked_holder_diagnostic_is_reaped_and_allows_the_next_cleanup() {
        use nix::{
            errno::Errno,
            sys::wait::{WaitPidFlag, waitpid},
            unistd::Pid,
        };

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let pid_file = root.join("du.pid");
        install_script(
            &bin.join("du"),
            &format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        );
        let mut cfg = config(root);
        cfg.host.brew_root = root.join("brew");
        let mut search = OsString::from(&bin);
        search.push(":");
        search.push(std::env::var_os("PATH").unwrap_or_default());
        let process = Process::new(root, BTreeMap::from([(OsString::from("PATH"), search)]));
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([consts::FREE_AGGRESSIVE; 4]);

        let started = Instant::now();
        let error = storage.cleanup().unwrap_err().to_string();
        assert!(started.elapsed() < consts::CONTROL_TIMEOUT + Duration::from_secs(5));
        assert!(
            error.contains("trees this pass could not measure"),
            "{error}"
        );
        let pid = fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );

        install_script(
            &bin.join("du"),
            &format!(
                "#!/bin/sh\nprintf '1\\t{}/logs/bridge\\n'\n",
                root.display()
            ),
        );
        storage.set_available_sequence([consts::FREE_AGGRESSIVE; 4]);
        let error = storage.cleanup().unwrap_err().to_string();
        assert!(
            error.contains(&format!("{}=1024", root.join("logs/bridge").display())),
            "{error}"
        );
    }

    #[test]
    fn review_compiler_cache_is_pruned_when_pressure_stays_aggressive() {
        let directory = tempfile::tempdir().unwrap();
        let review = directory.path().join("cache/review/macos-aarch64");
        fs::create_dir_all(review.join("sccache")).unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_AGGRESSIVE,
            consts::FREE_AGGRESSIVE,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert!(!review.exists());
    }

    /// A `tart` that records what it was asked for.
    fn fake_tart(cfg: &mut CiConfig, root: &Path) -> PathBuf {
        cfg.host.brew_root = root.join("brew");
        install_double(&cfg.host.brew_root.join("bin"), "tart");
        root.join("asked")
    }

    /// The environment a `tart` double answers a listing from.
    fn tart_vars(asked: &Path, running: bool) -> BTreeMap<OsString, OsString> {
        BTreeMap::from([
            (
                OsString::from("KITHARA_TEST_TRACE"),
                asked.to_path_buf().into_os_string(),
            ),
            (
                OsString::from("KITHARA_TEST_STDOUT"),
                OsString::from(format!(
                    "[{{\"Name\":\"kithara-ci-job\",\"Running\":{running},\"State\":\"stopped\"}},\
                     {{\"Name\":\"kithara-macos-base\",\"Running\":false,\"State\":\"stopped\"}}]"
                )),
            ),
        ])
    }

    /// What `tart` was asked for other than the listing every run begins with.
    ///
    /// The listing is how the code under test decides, so its presence in the
    /// trace carries no verdict; what follows it does.
    fn asked_beyond_listing(asked: &Path) -> Vec<String> {
        fs::read_to_string(asked)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with("list"))
            .map(str::to_owned)
            .collect()
    }

    /// Lay out `<TART_HOME>/vms` with a base bundle and, optionally, a clone
    /// whose parts were last written `since` ago.
    fn tart_vms(cfg: &mut CiConfig, root: &Path, clone_age: Option<Duration>) {
        let vms = root.join("vm/tart/vms");
        cfg.host.macos_vm_bundle = vms.join("kithara-macos-base");
        fs::create_dir_all(&cfg.host.macos_vm_bundle).unwrap();
        fs::write(cfg.host.macos_vm_bundle.join("disk.img"), b"base").unwrap();
        let Some(age) = clone_age else {
            return;
        };
        let clone = vms.join("kithara-ci-job");
        fs::create_dir_all(&clone).unwrap();
        let part = clone.join("disk.img");
        fs::write(&part, b"clone").unwrap();
        fs::File::options()
            .write(true)
            .open(&part)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - age))
            .unwrap();
    }

    /// The clone the CI host held on 2026-08-22: left behind on 12 August by a
    /// runner that has not been loaded since, on a volume reporting `Normal`
    /// and freeing nothing every five minutes. What it returns is the
    /// divergence the dead runner wrote, not the size a directory walk reads
    /// off a copy-on-write clone. Driven through `cleanup` rather than the
    /// step alone, because a step nothing calls frees nothing.
    #[test]
    fn an_abandoned_job_vm_is_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path());
        tart_vms(&mut cfg, directory.path(), Some(10 * HostStorage::DAY));
        let process = Process::new(directory.path(), tart_vars(&asked, false));
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_available_sequence([
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
            consts::FREE_NORMAL,
        ]);

        storage.cleanup().unwrap();

        assert_eq!(
            asked_beyond_listing(&asked),
            ["delete kithara-ci-job"],
            "an abandoned clone must be handed back to tart"
        );
    }

    /// The base bundle is the one thing here that cannot be remade without a
    /// person and an IPSW, so it is named out of the sweep rather than aged out
    /// of it: with no clone beside it, cleanup must ask tart for nothing.
    #[test]
    fn the_base_bundle_is_never_a_delete_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path());
        tart_vms(&mut cfg, directory.path(), None);
        let process = Process::new(directory.path(), tart_vars(&asked, false));
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_abandoned_job_vms(HostStorage::DAY);

        assert!(
            !asked.exists(),
            "the base bundle must never be offered for deletion"
        );
        assert!(cfg.host.macos_vm_bundle.is_dir());
    }

    /// A guest serves every job it is offered and writes nothing to its bundle
    /// while it waits, so an idle runner looks exactly as stale as a dead one.
    /// Liveness is what separates them.
    #[test]
    fn a_job_vm_a_runner_serves_from_survives() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path());
        tart_vms(&mut cfg, directory.path(), Some(10 * HostStorage::DAY));
        let process = Process::new(directory.path(), tart_vars(&asked, true));
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_abandoned_job_vms(HostStorage::DAY);

        assert!(
            asked_beyond_listing(&asked).is_empty(),
            "a running guest must not be taken from the runner serving jobs on it"
        );
    }

    /// The runner loop leaves the clone stopped from `tart clone` until the
    /// guest answers, which is up to two hundred seconds — longer than the five
    /// minutes this runs on. Deleting it there fails the boot and takes the
    /// runner down with it.
    #[test]
    fn a_freshly_cloned_job_vm_survives() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path());
        tart_vms(&mut cfg, directory.path(), Some(Duration::ZERO));
        let process = Process::new(directory.path(), tart_vars(&asked, false));
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_abandoned_job_vms(HostStorage::DAY);

        assert!(
            !asked.exists(),
            "a clone still booting must survive the interval it boots across"
        );
    }
}
