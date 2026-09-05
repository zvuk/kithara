#[cfg(test)]
use std::{cell::RefCell, collections::VecDeque};
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::runner_images::JobVm;
use crate::ci::{
    SCCACHE_SLOT_CONTROL_NAMESPACE, build_cache,
    config::CiConfig,
    environment::{CacheTrust, scratch_root},
    process::Process,
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
    #[cfg(test)]
    pressure_sequence: RefCell<VecDeque<Pressure>>,
}

struct Agents;

impl Agents {
    const LAUNCHCTL: &'static str = "/bin/launchctl";
    const RUNNING: &'static str = "running";
    /// A host with no launchd has no agents to be wrong about, and the Linux
    /// executor runs this same command.
    const ABSENT: &'static str = "not-applicable";
    /// The agents that must hold a process for work to reach this host.
    ///
    /// `cleanup` and `health` are periodic and spend nearly all their life
    /// loaded with nothing running, so a missing process says nothing about
    /// them. These two are `KeepAlive`, and a missing process means work has
    /// stopped.
    const ALWAYS_ON: &'static [&'static str] = &["colima", "gitlab-runner"];
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
    const ACTIVE_LEASE: Duration = Duration::from_secs(12 * 60 * 60);
    /// The profile `install-services` starts the Linux guest under.
    const COLIMA_PROFILE: &'static str = "kithara";
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const LOG_LIMIT_BYTES: u64 = 20_000_000;
    const REMOVABLE_ROOTS: &'static [&'static str] = &["cache", "logs", "vm", "workspaces"];
    /// The cache namespaces this repository still writes to.
    ///
    /// The steps below prune by name, so a namespace that stops being written
    /// to becomes invisible rather than stale, and nothing ever comes back for
    /// it. Six gigabytes of `cargo-reapi` stores sat here after that tool came
    /// off the CI path. Anything not named here is pruned on its own age.
    const CACHE_NAMESPACES: &'static [&'static str] = &[
        SCCACHE_SLOT_CONTROL_NAMESPACE,
        "bootstrap",
        "gitlab-runner",
        "quarantine",
        "review",
        "trusted",
    ];

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
            pressure_sequence: RefCell::new(VecDeque::new()),
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
            pressure_sequence: RefCell::new(VecDeque::new()),
        })
    }

    #[cfg(test)]
    fn set_pressure_sequence(&mut self, sequence: impl IntoIterator<Item = Pressure>) {
        self.pressure_sequence = RefCell::new(sequence.into_iter().collect());
    }

    pub(super) fn preflight(&self) -> Result<()> {
        let free = self.free_bytes()?;
        let (pressure, volume) = self.worst_pressure()?;
        match pressure {
            Pressure::Reject => bail!("{} is full; new jobs stop here", volume.display()),
            Pressure::Soft | Pressure::Aggressive => {
                warn!(volume = %volume.display(), ?pressure, "CI volume is under pressure");
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
        self.process.require_tools(&["git", "sccache"])?;
        info!(free_bytes = free, ?pressure, "host preflight passed");
        Ok(())
    }

    pub(super) fn cleanup(&self) -> Result<()> {
        let initial = self.free_bytes()?;
        // The worst single volume, not the sum: adding a second volume's bytes
        // to the first and comparing that against thresholds calibrated for the
        // first reads every machine with a guest volume as full, and the branch
        // it reaches for throws away the compiler caches that were never the
        // problem. `preflight` already decides this way.
        let (pressure, volume) = self.worst_pressure()?;
        info!(free_bytes = initial, ?pressure, volume = %volume.display(), "cleanup started");

        self.prune_host_trees("workspaces/tmp", Self::DAY)?;
        self.prune_scratch_trees(Self::DAY)?;
        self.prune_host_trees("workspaces/builds", Self::DAY)?;
        self.prune_host_trees("vm/overlays", Self::DAY)?;
        self.prune_host_trees("vm/android/avd", Self::DAY)?;
        self.prune_abandoned_job_vms(Self::DAY);
        self.prune_host_files("logs", 14 * Self::DAY)?;
        self.rotate_logs()?;
        self.prune_retired_caches(7 * Self::DAY)?;

        let mut target_dirs =
            build_cache::persistent_target_dirs(&self.build_root.join("workspaces/gitlab"))?;
        target_dirs.extend(build_cache::cached_target_dirs(
            &self.host_root.join("cache"),
        )?);
        build_cache::enforce_budget(&target_dirs, self.config.host.build_cache_budget_bytes()?)?;

        // Cargo targets are the largest reproducible caches and already have a
        // bounded owner. Re-read pressure after enforcing that budget so a
        // successful trim does not throw away review compiler artifacts too.
        let cache_pressure = self.worst_pressure()?.0;
        match cache_pressure {
            Pressure::Soft => {
                self.prune_host_trees("cache/quarantine", 7 * Self::DAY)?;
                self.prune_host_trees("cache/review", 30 * Self::DAY)?;
                self.prune_host_trees("cache/bootstrap/quarantine", 7 * Self::DAY)?;
                self.prune_host_trees("cache/bootstrap/review", 30 * Self::DAY)?;
                self.prune_docker_cache("720h");
            }
            Pressure::Aggressive | Pressure::Reject => {
                self.prune_host_trees("cache/quarantine", Duration::ZERO)?;
                self.prune_host_trees("cache/review", Duration::ZERO)?;
                self.prune_host_trees("cache/bootstrap/quarantine", Duration::ZERO)?;
                self.prune_host_trees("cache/bootstrap/review", Duration::ZERO)?;
                self.prune_host_trees("cache/trusted", 7 * Self::DAY)?;
                self.prune_host_trees("cache/bootstrap/trusted", 7 * Self::DAY)?;
                self.prune_host_trees("vm/tart/cache", 7 * Self::DAY)?;
                self.prune_docker_cache("168h");
            }
            Pressure::Normal => {}
        }

        // Unconditional, and after the pruning above, because the guest frees
        // blocks on its own schedule and holds them until asked. Its root
        // filesystem is mounted `discard` and stays at a gigabyte, but the data
        // disk carrying `/var/lib/docker` is not, so every layer Docker deletes
        // stays allocated in a file this volume pays for. One trim on a machine
        // that had drifted to 44 GB free returned 63 of them in seconds — too
        // cheap to hold back for a threshold, and holding it back is what let
        // the drift reach refusal five times.
        self.trim_linux_guest();

        let (mut final_pressure, _) = self.worst_pressure()?;
        if final_pressure == Pressure::Reject {
            // The Linux guest has already been trimmed, so what remains is
            // Docker state younger than the prune window. Recycling reaches
            // that state at the cost of a cold image build, which is reserved
            // for the point where new jobs are already being refused.
            self.recycle_linux_guest();
            final_pressure = self.worst_pressure()?.0;
        }
        if final_pressure == Pressure::Reject {
            self.prune_host_trees("cache/trusted", Duration::ZERO)?;
            self.prune_host_trees("cache/bootstrap/trusted", Duration::ZERO)?;
            self.prune_retired_caches(Duration::ZERO)?;
            final_pressure = self.worst_pressure()?.0;
        }
        let free = self.free_bytes()?;
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
                volume.display(),
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
            .filter(|(_, state)| ![Agents::RUNNING, Agents::ABSENT].contains(*state))
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
            bail!("{} is above the new-job threshold", worst.display());
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
    fn agent_states(&self) -> BTreeMap<&'static str, &'static str> {
        if !Path::new(Agents::LAUNCHCTL).is_file() {
            return Agents::ALWAYS_ON
                .iter()
                .map(|name| (*name, Agents::ABSENT))
                .collect();
        }
        let listing = self
            .process
            .capture(Agents::LAUNCHCTL, &["list"], "launchd agent listing")
            .unwrap_or_default();
        agent_states_from(&listing)
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
    fn worst_pressure(&self) -> Result<(Pressure, PathBuf)> {
        #[cfg(test)]
        if let Some(pressure) = self.pressure_sequence.borrow_mut().pop_front() {
            return Ok((pressure, self.host_root.clone()));
        }
        let volumes = self.volumes()?;
        let mut worst = (Pressure::Normal, self.host_root.clone());
        for volume in volumes {
            let pressure = self.pressure_of(&volume);
            if pressure > worst.0 {
                worst = (pressure, volume.path);
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
            if Self::CACHE_NAMESPACES.contains(&name.as_ref()) {
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
                || fs::symlink_metadata(&path)?.len() <= Self::LOG_LIMIT_BYTES
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
        let lease_active = Self::lease_directory_active(&path.join(".kithara-ci-leases"));
        let legacy_active = Self::legacy_marker_active(&path.join(".kithara-ci-active"));
        if lease_active || legacy_active {
            return true;
        }
        self.process
            .command("/usr/sbin/lsof")
            .arg("+D")
            .arg(path)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn lease_directory_active(leases: &Path) -> bool {
        match fs::symlink_metadata(leases) {
            Ok(metadata) if metadata.file_type().is_dir() => Self::owner_lease_active(leases),
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

    fn legacy_marker_active(legacy: &Path) -> bool {
        match fs::symlink_metadata(legacy) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Self::lease_is_fresh(legacy, &metadata)
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

    fn owner_lease_active(directory: &Path) -> bool {
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
            active |= Self::lease_is_fresh(&path, &metadata);
        }
        active
    }

    fn lease_is_fresh(marker: &Path, metadata: &fs::Metadata) -> bool {
        match older_than(metadata, Self::ACTIVE_LEASE) {
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
    /// The macOS lane clones [`JobVm::NAME`] from the base bundle and destroys
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
        let bundle = home.join("vms").join(JobVm::NAME);
        if !tart.is_file() || !bundle.is_dir() {
            return;
        }
        let Some(touched) = newest_part_mtime(&bundle) else {
            return;
        };
        if !older_than_time(touched, age) || self.job_vm_is_running(&tart) {
            return;
        }
        info!(vm = JobVm::NAME, bundle = %bundle.display(), "removing abandoned macOS job VM");
        let mut command = self.process.command(&tart);
        command.args(["delete", JobVm::NAME]);
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
        listed.iter().any(|vm| vm.name == JobVm::NAME && vm.running)
    }

    fn prune_docker_cache(&self, age: &str) {
        let home = self.host_root.join("home").join(&self.config.host.ci_user);
        let socket = home.join(".colima/kithara/docker.sock");
        let docker = self.config.host.brew_tool("docker");
        if !socket.exists() || !docker.is_file() {
            return;
        }
        let mut command = self.process.command(docker);
        command
            .env("DOCKER_HOST", format!("unix://{}", socket.display()))
            .args(["builder", "prune", "--force", "--filter"])
            .arg(format!("until={age}"));
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
    /// released. A discard is what tells the host they are free, and it is the
    /// step between pruning, which frees nothing here, and recycling, which
    /// costs a cold image build.
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
            Self::COLIMA_PROFILE,
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

    /// Delete the Linux guest and the disk it kept, so the space is allocated
    /// again from nothing.
    ///
    /// The agent runs `colima start` in the foreground, so the process ends
    /// with the guest and launchd starts a fresh one. The images inside are
    /// rebuilt, which is the cost.
    ///
    /// Deleting the instance alone reclaims almost nothing: lima keeps the data
    /// disk as a named volume so that it survives a recreated guest, and the
    /// disk is where the space goes. It is sparse, so it grows with every write
    /// and never shrinks when the guest deletes. On 2026-08-07 the instance held
    /// 2 GB and the disk held 95 GB.
    ///
    /// Both are attempted even if the first fails: with the instance already
    /// gone, deleting the disk is exactly what still has to happen.
    fn recycle_linux_guest(&self) {
        let colima = self.config.host.brew_tool("colima");
        if !colima.is_file() {
            return;
        }
        info!("recycling the Linux guest to reclaim volume space");
        if let Err(error) = self.process.run(
            &colima.display().to_string(),
            &["delete", "--force", "--profile", Self::COLIMA_PROFILE],
            "recycle the Linux guest",
        ) {
            warn!(%error, "could not recycle the Linux guest");
        }

        let limactl = self.config.host.brew_tool("limactl");
        if !limactl.is_file() {
            warn!("limactl is absent, so the guest's data disk stays allocated");
            return;
        }
        let home = self.host_root.join("home").join(&self.config.host.ci_user);
        let mut command = self.process.command(limactl);
        command.env("LIMA_HOME", home.join(".colima/_lima")).args([
            "disk",
            "delete",
            &Self::linux_guest_disk(),
        ]);
        if let Err(error) = self
            .process
            .run_command(&mut command, "delete the Linux guest's data disk")
        {
            warn!(%error, "could not delete the Linux guest's data disk");
        }
    }

    /// colima names the disk after the profile it belongs to.
    fn linux_guest_disk() -> String {
        format!("colima-{}", Self::COLIMA_PROFILE)
    }

    fn is_removable(&self, target: &Path) -> bool {
        is_removable_under(&self.host_root, target, Self::REMOVABLE_ROOTS)
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
    /// already known to be on. A first-level directory with no subdirectories
    /// of its own stands for itself.
    ///
    /// Sizes are apparent, not allocated, so a sparse disk image reads larger
    /// than it costs rather than smaller. This picks which tree to look at; it
    /// is not an accounting of the volume.
    fn largest_trees(&self, count: usize) -> Vec<(PathBuf, u64)> {
        let mut roots = vec![self.host_root.clone()];
        if !self.build_root.starts_with(&self.host_root) {
            roots.push(self.build_root.clone());
        }
        let mut sizes = Vec::new();
        for root in &roots {
            for first in child_dirs(root) {
                let children = child_dirs(&first);
                if children.is_empty() {
                    let bytes = tree_bytes(&first);
                    sizes.push((first, bytes));
                    continue;
                }
                for child in children {
                    let bytes = tree_bytes(&child);
                    sizes.push((child, bytes));
                }
            }
        }
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

/// Immediate subdirectories of `directory`, following no symlink, empty when it
/// cannot be read.
fn child_dirs(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// Apparent bytes of the files under `path`, following no symlink.
///
/// What it cannot read it skips: this runs to name a tree while jobs are being
/// refused, and one unreadable directory is not a reason to answer nothing.
fn tree_bytes(path: &Path) -> u64 {
    let mut pending = vec![path.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total =
                    total.saturating_add(entry.metadata().map(|it| it.len()).unwrap_or_default());
            }
        }
    }
    total
}

fn is_removable_under(root: &Path, target: &Path, removable_roots: &[&str]) -> bool {
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
        .any(|allowed| first == std::ffi::OsStr::new(allowed))
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
fn agent_states_from(listing: &str) -> BTreeMap<&'static str, &'static str> {
    Agents::ALWAYS_ON
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
                            Agents::RUNNING
                        }
                    })
                })
                .unwrap_or("not-loaded");
            (*name, state)
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs::FileTimes, time::SystemTime};

    use super::*;
    use crate::ci::config::fixture;

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
        // cleanup stayed `Normal` and never recycled the guest, while jobs were
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
    #[cfg(unix)]
    #[test]
    fn the_guest_is_trimmed_even_with_nothing_under_pressure() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let bin = cfg.host.brew_root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let asked = directory.path().join("asked");
        fs::write(
            bin.join("colima"),
            format!("#!/bin/sh\necho \"$@\" > {}\n", asked.display()),
        )
        .unwrap();
        fs::set_permissions(bin.join("colima"), fs::Permissions::from_mode(0o755)).unwrap();

        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();
        assert_eq!(storage.worst_pressure().unwrap().0, Pressure::Normal);
        storage.cleanup().unwrap();

        let arguments = fs::read_to_string(&asked).expect("the guest was never asked for anything");
        assert!(
            arguments.contains("fstrim"),
            "cleanup asked the guest for {arguments} instead of a trim"
        );
    }

    /// Verbatim from the host while the macOS runner was crash-looping.
    const CRASH_LOOP_LISTING: &str = "\
82778\t0\tcom.zvuk.kithara-ci.gitlab-runner
-\t0\tcom.zvuk.kithara-ci.health
-\t1\tcom.zvuk.kithara-ci.macos-runner
-\t0\tcom.zvuk.kithara-ci.cleanup
54543\t0\tcom.zvuk.kithara-ci.colima
";

    #[test]
    fn legacy_macos_runner_is_not_health_owned() {
        assert!(!agent_states_from(CRASH_LOOP_LISTING).contains_key("macos-runner"));
    }

    #[test]
    fn the_agents_still_holding_a_process_read_as_running() {
        let states = agent_states_from(CRASH_LOOP_LISTING);
        assert_eq!(states.get("gitlab-runner"), Some(&"running"));
        assert_eq!(states.get("colima"), Some(&"running"));
    }

    /// An agent nobody ever loaded is as unable to take work as one that keeps
    /// dying, and reads differently so an operator knows which to fix.
    #[test]
    fn an_agent_missing_from_the_listing_reads_as_not_loaded() {
        assert_eq!(
            agent_states_from("82778\t0\tcom.zvuk.kithara-ci.gitlab-runner\n").get("colima"),
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

    #[test]
    fn zero_age_prune_preserves_sccache_slot_control_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let control = directory.path().join("cache/.kithara-ci-sccache-slots");
        fs::create_dir_all(&control).unwrap();
        fs::write(control.join("slot-0.lock"), b"").unwrap();
        let cfg = config(directory.path());
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_retired_caches(Duration::ZERO).unwrap();

        assert!(control.is_dir());
        assert!(control.join("slot-0.lock").is_file());
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
        for name in HostStorage::REMOVABLE_ROOTS {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let cfg = config(directory.path());
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
        storage.set_pressure_sequence([Pressure::Normal, Pressure::Normal, Pressure::Normal]);

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
        storage.set_pressure_sequence([Pressure::Normal, Pressure::Normal, Pressure::Normal]);

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
    fn persistent_targets_are_discovered_under_the_checkout_root() {
        let directory = tempfile::tempdir().unwrap();
        let host_root = directory.path().join("host");
        let build_root = directory.path().join("builds");
        let checkout = build_root.join("workspaces/gitlab/project");
        fs::create_dir_all(checkout.join("target/debug")).unwrap();
        fs::write(checkout.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir_all(host_root.join("workspaces/gitlab/stale/target/debug")).unwrap();
        fs::write(
            host_root.join("workspaces/gitlab/stale/Cargo.toml"),
            "[workspace]\n",
        )
        .unwrap();

        let targets =
            build_cache::persistent_target_dirs(&build_root.join("workspaces/gitlab")).unwrap();

        assert_eq!(targets, [checkout.join("target")]);
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
        storage.set_pressure_sequence([Pressure::Normal, Pressure::Normal, Pressure::Normal]);

        storage.cleanup().unwrap();

        assert!(checkout.join("justfile").is_file());
        assert!(checkout.join("xtask/Cargo.toml").is_file());
    }

    #[test]
    fn active_marker_pins_a_workspace() {
        let directory = tempfile::tempdir().unwrap();
        for name in HostStorage::REMOVABLE_ROOTS {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let cfg = config(directory.path());
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
        let stale_time = SystemTime::now() - HostStorage::ACTIVE_LEASE - Duration::from_secs(1);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(FileTimes::new().set_modified(stale_time))
            .unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_pressure_sequence([
            Pressure::Aggressive,
            Pressure::Aggressive,
            Pressure::Normal,
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
        fs::create_dir_all(review.join("sccache-slots/slot-0")).unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_pressure_sequence([Pressure::Aggressive, Pressure::Normal, Pressure::Normal]);

        storage.cleanup().unwrap();

        assert!(review.is_dir());
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
        storage.set_pressure_sequence([
            Pressure::Aggressive,
            Pressure::Aggressive,
            Pressure::Aggressive,
        ]);

        assert!(storage.cleanup().is_err());
    }

    /// A pass that cannot reach the threshold names the tree holding the space.
    ///
    /// Reporting only that every owned step reached its floor is what let this
    /// recur: it rules out the caches and names nothing, so the trees no step
    /// owns are never the thing anyone looks at next.
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
        storage.set_pressure_sequence([
            Pressure::Aggressive,
            Pressure::Aggressive,
            Pressure::Aggressive,
        ]);

        let error = storage.cleanup().unwrap_err().to_string();

        assert!(
            error.contains("vm/tart=400000"),
            "the failure must name the tree holding the space and its size: {error}"
        );
        assert!(
            !error.contains("not build caches"),
            "the old message ruled the caches out and named nothing: {error}"
        );
    }

    #[test]
    fn review_compiler_cache_is_pruned_when_pressure_stays_aggressive() {
        let directory = tempfile::tempdir().unwrap();
        let review = directory.path().join("cache/review/macos-aarch64");
        fs::create_dir_all(review.join("sccache-slots/slot-0")).unwrap();
        let mut cfg = config(directory.path());
        cfg.host.brew_root = directory.path().join("brew");
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_pressure_sequence([
            Pressure::Aggressive,
            Pressure::Aggressive,
            Pressure::Normal,
        ]);

        storage.cleanup().unwrap();

        assert!(!review.exists());
    }

    /// A `tart` that answers a listing and records what else it was asked for.
    #[cfg(unix)]
    fn fake_tart(cfg: &mut CiConfig, root: &Path, running: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        cfg.host.brew_root = root.join("brew");
        let bin = cfg.host.brew_root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let asked = root.join("asked");
        let tart = bin.join("tart");
        fs::write(
            &tart,
            format!(
                "#!/bin/sh\nif [ \"$1\" = list ]; then printf '%s' \
                 '[{{\"Name\":\"kithara-ci-job\",\"Running\":{running},\"State\":\"stopped\"}},\
                 {{\"Name\":\"kithara-macos-base\",\"Running\":false,\"State\":\"stopped\"}}]'; \
                 exit 0; fi\nprintf '%s' \"$*\" > {}\n",
                asked.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tart, fs::Permissions::from_mode(0o755)).unwrap();
        asked
    }

    /// Lay out `<TART_HOME>/vms` with a base bundle and, optionally, a clone
    /// whose parts were last written `since` ago.
    #[cfg(unix)]
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
    #[cfg(unix)]
    #[test]
    fn an_abandoned_job_vm_is_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path(), false);
        tart_vms(&mut cfg, directory.path(), Some(10 * HostStorage::DAY));
        let process = Process::new(directory.path(), BTreeMap::new());
        let mut storage = HostStorage::for_test(&cfg, &process).unwrap();
        storage.set_pressure_sequence([Pressure::Normal, Pressure::Normal, Pressure::Normal]);

        storage.cleanup().unwrap();

        assert_eq!(
            fs::read_to_string(&asked).unwrap_or_default(),
            "delete kithara-ci-job",
            "an abandoned clone must be handed back to tart"
        );
    }

    /// The base bundle is the one thing here that cannot be remade without a
    /// person and an IPSW, so it is named out of the sweep rather than aged out
    /// of it: with no clone beside it, cleanup must ask tart for nothing.
    #[cfg(unix)]
    #[test]
    fn the_base_bundle_is_never_a_delete_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path(), false);
        tart_vms(&mut cfg, directory.path(), None);
        let process = Process::new(directory.path(), BTreeMap::new());
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
    #[cfg(unix)]
    #[test]
    fn a_job_vm_a_runner_serves_from_survives() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path(), true);
        tart_vms(&mut cfg, directory.path(), Some(10 * HostStorage::DAY));
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_abandoned_job_vms(HostStorage::DAY);

        assert!(
            !asked.exists(),
            "a running guest must not be taken from the runner serving jobs on it"
        );
    }

    /// The runner loop leaves the clone stopped from `tart clone` until the
    /// guest answers, which is up to two hundred seconds — longer than the five
    /// minutes this runs on. Deleting it there fails the boot and takes the
    /// runner down with it.
    #[cfg(unix)]
    #[test]
    fn a_freshly_cloned_job_vm_survives() {
        let directory = tempfile::tempdir().unwrap();
        let mut cfg = config(directory.path());
        let asked = fake_tart(&mut cfg, directory.path(), false);
        tart_vms(&mut cfg, directory.path(), Some(Duration::ZERO));
        let process = Process::new(directory.path(), BTreeMap::new());
        let storage = HostStorage::for_test(&cfg, &process).unwrap();

        storage.prune_abandoned_job_vms(HostStorage::DAY);

        assert!(
            !asked.exists(),
            "a clone still booting must survive the interval it boots across"
        );
    }
}
