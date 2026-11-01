use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path as FsPath, PathBuf},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::Ctx;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{
    build_cache,
    build_dir::{LaneTarget, Target},
    cache::missing_defaults,
    config::CiConfig,
    kache::{KacheJob, LocalStore},
    run::CacheGroup,
};
use crate::{consts, job::is_gitlab};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CacheTrust {
    Quarantine,
    Review,
    Trusted,
}

impl CacheTrust {
    /// Every namespace `prepare` can hand a lane, so a sweep over the scratch
    /// root covers everything that can appear in it.
    pub(super) const ALL: [Self; 3] = [Self::Quarantine, Self::Review, Self::Trusted];

    pub(super) fn from_environment() -> Result<Self> {
        Self::read(&process_var)
    }

    /// The trust `KITHARA_CACHE_TRUST` names; `review` when it names none.
    pub(super) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Self> {
        let Some(value) = var("KITHARA_CACHE_TRUST") else {
            return Ok(Self::Review);
        };
        match value.to_str() {
            Some("quarantine") => Ok(Self::Quarantine),
            Some("review") => Ok(Self::Review),
            Some("trusted") => Ok(Self::Trusted),
            _ => bail!(
                "unsupported KITHARA_CACHE_TRUST value: {}",
                value.to_string_lossy()
            ),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
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

fn lease_owner(job_id: Option<&str>, pid: u32) -> Result<String> {
    if let Some(job_id) = job_id {
        Ok(format!("job-{}", parse_decimal_id("CI_JOB_ID", job_id)?))
    } else {
        Ok(format!("pid-{pid}"))
    }
}

/// The name this job holds what is its own under.
pub(crate) fn job_owner() -> Result<String> {
    let job_id = if is_gitlab() {
        Some(env::var("CI_JOB_ID").context("CI_JOB_ID must identify the GitLab job")?)
    } else {
        None
    };
    lease_owner(job_id.as_deref(), std::process::id())
}

fn cache_lease(cache_root: &FsPath) -> Result<PathBuf> {
    Ok(cache_root.join(".kithara-ci-leases").join(job_owner()?))
}

fn kache_local_root(shared: &FsPath, var: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    if let Some(root) = var(consts::KACHE_LOCAL_ROOT_ENV) {
        let root = PathBuf::from(root);
        anyhow::ensure!(root.is_absolute(), "compiler-cache root must be absolute");
        return Ok(root);
    }
    anyhow::ensure!(
        !ci_in(var),
        "CI executor must configure {} on its local filesystem",
        consts::KACHE_LOCAL_ROOT_ENV
    );
    Ok(shared.to_path_buf())
}

/// Where the compiler cache keeps its local store, and how large it may grow.
struct SccachePaths {
    directory: PathBuf,
    cache_size: String,
}

impl SccachePaths {
    fn for_job(cache_root: &FsPath, config: &CiConfig) -> Self {
        Self {
            directory: cache_root.join("sccache"),
            cache_size: config.host.sccache_size.clone(),
        }
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct CiEnvironment {
    #[field(get, vis = "pub(crate)")]
    shared_root: PathBuf,
    pub(crate) cache_root: PathBuf,
    pub(crate) swiftpm_cache: PathBuf,
    pub(crate) temp: PathBuf,
    leases: [PathBuf; 2],
    /// The job's own kache, its directory removed when the job ends.
    #[field(get, vis = "pub(crate)")]
    kache: KacheJob,
    /// Held for the life of the job so a reclaim — this job's own or a sibling
    /// job's — leaves the directory this one builds into alone. The ceiling
    /// still charges its bytes; the claim only says they cannot be taken back.
    _target: Target,
    vars: BTreeMap<OsString, OsString>,
}

impl CiEnvironment {
    pub(crate) fn prepare(
        ctx: &Ctx,
        config: &CiConfig,
        cache_group: CacheGroup,
        lane: LaneTarget<'_>,
    ) -> Result<Self> {
        config.validate()?;
        raise_open_file_limit()?;
        let project_root =
            env::var_os("CI_PROJECT_DIR").map_or_else(|| ctx.root.clone(), PathBuf::from);
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("HOME or USERPROFILE must be set")?;
        let shared_root = prepare_shared_root(config, cache_group, &home)?;

        let trust = CacheTrust::from_environment()?;
        let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
        let cache_root = shared_root.join(trust.as_str()).join(&platform);
        // Entered before anything is reclaimed, including by this job itself:
        // its lease keeps every reclaim away from the directory it builds in.
        let target = Target::enter(&project_root, lane, &process_var)?;
        if is_ci() {
            let volume = match &target {
                Target::Alias { build, .. } => build.path(),
                Target::Named(_) => &project_root,
            };
            ensure_room_for_a_job(config, volume)?;
        }

        let sccache = SccachePaths::for_job(&cache_root, config);
        let gradle_home = cache_root.join("gradle");
        let fixture_cache = shared_root.join(trust.as_str()).join("fixtures");
        let leases = [cache_lease(&cache_root)?, cache_lease(&fixture_cache)?];
        let npm_cache = cache_root.join("npm");
        let swiftpm_cache = cache_root.join("swiftpm");
        let temp = scratch_root().join(trust.as_str());

        for directory in [
            &cache_root,
            &gradle_home,
            &fixture_cache,
            &npm_cache,
            &swiftpm_cache,
            &temp,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("creating CI directory {}", directory.display()))?;
        }
        fs::create_dir_all(&sccache.directory)
            .with_context(|| format!("creating CI directory {}", sccache.directory.display()))?;
        for lease in &leases {
            let lease_root = lease
                .parent()
                .context("CI cache lease must have a parent directory")?;
            fs::create_dir_all(lease_root)
                .with_context(|| format!("creating CI lease directory {}", lease_root.display()))?;
        }
        let mut vars = BTreeMap::new();
        set_path(&mut vars, &home, config)?;
        insert(&mut vars, "CARGO_INCREMENTAL", "0");
        if let Some(dir) = target.cargo_dir() {
            insert(&mut vars, "CARGO_TARGET_DIR", dir);
        }
        // Same reasoning as the justfile's: the system git fetches a large
        // git history far faster, but it fetches with the machine's
        // credentials, and a Linux container has none for the challenge
        // GitHub answers its anonymous request with. Cargo's own client asks
        // anonymously, so it is what the fleet without credentials uses.
        insert(
            &mut vars,
            "CARGO_NET_GIT_FETCH_WITH_CLI",
            if cfg!(target_os = "macos") {
                "true"
            } else {
                "false"
            },
        );
        // Same statement as the GitHub fleet's container: a Linux job links
        // with `lld`. The lane executor is the other way a job reaches this
        // machine, and a linker chosen for only one of them is a measurement
        // that does not carry between them.
        if cfg!(target_os = "linux") {
            for (name, value) in consts::LINUX_LINKER_ENV {
                insert(&mut vars, name, value);
            }
        }
        insert(&mut vars, "GRADLE_USER_HOME", gradle_home);
        // Beside the fixtures, for the reason the Linux fleet keeps them there:
        // a model fetched into a job's own temp directory is newer than the
        // build that embedded it, and Cargo rebuilds everything above it.
        insert(
            &mut vars,
            "KITHARA_BEAT_MODEL_CACHE",
            fixture_cache.join("beat-models"),
        );
        insert(&mut vars, "KITHARA_FIXTURE_CACHE", fixture_cache);
        insert(
            &mut vars,
            "KITHARA_NIGHTLY_TOOLCHAIN",
            &config.pins.nightly_toolchain,
        );
        insert(&mut vars, "npm_config_cache", npm_cache);
        insert(
            &mut vars,
            "RUSTUP_HOME",
            env::var_os("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup").into_os_string()),
        );
        insert_sccache_environment(
            &mut vars,
            &sccache,
            &project_root,
            ctx.config.tools.program("sccache"),
        );
        let kache_root = kache_local_root(&shared_root, &process_var)?;
        let kache_store = kache_root
            .join(trust.as_str())
            .join(&platform)
            .join("kache");
        let kache = KacheJob::prepare(
            &temp.join("kache").join(job_owner()?),
            Some(LocalStore {
                directory: &kache_store,
                max_size: &sccache.cache_size,
            }),
        )?;
        for (name, value) in kache.environment(&ctx.config.tools) {
            insert(&mut vars, name, value);
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
            insert_android_environment(&mut vars, config);
        }

        for lease in &leases {
            fs::write(lease, format!("pid={}\n", std::process::id()))
                .with_context(|| format!("creating CI cache lease {}", lease.display()))?;
        }

        Ok(Self {
            shared_root,
            cache_root,
            swiftpm_cache,
            temp,
            leases,
            kache,
            _target: target,
            vars,
        })
    }

    pub(crate) fn vars(&self) -> BTreeMap<OsString, OsString> {
        self.vars.clone()
    }
}

impl Drop for CiEnvironment {
    fn drop(&mut self) {
        for lease in &self.leases {
            let _ = fs::remove_file(lease);
        }
    }
}

/// Scratch space answers to three constraints at once. It sits outside the
/// checkout, or tools that walk the working tree — the architecture reporter,
/// for one — trip over the temporary copies they just created. It stays short,
/// because macOS caps Unix socket paths at `SUN_LEN` and the suite binds
/// sockets here. And it lives on local storage: the macOS guest reaches the
/// shared cache over virtiofs, which cannot bind a socket at all.
pub(crate) fn scratch_root() -> PathBuf {
    PathBuf::from("/tmp/kithara-ci")
}

#[cfg(unix)]
fn raise_open_file_limit() -> Result<()> {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};

    let (soft, hard) =
        getrlimit(Resource::RLIMIT_NOFILE).context("reading the file descriptor limit")?;
    let target = hard.min(consts::OPEN_FILES);
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

fn prepare_shared_root(
    config: &CiConfig,
    cache_group: CacheGroup,
    home: &FsPath,
) -> Result<PathBuf> {
    let configured = shared_root(config, cache_group);
    let root = if configured.is_dir() {
        configured
    } else if is_ci() {
        bail!("shared CI cache is not mounted at {}", configured.display());
    } else {
        home.join(".cache/kithara-ci")
    };
    fs::create_dir_all(&root)
        .with_context(|| format!("creating CI cache root {}", root.display()))?;
    Ok(root)
}

fn is_ci() -> bool {
    ci_in(&process_var)
}

/// Whether the environment `var` reads is a CI job's.
pub(crate) fn ci_in(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    var("CI").is_some_and(|value| !value.is_empty())
}

/// The process environment, in the shape the readers of an environment take.
pub(super) fn process_var(name: &str) -> Option<OsString> {
    env::var_os(name)
}

/// Refuse a job only once there is nothing left to reclaim and nothing left to
/// wait for.
///
/// The gate and the periodic cleanup never spoke: cleanup ran on a timer and
/// the job arrived when it arrived, so whether a job started came down to how
/// long ago the timer fired. A host under a build loses gigabytes a minute,
/// enough to spend a whole pass's worth of reclaimed space before the next one,
/// and the job landing in that window was refused while tens of gigabytes of
/// evictable compiler cache sat beside it. Growing the disk only moves the
/// window; asking for the space back closes it.
///
/// Asking once does not, because what a reclaim cannot touch is not lost — it
/// is held. A checkout an active job leases is skipped by design, so a host
/// whose slots are all compiling offers no candidate at all, and the refusal
/// reads as "no room" when it means "not yet". Six merge requests were refused
/// this way in one afternoon while the branches beside them went green on the
/// same base; one was 53 MB short, and every one passed on a manual retry
/// against unchanged code. So the gate re-asks until the room appears or the
/// profile's wait runs out, and the retry it was asking a human for costs a
/// poll instead of a whole job.
///
/// The room asked for is on `volume`, the one the job builds on.
fn ensure_room_for_a_job(config: &CiConfig, volume: &FsPath) -> Result<()> {
    let required = config.host.free_bytes_for_a_job();
    let workspaces = gitlab_workspaces(config.host.build_root());
    let cache = config.host.host_root.join("cache");
    let targets = build_cache::budget_roots(&workspaces, &cache)?;
    build_cache::enforce_budget(&targets, config.host.build_cache_budget_bytes()?)?;
    if free_bytes(volume)? >= required {
        return Ok(());
    }
    let deadline = Instant::now() + config.host.job_room_wait();
    loop {
        let free = free_bytes(volume)?;
        let reclaimed_from = reclaim_build_caches(&workspaces, &cache, free, required)?;
        let free = free_bytes(volume)?;
        if free >= required {
            return Ok(());
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            bail!(
                "{}",
                refusal(volume, free, required, &workspaces, reclaimed_from)
            );
        };
        warn!(
            free_bytes = free,
            required_bytes = required,
            shortfall_bytes = required.saturating_sub(free),
            seconds_left = left.as_secs(),
            "waiting for the CI volume to give the job room"
        );
        thread::sleep(consts::JOB_ROOM_POLL.min(left));
    }
}

/// The checkouts whose build caches the budget owns.
fn gitlab_workspaces(build_root: &FsPath) -> PathBuf {
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
fn refusal(
    volume: &FsPath,
    free: u64,
    required: u64,
    workspaces: &FsPath,
    reclaimed_from: usize,
) -> String {
    if reclaimed_from == 0 {
        return format!(
            "the volume holding {} has {free} bytes free and a job needs {required}; no \
             reclaimable build cache sits under {}, so the space is held by live work or by \
             trees the build-cache budget does not own",
            volume.display(),
            workspaces.display()
        );
    }
    format!(
        "the volume holding {} has {free} bytes free after reclaiming from {reclaimed_from} \
         build cache(s); a job needs {required} bytes",
        volume.display()
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
fn reclaim_build_caches(
    workspaces: &FsPath,
    cache: &FsPath,
    free: u64,
    required: u64,
) -> Result<usize> {
    let targets = build_cache::budget_roots(workspaces, cache)?;
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

/// How much room the cache still has. A job reads this through whatever the
/// executor mounted the cache with — a virtiofs share into an ephemeral macOS
/// guest, a bind mount into a container — and those report the filesystem
/// backing the share, which is the host's whole disk rather than the CI volume.
/// Free space survives that translation and still answers the question a job
/// asks; occupancy does not, and comparing the host's disk against a threshold
/// sized for the CI volume rejected every macOS job while the volume was barely
/// half full.
fn free_bytes(path: &FsPath) -> Result<u64> {
    fs4::available_space(path)
        .with_context(|| format!("reading available space for {}", path.display()))
}

fn set_path(
    vars: &mut BTreeMap<OsString, OsString>,
    home: &FsPath,
    config: &CiConfig,
) -> Result<()> {
    let mut paths = vec![home.join(".cargo/bin")];
    if cfg!(target_os = "macos") {
        paths.extend([
            config.host.host_root.join("toolchains/shared-bin"),
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

/// sccache takes its configuration from the environment it starts in, so
/// what the host left out of its store's environment is filled in here rather
/// than trusted to every host file. Cargo runs it under the name `wrapper`
/// gives: cc-rs hands a build script's C compiles to the same wrapper only
/// when it is named after a compiler cache it knows.
fn insert_sccache_environment(
    vars: &mut BTreeMap<OsString, OsString>,
    paths: &SccachePaths,
    project_root: &FsPath,
    wrapper: &str,
) {
    insert(vars, "RUSTC_WRAPPER", wrapper);
    insert(vars, "SCCACHE_BASEDIRS", project_root);
    insert(vars, "SCCACHE_CACHE_SIZE", &paths.cache_size);
    insert(vars, "SCCACHE_DIR", &paths.directory);
    insert(vars, "SCCACHE_IDLE_TIMEOUT", consts::SCCACHE_IDLE_TIMEOUT);
    for (name, value) in missing_defaults(|name| env::var(name).ok()) {
        insert(vars, name, value);
    }
}

/// The Android toolchain a mac host carries. Only that fleet builds for the
/// device, and the paths are the host profile's rather than this crate's.
fn insert_android_environment(vars: &mut BTreeMap<OsString, OsString>, config: &CiConfig) {
    let android_user_home = config.host.host_root.join("toolchains/android-user");
    insert(vars, "ANDROID_HOME", &config.host.android_home);
    insert(
        vars,
        "ANDROID_NDK_HOME",
        config
            .host
            .android_home
            .join("ndk")
            .join(&config.pins.android_ndk_version),
    );
    insert(vars, "ANDROID_USER_HOME", &android_user_home);
    insert(vars, "ANDROID_AVD_HOME", android_user_home.join("avd"));
    let java_home = config.host.java_home();
    if java_home.is_dir() {
        insert(vars, "JAVA_HOME", &java_home);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[cfg(unix)]
    use kithara_devtools::common::project::ProjectConfig;
    use kithara_devtools::lease;

    use super::*;

    fn reclaim(root: &FsPath) -> usize {
        reclaim_build_caches(&gitlab_workspaces(root), &root.join("cache"), 0, u64::MAX).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_jobs_can_resolve_provisioned_shared_tools() {
        let config = super::super::config::fixture();
        let mut vars = BTreeMap::new();

        set_path(&mut vars, FsPath::new("/ci-home"), &config).unwrap();

        let paths = env::split_paths(vars.get(OsStr::new("PATH")).unwrap()).collect::<Vec<_>>();
        assert!(
            paths.contains(&config.host.host_root.join("toolchains/shared-bin")),
            "CI PATH omits provisioned shared tools: {paths:?}"
        );
    }

    #[test]
    fn the_windows_cache_group_uses_the_compiler_cache() {
        let config = super::super::config::fixture();

        let root = FsPath::new("C:/cache/review/windows-x86_64");
        let paths = SccachePaths::for_job(root, &config);
        assert_eq!(paths.directory, root.join("sccache"));
        assert_eq!(paths.cache_size, config.host.sccache_size);
    }

    #[test]
    fn windows_targets_use_the_compiler_cache_for_every_lane() {
        let config = super::super::config::fixture();
        let root = FsPath::new("/cache");

        let paths = SccachePaths::for_job(root, &config);
        assert_eq!(paths.directory, root.join("sccache"));
    }

    #[test]
    fn lease_names_use_the_job_or_local_process() {
        assert_eq!(lease_owner(Some("29"), 41).unwrap(), "job-29");
        assert_eq!(lease_owner(None, 41).unwrap(), "pid-41");
        assert!(lease_owner(Some("../29"), 41).is_err());
    }

    #[test]
    fn a_job_compiles_through_one_cache_at_the_profiles_budget() {
        let config = super::super::config::fixture();
        let root = FsPath::new("/cache/review/macos-aarch64");

        let paths = SccachePaths::for_job(root, &config);

        assert_eq!(paths.directory, root.join("sccache"));
        assert_eq!(paths.cache_size, config.host.sccache_size);
    }

    /// A job compiles through kache, keeps what it compiled in its trust's
    /// store, shares it through its own scope's bucket, and returns where
    /// kache's configuration is.
    #[cfg(unix)]
    fn compiles_through_kache(
        vars: &BTreeMap<OsString, OsString>,
        ctx: &Ctx,
        config: &CiConfig,
        cache_root: &std::path::Path,
    ) -> PathBuf {
        assert_eq!(
            vars.get(OsStr::new("RUSTC_WRAPPER"))
                .map(OsString::as_os_str),
            Some(OsStr::new(ctx.config.tools.program("kache")))
        );
        assert_eq!(
            vars.get(OsStr::new("KACHE_FALLBACK"))
                .map(OsString::as_os_str),
            Some(OsStr::new(ctx.config.tools.program("sccache")))
        );
        let kache_config = PathBuf::from(
            vars.get(OsStr::new("KACHE_CONFIG"))
                .expect("a job tells kache where its configuration is"),
        );
        let kache: toml::Table =
            toml::from_str(&fs::read_to_string(&kache_config).unwrap()).unwrap();
        assert_eq!(
            kache["cache"]["local_store"].as_str().map(PathBuf::from),
            Some(cache_root.join("kache")),
            "what a job compiled stays for the next job of its trust"
        );
        assert_eq!(
            kache["cache"]["local_max_size"].as_str(),
            Some(config.host.sccache_size.as_str())
        );
        assert!(
            kache["cache"].get("remote").is_none(),
            "the remote is the runner's, named in kache's host layer"
        );
        assert!(
            kache_config.starts_with(scratch_root()),
            "kache's socket is bound on local storage"
        );
        kache_config
    }

    /// A GitLab lane builds in a directory of its own beside the checkout,
    /// behind the alias its executor names: Cargo is told the alias, so every
    /// compilation is keyed on one path whichever lane ran there last, and the
    /// checkout's `target` names the lane's directory for the paths artifacts
    /// are collected from. The job claims nothing else: no compiler-cache
    /// socket or slot, and the Cargo home is the executor's.
    #[cfg(unix)]
    #[test]
    fn a_gitlab_lane_builds_beside_its_checkout_behind_the_alias() {
        if env::var_os(consts::LANE_PREPARED).is_some() {
            let root = PathBuf::from(env::var_os(consts::CACHE_ROOT).unwrap());
            let project = PathBuf::from(env::var_os("CI_PROJECT_DIR").unwrap());
            let alias =
                PathBuf::from(env::var_os("CARGO_TARGET_DIR").unwrap()).join(consts::BUILD_ALIAS);
            let ctx = Ctx::new(project.clone(), ProjectConfig::default());
            let mut config = super::super::config::fixture();
            // The gate reads the volume the test runs on; one byte of room is
            // what this job asks of it.
            config.host.reject_bytes = config.host.quota_bytes - 1;

            let environment = CiEnvironment::prepare(
                &ctx,
                &config,
                CacheGroup::Macos,
                LaneTarget {
                    name: "apple-lint",
                    window: consts::DAY,
                },
            )
            .unwrap();
            let vars = environment.vars();
            let build = fs::read_link(&alias).unwrap();

            assert_eq!(
                vars.get(OsStr::new("CARGO_TARGET_DIR")),
                Some(alias.as_os_str().to_owned()).as_ref(),
                "Cargo is told the alias, the one path every lane's compilations share"
            );
            assert_eq!(build.parent(), alias.parent());
            assert_eq!(
                fs::canonicalize(project.join("target")).unwrap(),
                fs::canonicalize(&build).unwrap(),
                "artifact paths name the checkout's target"
            );
            assert!(
                lease::evict(&build).unwrap().is_none(),
                "the job holds the directory it builds in"
            );
            for name in [
                "CARGO_HOME",
                "SCCACHE_SERVER_UDS",
                "CARGO_UNSTABLE_MTIME_ON_USE",
            ] {
                assert!(
                    !vars.contains_key(OsStr::new(name)),
                    "{name} is the executor's to name, or nobody's"
                );
            }
            let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
            let cache_root = root.join("review").join(platform);
            let kache_config = compiles_through_kache(&vars, &ctx, &config, &cache_root);
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_DIR")).map(OsString::as_os_str),
                Some(cache_root.join("sccache").as_os_str())
            );
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_CACHE_SIZE"))
                    .map(OsString::as_os_str),
                Some(OsStr::new(&config.host.sccache_size))
            );
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_IDLE_TIMEOUT"))
                    .map(OsString::as_os_str),
                Some(OsStr::new(consts::SCCACHE_IDLE_TIMEOUT))
            );
            // The host named its store and left the prefix out, as every
            // Linux host file did: the server must still write under it.
            assert_eq!(
                vars.get(OsStr::new("SCCACHE_S3_KEY_PREFIX"))
                    .map(OsString::as_os_str),
                Some(OsStr::new("sccache"))
            );
            assert!(!vars.contains_key(OsStr::new("SCCACHE_REGION")));
            assert_eq!(
                vars.get(OsStr::new("KITHARA_FIXTURE_CACHE"))
                    .map(OsString::as_os_str),
                Some(root.join("review/fixtures").as_os_str())
            );
            let models = vars
                .get(OsStr::new("KITHARA_BEAT_MODEL_CACHE"))
                .expect("a job is told where the beat models live");
            assert!(
                PathBuf::from(models).starts_with(root.join("review")),
                "{models:?} is not in the shared cache of the job's trust"
            );
            let lease = cache_root.join(".kithara-ci-leases/job-29");
            assert!(lease.is_file());
            let fixture_lease = root.join("review/fixtures/.kithara-ci-leases/job-29");
            assert!(
                fixture_lease.is_file(),
                "fixture readers must outlive host cleanup"
            );
            drop(environment);
            assert!(
                !kache_config.exists(),
                "a finished job's daemon directory is gone"
            );
            assert!(!lease.exists());
            assert!(!fixture_lease.exists());
            assert!(
                lease::evict(&build).unwrap().is_some(),
                "a finished job leaves its build to the budget"
            );
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let project = directory
            .path()
            .join("workspaces/gitlab/runner/0/disrupt/kithara");
        fs::create_dir_all(&project).unwrap();
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(init.success());
        let root = project.with_file_name(".kithara.target");
        let output = Command::new(env::current_exe().unwrap())
            .arg("a_gitlab_lane_builds_beside_its_checkout_behind_the_alias")
            .arg("--nocapture")
            .env(consts::LANE_PREPARED, "1")
            .env(consts::CACHE_ROOT, directory.path())
            .env("KITHARA_CI_CACHE_ROOT", directory.path())
            .env(consts::KACHE_LOCAL_ROOT_ENV, directory.path())
            .env("KITHARA_CACHE_TRUST", "review")
            .env("SCCACHE_BUCKET", "kithara-review")
            .env("SCCACHE_ENDPOINT", "http://kithara-ci-cache:9000")
            .env("SCCACHE_REGION", "us-east-1")
            .env_remove("SCCACHE_S3_KEY_PREFIX")
            .env("CI", "true")
            .env("GITLAB_CI", "true")
            .env("CI_PROJECT_DIR", &project)
            .env("CARGO_TARGET_DIR", &root)
            .env("CI_RUNNER_ID", "999")
            .env("CI_CONCURRENT_ID", "1")
            .env("CI_JOB_ID", "29")
            .env("CI_JOB_URL", "https://gitlab.example/-/jobs/29")
            .env("HOME", directory.path().join("home"))
            .env_remove("CARGO_HOME")
            // A GitLab executor names no slots directory; a Linux runner the
            // suite itself runs on does.
            .env_remove(consts::BUILD_SLOTS_ENV)
            .env_remove("SCCACHE_SERVER_UDS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
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
        if env::var_os(consts::FAILED_PREPARE).is_some() {
            let root = PathBuf::from(env::var_os(consts::CACHE_ROOT).unwrap());
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let ctx = Ctx::new(project, ProjectConfig::default());
            let config = super::super::config::fixture();

            let lane = LaneTarget {
                name: "lint",
                window: consts::DAY,
            };
            let Err(error) = CiEnvironment::prepare(&ctx, &config, CacheGroup::Macos, lane) else {
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
            .env(consts::FAILED_PREPARE, "1")
            .env(consts::CACHE_ROOT, directory.path())
            .env("KITHARA_CI_CACHE_ROOT", directory.path())
            .env("KITHARA_CACHE_TRUST", "review")
            .env("GITLAB_CI", "true")
            .env("CI_CONCURRENT_ID", "0")
            .env("CI_JOB_ID", "30")
            .env("HOME", directory.path().join("invalid:home"))
            .env_remove("CI")
            .env_remove("CI_PROJECT_DIR")
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

    #[test]
    fn cache_trust_reads_review_when_unset_and_refuses_what_it_does_not_know() {
        let named = |value: &'static str| {
            move |name: &str| -> Option<OsString> {
                (name == "KITHARA_CACHE_TRUST").then(|| OsString::from(value))
            }
        };

        assert_eq!(
            CacheTrust::read(&|_: &str| -> Option<OsString> { None }).unwrap(),
            CacheTrust::Review
        );
        assert_eq!(
            CacheTrust::read(&named("trusted")).unwrap(),
            CacheTrust::Trusted
        );
        let error = CacheTrust::read(&named("public")).unwrap_err();
        assert!(
            error.to_string().contains("public"),
            "the error must name the value: {error}"
        );
    }

    /// A checkout under `root` and the build root beside it, holding build
    /// `id` with one built unit, the alias naming it as the executor leaves
    /// it. Returns the build's directory.
    #[cfg(unix)]
    fn build_beside_a_checkout(root: &FsPath, id: &str) -> PathBuf {
        let checkout = root.join("workspaces/gitlab/runner-a/0/disrupt/kithara");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("Cargo.toml"), "[package]\n").unwrap();
        let build_root = checkout.with_file_name(".kithara.target");
        let unit = build_root.join(id).join("debug");
        fs::create_dir_all(&unit).unwrap();
        fs::write(unit.join("artifact"), vec![0_u8; 400_000]).unwrap();
        std::os::unix::fs::symlink(id, build_root.join(consts::BUILD_ALIAS)).unwrap();
        build_root.join(id)
    }

    /// A host under a build spends a whole cleanup pass's worth of space before
    /// the next pass fires, so a job arriving in that window used to be refused
    /// while evictable caches sat beside it. The gate reclaims them itself now;
    /// what it must not do is refuse first.
    #[cfg(unix)]
    #[test]
    fn the_gate_reclaims_caches_before_deciding_there_is_no_room() {
        let root = tempfile::tempdir().unwrap();
        let build = build_beside_a_checkout(root.path(), "lint");

        reclaim(root.path());

        assert!(
            !build.join("debug/artifact").exists(),
            "an evictable build cache must be reclaimed, not left for the timer"
        );
    }

    /// A host deployed from a branch serves `production/main` until the branch
    /// merges, and main's lane slots under the cache root fill the volume the
    /// job asks room on. The gate reclaims them like the builds beside the
    /// checkouts.
    #[test]
    fn the_gate_reclaims_the_slots_the_previous_layout_keeps() {
        let root = tempfile::tempdir().unwrap();
        let build = root
            .path()
            .join("cache")
            .join(consts::PREVIOUS_TARGET_SLOTS)
            .join("macos-aarch64-lane-lint-0/cargo/debug");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("artifact"), [0_u8; 8]).unwrap();

        reclaim(root.path());

        assert!(!build.exists(), "an idle slot of main's must be reclaimed");
    }

    /// A sibling job holds the directory it builds into while its tests run.
    /// Cargo has long released `.cargo-lock` by then, so without the claim the
    /// reclaim reads the cache as abandoned and deletes the binaries the tests
    /// are still executing — 1869 of them failed to exec that way before this
    /// existed.
    #[cfg(unix)]
    #[test]
    fn a_leased_build_directory_is_left_alone_by_a_sibling_job() {
        let root = tempfile::tempdir().unwrap();
        let build = build_beside_a_checkout(root.path(), "lint");

        let held = lease::hold(&build).expect("the running job claims its build");

        reclaim(root.path());

        assert!(
            build.join("debug/artifact").exists(),
            "a cache a job is building into must survive another job's reclaim"
        );
        drop(held);

        reclaim(root.path());

        assert!(
            !build.join("debug/artifact").exists(),
            "once the job is gone its cache is evictable again"
        );
    }

    /// A job asks for room on the volume it builds on, and a refusal names
    /// it: the cache share's free space says nothing about the disk the
    /// build fills.
    #[test]
    fn a_refused_job_names_the_volume_it_builds_on() {
        let volume = tempfile::tempdir().unwrap();
        let builds = tempfile::tempdir().unwrap();
        let mut config = super::super::config::fixture();
        config.host.quota_bytes = u64::MAX;
        config.host.reject_bytes = 0;
        config.host.job_room_wait_seconds = 0;
        config.host.build_root = Some(builds.path().to_path_buf());

        let error = ensure_room_for_a_job(&config, volume.path())
            .expect_err("no volume has u64::MAX bytes free");

        assert!(
            error
                .to_string()
                .contains(&volume.path().display().to_string()),
            "the refusal must name the volume the job builds on: {error}"
        );
    }

    #[test]
    fn a_refusal_with_nothing_to_reclaim_does_not_claim_it_reclaimed() {
        let message = refusal(
            FsPath::new("/ci/builds/kithara.target/lint"),
            10,
            20,
            FsPath::new("/ci/workspaces/gitlab"),
            0,
        );

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
        let message = refusal(
            FsPath::new("/ci/builds/kithara.target/lint"),
            10,
            20,
            FsPath::new("/ci/workspaces/gitlab"),
            3,
        );

        assert!(
            message.contains("after reclaiming from 3 build cache(s)"),
            "the refusal must say how many caches it emptied: {message}"
        );
    }

    #[test]
    fn a_workspace_root_that_does_not_exist_has_nothing_to_reclaim() {
        let root = tempfile::tempdir().unwrap();

        let reclaimed_from = reclaim(root.path());

        assert_eq!(reclaimed_from, 0);
    }
}
