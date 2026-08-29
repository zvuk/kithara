use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs4::TryLockError;
use kithara_devtools::{Ctx, lease, lock::FileLock};
use tracing::warn;

use super::{
    HOST_JOB_CONCURRENCY, SCCACHE_SLOT_CACHE_NAMESPACE, SCCACHE_SLOT_CONTROL_NAMESPACE,
    build_cache, config::CiConfig, run::CacheGroup,
};

pub(crate) const PROVISIONED_LINUX_IMAGE_ENV: &str = "KITHARA_CI_PROVISIONED_LINUX_IMAGE";

struct SccacheSlot {
    index: usize,
    /// Keeping the lock alive preserves exclusive ownership for the whole job.
    _lock: FileLock,
}

impl SccacheSlot {
    fn acquire_shared(shared_root: &Path) -> Result<Self> {
        let lock_root = shared_root.join(SCCACHE_SLOT_CONTROL_NAMESPACE);
        fs::create_dir_all(&lock_root).with_context(|| {
            format!(
                "creating sccache slot lock directory {}",
                lock_root.display()
            )
        })?;
        for index in 0..HOST_JOB_CONCURRENCY {
            let path = lock_root.join(format!("slot-{index}.lock"));
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .with_context(|| format!("opening sccache slot lock {}", path.display()))?;
            match FileLock::try_exclusive(file) {
                Ok(lock) => return Ok(Self { index, _lock: lock }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => {
                    return Err(error)
                        .with_context(|| format!("locking sccache slot {}", path.display()));
                }
            }
        }
        bail!("all {HOST_JOB_CONCURRENCY} host sccache slots are already in use")
    }

    const fn index(&self) -> usize {
        self.index
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CacheTrust {
    Quarantine,
    Review,
    Trusted,
}

impl CacheTrust {
    /// Every namespace `prepare` can hand a lane, so a sweep over the scratch
    /// root covers everything that can appear in it.
    pub(super) const ALL: [Self; 3] = [Self::Quarantine, Self::Review, Self::Trusted];

    fn from_environment() -> Result<Self> {
        match env::var("KITHARA_CACHE_TRUST")
            .unwrap_or_else(|_| "review".into())
            .as_str()
        {
            "quarantine" => Ok(Self::Quarantine),
            "review" => Ok(Self::Review),
            "trusted" => Ok(Self::Trusted),
            value => bail!("unsupported KITHARA_CACHE_TRUST value: {value}"),
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Quarantine => "quarantine",
            Self::Review => "review",
            Self::Trusted => "trusted",
        }
    }
}

fn parse_decimal_id(name: &str, value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("{name} must be a decimal integer");
    }
    value
        .parse()
        .with_context(|| format!("{name} must be a decimal integer"))
}

fn disposable_slot(value: Option<&str>) -> Result<usize> {
    let value = value.context("CI_CONCURRENT_ID must identify the disposable runner slot")?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("CI_CONCURRENT_ID must be a decimal slot in 0..{HOST_JOB_CONCURRENCY}");
    }
    let slot = value
        .parse::<usize>()
        .context("CI_CONCURRENT_ID must fit in usize")?;
    if slot >= HOST_JOB_CONCURRENCY {
        bail!("CI_CONCURRENT_ID must be a decimal slot in 0..{HOST_JOB_CONCURRENCY}");
    }
    Ok(slot)
}

fn lease_owner(job_id: Option<&str>, pid: u32) -> Result<String> {
    if let Some(job_id) = job_id {
        Ok(format!("job-{}", parse_decimal_id("CI_JOB_ID", job_id)?))
    } else {
        Ok(format!("pid-{pid}"))
    }
}

fn cache_lease(cache_root: &Path) -> Result<PathBuf> {
    let job_id = if is_gitlab() {
        Some(env::var("CI_JOB_ID").context("CI_JOB_ID must identify the GitLab job")?)
    } else {
        None
    };
    let owner = lease_owner(job_id.as_deref(), std::process::id())?;
    Ok(cache_root.join(".kithara-ci-leases").join(owner))
}

struct SccachePaths {
    directory: PathBuf,
    server_uds: Option<PathBuf>,
    cache_size: String,
}

impl SccachePaths {
    fn shared(cache_root: &Path, slot: usize, cache_size: &str) -> Self {
        Self {
            directory: cache_root
                .join(SCCACHE_SLOT_CACHE_NAMESPACE)
                .join(format!("slot-{slot}")),
            server_uds: cfg!(unix).then(|| {
                scratch_root()
                    .join("sccache")
                    .join(format!("slot-{slot}.sock"))
            }),
            cache_size: cache_size.to_owned(),
        }
    }

    fn disposable(cache_root: &Path, slot: usize, cache_size: &str) -> Self {
        Self {
            directory: cache_root
                .join(SCCACHE_SLOT_CACHE_NAMESPACE)
                .join(format!("slot-{slot}")),
            server_uds: None,
            cache_size: cache_size.to_owned(),
        }
    }

    fn local(cache_root: &Path, cache_size: &str) -> Self {
        Self {
            directory: cache_root.join("sccache"),
            server_uds: None,
            cache_size: cache_size.to_owned(),
        }
    }
}

struct PreparedSccache {
    paths: SccachePaths,
    _slot: Option<SccacheSlot>,
}

impl PreparedSccache {
    fn for_environment(
        shared_root: &Path,
        cache_root: &Path,
        config: &CiConfig,
        cache_group: CacheGroup,
    ) -> Result<Option<Self>> {
        Self::for_target(cfg!(windows), shared_root, cache_root, config, cache_group)
    }

    fn for_target(
        target_is_windows: bool,
        shared_root: &Path,
        cache_root: &Path,
        config: &CiConfig,
        cache_group: CacheGroup,
    ) -> Result<Option<Self>> {
        if target_is_windows {
            return Ok(None);
        }
        if !cache_group.uses_sccache() {
            return Ok(None);
        }
        if !is_gitlab() {
            return Ok(Some(Self {
                paths: SccachePaths::local(cache_root, &config.host.sccache_size),
                _slot: None,
            }));
        }
        let cache_size = config.host.sccache_slot_size()?;
        match cache_group {
            CacheGroup::Linux => {
                let concurrent_id = env::var("CI_CONCURRENT_ID")
                    .context("CI_CONCURRENT_ID must identify the disposable runner slot")?;
                let slot = disposable_slot(Some(&concurrent_id))?;
                Ok(Some(Self {
                    paths: SccachePaths::disposable(cache_root, slot, &cache_size),
                    _slot: None,
                }))
            }
            CacheGroup::Macos | CacheGroup::Host => {
                let slot = SccacheSlot::acquire_shared(shared_root)?;
                let paths = SccachePaths::shared(cache_root, slot.index(), &cache_size);
                Ok(Some(Self {
                    paths,
                    _slot: Some(slot),
                }))
            }
            CacheGroup::Windows => Ok(None),
        }
    }
}

pub(crate) struct CiEnvironment {
    shared_root: PathBuf,
    pub(crate) cache_root: PathBuf,
    pub(crate) swiftpm_cache: PathBuf,
    pub(crate) temp: PathBuf,
    lease: PathBuf,
    sccache: Option<PreparedSccache>,
    /// Held for the life of the job so a reclaim — this job's own or a sibling
    /// job's — leaves the directory this one builds into alone. The ceiling
    /// still charges its bytes; the claim only says they cannot be taken back.
    _target: Option<lease::Lease>,
    vars: BTreeMap<OsString, OsString>,
}

impl CiEnvironment {
    pub(crate) fn prepare(ctx: &Ctx, config: &CiConfig, cache_group: CacheGroup) -> Result<Self> {
        config.validate()?;
        raise_open_file_limit()?;
        let project_root =
            env::var_os("CI_PROJECT_DIR").map_or_else(|| ctx.root.clone(), PathBuf::from);
        let target = project_root.join("target");
        // Claimed before anything is reclaimed, including by this job itself:
        // the directory this job is about to build into is the one it must not
        // lose. Its bytes still answer to the ceiling, because
        // `candidate_entries` charges what it keeps, so the claim costs the host
        // nothing it could otherwise have freed.
        let target_lease = lease::hold(&target);
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("HOME or USERPROFILE must be set")?;
        let shared_root = shared_root(config, cache_group);
        let shared_root = if shared_root.is_dir() {
            shared_root
        } else if is_ci() {
            bail!(
                "shared CI cache is not mounted at {}",
                shared_root.display()
            );
        } else {
            home.join(".cache/kithara-ci")
        };
        fs::create_dir_all(&shared_root)
            .with_context(|| format!("creating CI cache root {}", shared_root.display()))?;

        ensure_room_for_a_job(config, &shared_root)?;

        let trust = CacheTrust::from_environment()?;
        let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
        let cache_root = shared_root.join(trust.as_str()).join(platform);
        let sccache =
            PreparedSccache::for_environment(&shared_root, &cache_root, config, cache_group)?;
        let lease = cache_lease(&cache_root)?;
        let cargo_home = cache_root.join("cargo");
        let gradle_home = cache_root.join("gradle");
        let fixture_cache = cache_root.join("fixtures");
        let npm_cache = cache_root.join("npm");
        let swiftpm_cache = cache_root.join("swiftpm");
        let temp = scratch_root().join(trust.as_str());

        for directory in [
            &cache_root,
            &cargo_home,
            &gradle_home,
            &fixture_cache,
            &npm_cache,
            &swiftpm_cache,
            &temp,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("creating CI directory {}", directory.display()))?;
        }
        if let Some(sccache) = &sccache {
            fs::create_dir_all(&sccache.paths.directory).with_context(|| {
                format!(
                    "creating CI directory {}",
                    sccache.paths.directory.display()
                )
            })?;
        }
        let lease_root = lease
            .parent()
            .context("CI cache lease must have a parent directory")?;
        fs::create_dir_all(lease_root)
            .with_context(|| format!("creating CI lease directory {}", lease_root.display()))?;
        if let Some(socket_root) = sccache
            .as_ref()
            .and_then(|sccache| sccache.paths.server_uds.as_deref())
            .and_then(Path::parent)
        {
            fs::create_dir_all(socket_root).with_context(|| {
                format!(
                    "creating sccache socket directory {}",
                    socket_root.display()
                )
            })?;
        }
        let mut vars = BTreeMap::new();
        set_path(&mut vars, &home, config)?;
        insert(&mut vars, "CARGO_HOME", cargo_home);
        insert(&mut vars, "CARGO_INCREMENTAL", "0");
        insert(&mut vars, "CARGO_TARGET_DIR", target);
        insert(&mut vars, "GRADLE_USER_HOME", gradle_home);
        insert(&mut vars, "KITHARA_FIXTURE_CACHE", fixture_cache);
        insert(
            &mut vars,
            "KITHARA_NIGHTLY_TOOLCHAIN",
            &config.pins.nightly_toolchain,
        );
        insert(&mut vars, "npm_config_cache", npm_cache);
        // Everywhere but Windows. `ffmpeg-sys-next` declares a `--cfg` for
        // every library version it knows, which is thousands of them, and the
        // command Cargo builds for it passes what Windows accepts. Cargo hands
        // that to the wrapper through a response file; sccache expands it and
        // spawns the compiler with the arguments themselves, which does not
        // fit: `failed to spawn rustc.exe: The filename or extension is too
        // long. (os error 206)`. The cache is worth less than the lane.
        if sccache.is_some() {
            insert(&mut vars, "RUSTC_WRAPPER", "sccache");
        }
        insert(
            &mut vars,
            "RUSTUP_HOME",
            env::var_os("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup").into_os_string()),
        );
        if let Some(sccache) = &sccache {
            insert(&mut vars, "SCCACHE_BASEDIRS", &project_root);
            insert(&mut vars, "SCCACHE_CACHE_SIZE", &sccache.paths.cache_size);
            insert(&mut vars, "SCCACHE_DIR", &sccache.paths.directory);
            if let Some(server_uds) = &sccache.paths.server_uds {
                insert(&mut vars, "SCCACHE_SERVER_UDS", server_uds);
            }
        }
        insert(&mut vars, "SWIFTPM_CACHE_PATH", &swiftpm_cache);
        insert(&mut vars, "TMPDIR", &temp);
        insert(
            &mut vars,
            "WASM_SLIM_TOOLCHAIN",
            &config.pins.nightly_toolchain,
        );
        if cfg!(windows) {
            insert(&mut vars, "TEMP", &temp);
            insert(&mut vars, "TMP", &temp);
        }

        if cfg!(target_os = "macos") {
            let android_user_home = config.host.host_root.join("toolchains/android-user");
            insert(&mut vars, "ANDROID_HOME", &config.host.android_home);
            insert(
                &mut vars,
                "ANDROID_NDK_HOME",
                config
                    .host
                    .android_home
                    .join("ndk")
                    .join(&config.pins.android_ndk_version),
            );
            insert(&mut vars, "ANDROID_USER_HOME", &android_user_home);
            insert(&mut vars, "ANDROID_AVD_HOME", android_user_home.join("avd"));
            let java_home = config.host.java_home();
            if java_home.is_dir() {
                insert(&mut vars, "JAVA_HOME", &java_home);
            }
        }

        fs::write(&lease, format!("pid={}\n", std::process::id()))
            .with_context(|| format!("creating CI cache lease {}", lease.display()))?;

        Ok(Self {
            shared_root,
            cache_root,
            swiftpm_cache,
            temp,
            lease,
            sccache,
            _target: target_lease,
            vars,
        })
    }

    pub(crate) fn vars(&self) -> BTreeMap<OsString, OsString> {
        self.vars.clone()
    }

    pub(crate) fn shared_root(&self) -> &Path {
        &self.shared_root
    }

    pub(crate) const fn uses_sccache(&self) -> bool {
        self.sccache.is_some()
    }
}

impl Drop for CiEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lease);
    }
}

/// Scratch space answers to three constraints at once. It sits outside the
/// checkout, or tools that walk the working tree — the architecture reporter,
/// for one — trip over the temporary copies they just created. It stays short,
/// because macOS caps Unix socket paths at `SUN_LEN` and the suite binds
/// sockets here. And it lives on local storage: the macOS guest reaches the
/// shared cache over virtiofs, which cannot bind a socket at all.
pub(super) fn scratch_root() -> PathBuf {
    PathBuf::from("/tmp/kithara-ci")
}

/// The integration suite opens far more files across its cache and segment
/// fixtures than the 256 descriptor soft limit a macOS session starts with.
/// The lane raises its own ceiling so every executor gets the same budget.
const OPEN_FILES: u64 = 65536;

#[cfg(unix)]
fn raise_open_file_limit() -> Result<()> {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};

    let (soft, hard) =
        getrlimit(Resource::RLIMIT_NOFILE).context("reading the file descriptor limit")?;
    let target = hard.min(OPEN_FILES);
    if soft >= target {
        return Ok(());
    }
    setrlimit(Resource::RLIMIT_NOFILE, target, hard)
        .context("raising the file descriptor limit")?;
    Ok(())
}

/// Windows hands out handles from a pool and has no per-process ceiling to
/// lift, so the suite already gets the budget the Unix executors have to ask
/// for.
#[cfg(not(unix))]
fn raise_open_file_limit() -> Result<()> {
    Ok(())
}

fn shared_root(config: &CiConfig, cache_group: CacheGroup) -> PathBuf {
    if let Some(root) = env::var_os("KITHARA_CI_CACHE_ROOT") {
        return PathBuf::from(root);
    }
    match cache_group {
        CacheGroup::Macos => config.host.cache_root_macos.clone(),
        CacheGroup::Linux => config.host.cache_root_linux.clone(),
        CacheGroup::Windows => config.host.cache_root_windows.clone(),
        CacheGroup::Host => config.host.host_root.join("cache"),
    }
}

fn is_ci() -> bool {
    env::var_os("CI").is_some_and(|value| !value.is_empty())
}

/// Refuse a job only once there is nothing left to reclaim.
///
/// The gate and the periodic cleanup never spoke: cleanup ran on a timer and
/// the job arrived when it arrived, so whether a job started came down to how
/// long ago the timer fired. A host under a build loses gigabytes a minute,
/// enough to spend a whole pass's worth of reclaimed space before the next one,
/// and the job landing in that window was refused while tens of gigabytes of
/// evictable compiler cache sat beside it. Growing the disk only moves the
/// window; asking for the space back closes it.
fn ensure_room_for_a_job(config: &CiConfig, shared_root: &Path) -> Result<()> {
    let required = config.host.free_bytes_for_a_job();
    let free = free_bytes(shared_root)?;
    if !is_ci() || free >= required {
        return Ok(());
    }
    let workspaces = gitlab_workspaces(config.host.build_root());
    let reclaimed_from = reclaim_build_caches(&workspaces, free, required)?;
    let free = free_bytes(shared_root)?;
    if free < required {
        bail!("{}", refusal(free, required, &workspaces, reclaimed_from));
    }
    Ok(())
}

/// The checkouts whose build caches the budget owns.
fn gitlab_workspaces(build_root: &Path) -> PathBuf {
    build_root.join("workspaces/gitlab")
}

/// Why the job is refused, in the terms of what the reclaim could act on.
///
/// Saying "after reclaiming build caches" when there was nothing to reclaim
/// pointed every reading of this refusal at the compiler cache, which was
/// neither holding the space nor able to give any back: a workspace a live job
/// leases is skipped, so on a host whose only checkout is the running one the
/// reclaim has no candidate at all and the sentence described work that never
/// happened. What an operator needs at that point is the opposite — that the
/// space is held somewhere this gate does not look.
fn refusal(free: u64, required: u64, workspaces: &Path, reclaimed_from: usize) -> String {
    if reclaimed_from == 0 {
        return format!(
            "the CI cache has {free} bytes free and a job needs {required}; no reclaimable build \
             cache sits under {}, so the space is held by live work or by trees the build-cache \
             budget does not own",
            workspaces.display()
        );
    }
    format!(
        "the CI cache has {free} bytes free after reclaiming from {reclaimed_from} build \
         cache(s); a job needs {required} bytes"
    )
}

/// Return what this host accumulated for itself, before deciding there is no
/// room for the job.
///
/// The gate and the periodic cleanup never spoke: cleanup ran on a timer and
/// the job arrived when it arrived, so whether a job started depended on how
/// long ago the timer fired. A host under a build loses gigabytes a minute,
/// which is enough to spend a whole pass's worth of reclaimed space before the
/// next one — and the job that lands in that window is refused while tens of
/// gigabytes of evictable compiler cache sit beside it. Reclaiming here makes
/// the refusal mean what it says: the space is held by live work, not by
/// leftovers.
///
/// What is reclaimed is the shortfall, not the host's ceiling. The ceiling is
/// the hourly pass's question and it answers "nothing to do" whenever the caches
/// happen to sit under it — which is exactly the state a refused job finds
/// itself in, since a full volume is rarely full of build caches alone.
///
/// Failing to reclaim is not itself a refusal — the gate re-reads free space
/// and answers on that.
fn reclaim_build_caches(workspaces: &Path, free: u64, required: u64) -> Result<usize> {
    let targets = build_cache::persistent_target_dirs(workspaces)?;
    if targets.is_empty() {
        warn!(
            free_bytes = free,
            required_bytes = required,
            root = %workspaces.display(),
            "no reclaimable build cache to free before refusing the job"
        );
        return Ok(0);
    }
    let shortfall = required.saturating_sub(free);
    warn!(
        free_bytes = free,
        required_bytes = required,
        shortfall_bytes = shortfall,
        targets = targets.len(),
        "reclaiming build caches before refusing the job"
    );
    build_cache::reclaim_at_least(&targets, shortfall)?;
    Ok(targets.len())
}

pub(crate) fn is_gitlab() -> bool {
    env::var_os("GITLAB_CI").is_some_and(|value| !value.is_empty())
}

/// How much room the cache still has. A job reads this through whatever the
/// executor mounted the cache with — a virtiofs share into an ephemeral macOS
/// guest, a bind mount into a container — and those report the filesystem
/// backing the share, which is the host's whole disk rather than the CI volume.
/// Free space survives that translation and still answers the question a job
/// asks; occupancy does not, and comparing the host's disk against a threshold
/// sized for the CI volume rejected every macOS job while the volume was barely
/// half full.
fn free_bytes(path: &Path) -> Result<u64> {
    fs4::available_space(path)
        .with_context(|| format!("reading available space for {}", path.display()))
}

fn set_path(vars: &mut BTreeMap<OsString, OsString>, home: &Path, config: &CiConfig) -> Result<()> {
    let mut paths = vec![home.join(".cargo/bin")];
    if cfg!(target_os = "macos") {
        paths.extend([
            config.host.android_home.join("cmdline-tools/latest/bin"),
            config.host.android_home.join("emulator"),
            config.host.android_home.join("platform-tools"),
            config.host.brew_root.join("bin"),
        ]);
    }
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let joined = env::join_paths(paths).context("joining CI PATH")?;
    vars.insert(OsString::from("PATH"), joined);
    Ok(())
}

fn insert(
    vars: &mut BTreeMap<OsString, OsString>,
    name: impl AsRef<OsStr>,
    value: impl Into<OsString>,
) {
    vars.insert(name.as_ref().to_os_string(), value.into());
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::process::Command;

    #[cfg(unix)]
    use kithara_devtools::common::project::ProjectConfig;

    use super::*;

    #[cfg(unix)]
    struct ChildEnv;

    #[cfg(unix)]
    impl ChildEnv {
        const CACHE_ROOT: &str = "KITHARA_TEST_CACHE_ROOT";
        const FAILED_PREPARE: &str = "KITHARA_TEST_FAILED_ENV_CHILD";
        const PREPARED: &str = "KITHARA_TEST_PREPARED_ENV_CHILD";
    }

    #[test]
    fn shared_shell_slots_are_exclusive_and_reusable() {
        let directory = tempfile::tempdir().unwrap();

        let held: Vec<SccacheSlot> = (0..HOST_JOB_CONCURRENCY)
            .map(|_| SccacheSlot::acquire_shared(directory.path()).unwrap())
            .collect();

        for (expected, slot) in held.iter().enumerate() {
            assert_eq!(slot.index(), expected);
        }
        assert!(SccacheSlot::acquire_shared(directory.path()).is_err());
        let mut held = held;
        let first = held.remove(0);
        drop(first);
        assert_eq!(
            SccacheSlot::acquire_shared(directory.path())
                .unwrap()
                .index(),
            0
        );
    }

    #[test]
    fn shared_shell_slot_paths_do_not_contain_runner_identity() {
        let root = Path::new("/cache/review/macos-aarch64");

        let paths = SccachePaths::shared(root, 1, "25G");

        let local = SccachePaths::local(root, "50G");
        assert_eq!(paths.directory, root.join("sccache-slots/slot-1"));
        assert!(!paths.directory.starts_with(&local.directory));
        assert!(!local.directory.starts_with(&paths.directory));
        #[cfg(unix)]
        assert_eq!(
            paths.server_uds,
            Some(PathBuf::from("/tmp/kithara-ci/sccache/slot-1.sock"))
        );
        #[cfg(not(unix))]
        assert_eq!(paths.server_uds, None);
        assert_eq!(paths.cache_size, "25G");
    }

    #[test]
    fn disposable_slot_rejects_the_concurrency_boundary() {
        assert!(disposable_slot(Some(&HOST_JOB_CONCURRENCY.to_string())).is_err());
        assert!(disposable_slot(Some(&(HOST_JOB_CONCURRENCY - 1).to_string())).is_ok());
        assert!(disposable_slot(None).is_err());
        assert!(disposable_slot(Some("slot-1")).is_err());
    }

    #[test]
    fn disposable_slots_use_concurrent_id_for_disk_only() {
        let root = Path::new("/cache/review/linux-aarch64");

        assert_eq!(disposable_slot(Some("0")).unwrap(), 0);
        assert_eq!(disposable_slot(Some("1")).unwrap(), 1);
        let paths = SccachePaths::disposable(root, 1, "25G");
        assert_eq!(paths.directory, root.join("sccache-slots/slot-1"));
        assert_eq!(paths.server_uds, None);
    }

    #[test]
    fn windows_prepared_environment_disables_sccache() {
        let config = super::super::config::fixture();
        let root = Path::new("/cache");

        let prepared =
            PreparedSccache::for_environment(root, root, &config, CacheGroup::Windows).unwrap();

        assert!(prepared.is_none());
    }

    #[test]
    fn windows_target_disables_sccache_independently_of_lane() {
        let config = super::super::config::fixture();
        let root = Path::new("/cache");

        let prepared =
            PreparedSccache::for_target(true, root, root, &config, CacheGroup::Macos).unwrap();

        assert!(prepared.is_none());
    }

    #[test]
    fn lease_names_use_the_job_or_local_process() {
        assert_eq!(lease_owner(Some("29"), 41).unwrap(), "job-29");
        assert_eq!(lease_owner(None, 41).unwrap(), "pid-41");
        assert!(lease_owner(Some("../29"), 41).is_err());
    }

    #[test]
    fn shared_slot_endpoint_is_trust_independent_and_disks_are_separate() {
        let review_root = Path::new("/cache/review/macos-aarch64");
        let trusted_root = Path::new("/cache/trusted/macos-aarch64");
        let linux_root = Path::new("/cache/review/linux-aarch64");
        let review = SccachePaths::shared(review_root, 1, "25G");
        let same = SccachePaths::shared(review_root, 1, "25G");
        let trusted = SccachePaths::shared(trusted_root, 1, "25G");
        let linux = SccachePaths::shared(linux_root, 1, "25G");
        let other_slot = SccachePaths::shared(review_root, 0, "25G");

        assert_eq!(review.directory, same.directory);
        assert_eq!(review.server_uds, same.server_uds);
        assert_eq!(review.server_uds, trusted.server_uds);
        assert_ne!(review.directory, trusted.directory);
        assert_ne!(review.directory, linux.directory);
        #[cfg(unix)]
        assert_ne!(review.server_uds, other_slot.server_uds);
        assert_ne!(review.directory, other_slot.directory);
        assert_eq!(review.cache_size, "25G");
        assert_eq!(other_slot.cache_size, "25G");
    }

    #[test]
    fn local_sccache_paths_keep_the_shared_layout_and_configured_budget() {
        let root = Path::new("/cache/review/macos-aarch64");

        let paths = SccachePaths::local(root, "50G");

        assert_eq!(paths.directory, root.join("sccache"));
        assert_eq!(paths.server_uds, None);
        assert_eq!(paths.cache_size, "50G");
    }

    #[cfg(unix)]
    #[test]
    fn gitlab_shell_environment_holds_a_host_slot_and_job_lease() {
        if env::var_os(ChildEnv::PREPARED).is_some() {
            let root = PathBuf::from(env::var_os(ChildEnv::CACHE_ROOT).unwrap());
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let ctx = Ctx::new(project, ProjectConfig::default());
            let config = super::super::config::fixture();

            let environment = CiEnvironment::prepare(&ctx, &config, CacheGroup::Macos).unwrap();
            let vars = environment.vars();
            let cache_root =
                root.join("review")
                    .join(format!("{}-{}", env::consts::OS, env::consts::ARCH));

            assert_eq!(
                vars.get(OsStr::new("SCCACHE_DIR")).map(OsString::as_os_str),
                Some(cache_root.join("sccache-slots/slot-0").as_os_str())
            );
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_SERVER_UDS"))
                    .map(OsString::as_os_str),
                Some(OsStr::new("/tmp/kithara-ci/sccache/slot-0.sock"))
            );
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_CACHE_SIZE"))
                    .map(OsString::as_os_str),
                Some(config.host.sccache_slot_size().unwrap().as_str().as_ref())
            );
            let lease = cache_root.join(".kithara-ci-leases/job-29");
            assert!(lease.is_file());
            let lock_path = root.join(".kithara-ci-sccache-slots/slot-0.lock");
            let contend = || {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .unwrap();
                FileLock::try_exclusive(file)
            };
            assert!(matches!(contend(), Err(TryLockError::WouldBlock)));
            drop(environment);
            assert!(!lease.exists());
            contend().expect("the slot a finished job held must be free");
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let output = Command::new(env::current_exe().unwrap())
            .arg("gitlab_shell_environment_holds_a_host_slot_and_job_lease")
            .arg("--nocapture")
            .env(ChildEnv::PREPARED, "1")
            .env(ChildEnv::CACHE_ROOT, directory.path())
            .env("KITHARA_CI_CACHE_ROOT", directory.path())
            .env("KITHARA_CACHE_TRUST", "review")
            .env("GITLAB_CI", "true")
            .env("CI_RUNNER_ID", "999")
            .env("CI_CONCURRENT_ID", "1")
            .env("CI_JOB_ID", "29")
            .env("HOME", directory.path().join("home"))
            .env_remove("CI")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_prepare_never_publishes_a_cache_lease() {
        if env::var_os(ChildEnv::FAILED_PREPARE).is_some() {
            let root = PathBuf::from(env::var_os(ChildEnv::CACHE_ROOT).unwrap());
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let ctx = Ctx::new(project, ProjectConfig::default());
            let config = super::super::config::fixture();

            let Err(error) = CiEnvironment::prepare(&ctx, &config, CacheGroup::Macos) else {
                panic!("prepare unexpectedly succeeded");
            };
            assert!(error.to_string().contains("joining CI PATH"));
            let cache_root =
                root.join("review")
                    .join(format!("{}-{}", env::consts::OS, env::consts::ARCH));
            assert!(!cache_root.join(".kithara-ci-leases/job-30").exists());
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let output = Command::new(env::current_exe().unwrap())
            .arg("failed_prepare_never_publishes_a_cache_lease")
            .arg("--nocapture")
            .env(ChildEnv::FAILED_PREPARE, "1")
            .env(ChildEnv::CACHE_ROOT, directory.path())
            .env("KITHARA_CI_CACHE_ROOT", directory.path())
            .env("KITHARA_CACHE_TRUST", "review")
            .env("GITLAB_CI", "true")
            .env("CI_CONCURRENT_ID", "0")
            .env("CI_JOB_ID", "30")
            .env("HOME", directory.path().join("invalid:home"))
            .env_remove("CI")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cache_trust_is_strict() {
        assert_eq!(CacheTrust::Review.as_str(), "review");
        assert_eq!(CacheTrust::Quarantine.as_str(), "quarantine");
        assert_eq!(CacheTrust::Trusted.as_str(), "trusted");
    }

    /// A host under a build spends a whole cleanup pass's worth of space before
    /// the next pass fires, so a job arriving in that window used to be refused
    /// while evictable caches sat beside it. The gate reclaims them itself now;
    /// what it must not do is refuse first.
    #[test]
    fn the_gate_reclaims_caches_before_deciding_there_is_no_room() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root
            .path()
            .join("workspaces/gitlab/runner-a/0/disrupt/kithara");
        let target = checkout.join("target/debug");
        fs::create_dir_all(&target).unwrap();
        fs::write(checkout.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(target.join("artifact"), vec![0_u8; 400_000]).unwrap();

        reclaim_build_caches(&gitlab_workspaces(root.path()), 0, u64::MAX).unwrap();

        assert!(
            !target.join("artifact").exists(),
            "an evictable build cache must be reclaimed, not left for the timer"
        );
    }

    /// A sibling job holds the directory it builds into while its tests run.
    /// Cargo has long released `.cargo-lock` by then, so without the claim the
    /// reclaim reads the cache as abandoned and deletes the binaries the tests
    /// are still executing — 1869 of them failed to exec that way before this
    /// existed.
    #[test]
    fn a_leased_build_directory_is_left_alone_by_a_sibling_job() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root
            .path()
            .join("workspaces/gitlab/runner-a/0/disrupt/kithara");
        let target = checkout.join("target/debug");
        fs::create_dir_all(&target).unwrap();
        fs::write(checkout.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(target.join("artifact"), vec![0_u8; 400_000]).unwrap();

        let held = lease::hold(&checkout.join("target")).expect("the running job claims its build");

        reclaim_build_caches(&gitlab_workspaces(root.path()), 0, u64::MAX).unwrap();

        assert!(
            target.join("artifact").exists(),
            "a cache a job is building into must survive another job's reclaim"
        );
        drop(held);

        reclaim_build_caches(&gitlab_workspaces(root.path()), 0, u64::MAX).unwrap();

        assert!(
            !target.join("artifact").exists(),
            "once the job is gone its cache is evictable again"
        );
    }

    #[test]
    fn a_refusal_with_nothing_to_reclaim_does_not_claim_it_reclaimed() {
        let message = refusal(10, 20, Path::new("/ci/workspaces/gitlab"), 0);

        assert!(
            !message.contains("after reclaiming"),
            "a reclaim that never had a candidate must not be reported as done: {message}"
        );
        assert!(
            message.contains("no reclaimable build cache sits under /ci/workspaces/gitlab"),
            "the refusal must name the root it found nothing under: {message}"
        );
    }

    #[test]
    fn a_refusal_that_reclaimed_says_how_much_it_had_to_work_with() {
        let message = refusal(10, 20, Path::new("/ci/workspaces/gitlab"), 3);

        assert!(
            message.contains("after reclaiming from 3 build cache(s)"),
            "the refusal must say how many caches it emptied: {message}"
        );
    }

    #[test]
    fn a_workspace_root_that_does_not_exist_has_nothing_to_reclaim() {
        let root = tempfile::tempdir().unwrap();

        let reclaimed_from =
            reclaim_build_caches(&gitlab_workspaces(root.path()), 0, u64::MAX).unwrap();

        assert_eq!(reclaimed_from, 0);
    }
}
