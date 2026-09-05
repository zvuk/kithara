use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::ci::SCCACHE_SLOT_CONTROL_NAMESPACE;

/// Directory every Unix executor reads the installed host profile from.
pub(crate) const LANE_CONFIG_DIR: &str = "/etc/kithara-ci";

/// Installed profile of the Mac mini and the guests it hosts, read through
/// `KITHARA_CI_HOST_CONFIG`. A Linux machine carries its own; see
/// [`crate::ci::linux`].
pub(crate) const MAC_CONFIG_PATH: &str = "/etc/kithara-ci/mac-host.toml";

/// Machine profile of one CI host: volumes, accounts, and installed roots.
/// It is provisioned per machine and never tracked in the repository; the
/// reviewed build contract lives in [`super::CiPins`].
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiHost {
    /// How long a cache lease keeps its tree alive before cleanup breaks it.
    #[serde(default = "default_active_lease_hours")]
    pub(crate) active_lease_hours: u64,
    pub(crate) admin_user: String,
    pub(crate) aggressive_cleanup_bytes: u64,
    /// The agents that must hold a process for work to reach this host.
    ///
    /// `cleanup` and `health` are periodic and spend nearly all their life
    /// loaded with nothing running, so a missing process says nothing about
    /// them. These are `KeepAlive`, and a missing process means work stopped.
    #[serde(default = "default_always_on_agents")]
    pub(crate) always_on_agents: Vec<String>,
    pub(crate) android_home: PathBuf,
    pub(crate) brew_root: PathBuf,
    /// Positive decimal gigabytes, such as `25GB`. Defaulted: installed
    /// profiles predate it, and refusing to load would kill the cleanup.
    #[serde(default = "default_build_cache_size")]
    pub(crate) build_cache_size: String,
    /// The cache namespaces this repository still writes to.
    ///
    /// Cleanup prunes by name, so a namespace that stops being written to
    /// becomes invisible rather than stale, and nothing ever comes back for it.
    /// Six gigabytes of `cargo-reapi` stores sat here after that tool came off
    /// the CI path. Anything not named here is pruned on its own age.
    #[serde(default = "default_cache_namespaces")]
    pub(crate) cache_namespaces: Vec<String>,
    pub(crate) cache_root_macos: PathBuf,
    pub(crate) cache_root_linux: PathBuf,
    pub(crate) cache_root_windows: PathBuf,
    pub(crate) ci_uid: u32,
    pub(crate) ci_user: String,
    /// The profile `install-services` starts the Linux guest under.
    #[serde(default = "default_colima_profile")]
    pub(crate) colima_profile: String,
    /// Cores this machine has.
    ///
    /// Job admission and per-job Cargo workers are both carved out of this, and
    /// nothing else states the relation: raising the first without lowering the
    /// second is how a machine ends up with more compilers than cores and no
    /// room left for the runner, sccache, and the linkers. Defaulted: installed
    /// profiles predate the field, and refusing to load would kill every lane.
    #[serde(default = "default_cores")]
    pub(crate) cores: usize,
    /// Maximum jobs admitted at once.
    ///
    /// Runner rendering and per-job cache partitioning share this value.
    /// Raising it buys wall-clock and costs disk: every admitted job carries
    /// its own checkout and `target`, and those are what the volume runs out
    /// of. The compiler cache follows on its own - the host's budget is divided
    /// between the slots. Defaulted for the same reason `cores` is.
    #[serde(default = "default_job_concurrency")]
    pub(crate) job_concurrency: usize,
    /// Size a host log is rotated at.
    #[serde(default = "default_log_limit_bytes")]
    pub(crate) log_limit_bytes: u64,
    pub(crate) macos_guest_shared_root: PathBuf,
    pub(crate) macos_guest_user: String,
    /// Locally built macOS VM bundle cloned for every job.
    pub(crate) macos_vm_bundle: PathBuf,
    pub(crate) macos_guest_xcode_developer_dir: PathBuf,
    pub(crate) gitlab_url: Url,
    pub(crate) host_root: PathBuf,
    /// Where runners check work out. Separate from `host_root` because Apple's
    /// packaging cannot run on a case-sensitive volume — `xcodebuild` writes
    /// `Headers` and `cargo swift package` then removes `headers` — while the
    /// rest of the host root is happy either way. Defaults to `host_root` for
    /// a machine whose volume already folds case.
    #[serde(default)]
    pub(crate) build_root: Option<PathBuf>,
    pub(crate) host_xcode_developer_dir: PathBuf,
    pub(crate) quota_bytes: u64,
    pub(crate) reject_bytes: u64,
    /// The trees directly below a CI root that cleanup may remove whole.
    #[serde(default = "default_removable_roots")]
    pub(crate) removable_roots: Vec<String>,
    /// Aggregate whole-gigabyte sccache budget, divided between host jobs.
    pub(crate) sccache_size: String,
    pub(crate) soft_cleanup_bytes: u64,
    pub(crate) sync_uid: u32,
    pub(crate) sync_user: String,
    /// The Windows guest this machine serves its Windows lane from. Defaulted:
    /// installed profiles predate it, and refusing to load would kill cleanup
    /// on every host that has no Windows guest at all.
    #[serde(default)]
    pub(crate) windows: Option<WindowsGuest>,
}

/// The Windows guest a mac starts directly.
///
/// The Linux host's guest is described to libvirt, which owns its shape from
/// then on. This one is a `qemu` process the host launches itself, so the
/// shape belongs here rather than in a shell script beside the disk image.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub(crate) struct WindowsGuest {
    pub(crate) vcpus: u32,
    pub(crate) memory_mib: u32,
    /// The disk carrying everything the guest writes: the page file, `TEMP`,
    /// and build trees.
    ///
    /// Kept apart from the system image because Windows sizes its page file to
    /// memory, and a `qcow2` grows to hold that and never returns the room on
    /// its own. Eight gigabytes of RAM cost this host eight to twelve inside
    /// the image it can neither shrink in place nor copy — the copy needs as
    /// much free space as the image is large. Trimming the memory instead only
    /// moves the growth: Windows 11 on ARM wants four gigabytes to begin with,
    /// and a Rust build under that swaps, which grows the page file back.
    pub(crate) data_disk_gib: u32,
}

impl CiHost {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading CI host profile {}", path.display()))?;
        let host: Self = toml::from_str(&text)
            .with_context(|| format!("parsing CI host profile {}", path.display()))?;
        host.validate()?;
        Ok(host)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing CI host profile")?;
        fs::write(path, text).with_context(|| format!("writing CI host profile {}", path.display()))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for (name, path) in [
            ("android_home", &self.android_home),
            ("brew_root", &self.brew_root),
            ("cache_root_macos", &self.cache_root_macos),
            ("cache_root_linux", &self.cache_root_linux),
            ("cache_root_windows", &self.cache_root_windows),
            ("macos_guest_shared_root", &self.macos_guest_shared_root),
            ("macos_vm_bundle", &self.macos_vm_bundle),
            (
                "macos_guest_xcode_developer_dir",
                &self.macos_guest_xcode_developer_dir,
            ),
            ("host_root", &self.host_root),
            ("host_xcode_developer_dir", &self.host_xcode_developer_dir),
        ] {
            if path.as_os_str().is_empty() {
                bail!("CI host profile {name} must not be empty");
            }
        }
        if self.job_concurrency == 0 {
            bail!("CI host profile job_concurrency must admit at least one job");
        }
        if self.cores.saturating_sub(1) < self.job_concurrency {
            bail!(
                "CI host profile cores ({}) leave fewer than job_concurrency ({}) spare workers; \
                 every job would build single-threaded",
                self.cores,
                self.job_concurrency
            );
        }
        self.sccache_slot_size()?;
        if self.build_cache_size.trim().is_empty() {
            bail!("CI host profile build_cache_size must not be empty");
        }
        self.build_cache_budget_bytes()?;
        if self.soft_cleanup_bytes == 0
            || self.soft_cleanup_bytes >= self.aggressive_cleanup_bytes
            || self.aggressive_cleanup_bytes >= self.reject_bytes
            || self.reject_bytes >= self.quota_bytes
        {
            bail!("CI disk thresholds must satisfy 0 < soft < aggressive < reject < quota");
        }
        if self.active_lease_hours == 0 {
            bail!(
                "CI host profile active_lease_hours must be at least one hour; at zero every \
                 lease reads as stale and cleanup takes the workspace a running job holds"
            );
        }
        if self.colima_profile.trim().is_empty() {
            bail!(
                "CI host profile colima_profile must name an instance; an empty name addresses \
                 the wrong Docker socket instead of failing"
            );
        }
        if self.ci_uid == 0 || self.sync_uid == 0 || self.ci_uid == self.sync_uid {
            bail!("CI and synchronization UIDs must be distinct non-root values");
        }
        if self.gitlab_url.scheme() != "https"
            || self.gitlab_url.host_str().is_none()
            || !self.gitlab_url.username().is_empty()
            || self.gitlab_url.password().is_some()
            || self.gitlab_url.query().is_some()
            || self.gitlab_url.fragment().is_some()
        {
            bail!("gitlab_url must be an HTTPS origin without credentials or query");
        }
        for (name, user) in [
            ("admin_user", self.admin_user.as_str()),
            ("ci_user", self.ci_user.as_str()),
            ("macos_guest_user", self.macos_guest_user.as_str()),
            ("sync_user", self.sync_user.as_str()),
        ] {
            if !safe_account(user) {
                bail!("CI host profile {name} contains unsupported characters");
            }
        }
        Ok(())
    }

    pub(crate) fn validate_macos_layout(&self) -> Result<()> {
        self.validate()?;
        for (name, path) in [
            ("android_home", &self.android_home),
            ("brew_root", &self.brew_root),
            ("cache_root_macos", &self.cache_root_macos),
            ("macos_guest_shared_root", &self.macos_guest_shared_root),
            ("macos_vm_bundle", &self.macos_vm_bundle),
            (
                "macos_guest_xcode_developer_dir",
                &self.macos_guest_xcode_developer_dir,
            ),
            ("host_root", &self.host_root),
            ("host_xcode_developer_dir", &self.host_xcode_developer_dir),
        ] {
            if !path.is_absolute() {
                bail!("CI host profile {name} must be an absolute macOS path");
            }
        }
        if self.host_root.parent() != Some(Path::new("/Volumes")) {
            bail!("host_root must name a dedicated volume directly below /Volumes");
        }
        if self
            .build_root
            .as_ref()
            .is_some_and(|build_root| !build_root.is_absolute())
        {
            bail!("CI host profile build_root must be an absolute macOS path");
        }
        Ok(())
    }

    pub(crate) fn gitlab_origin(&self) -> String {
        self.gitlab_url.as_str().trim_end_matches('/').to_string()
    }

    pub(crate) fn build_root(&self) -> &Path {
        self.build_root.as_deref().unwrap_or(&self.host_root)
    }

    pub(crate) fn build_cache_budget_bytes(&self) -> Result<u64> {
        parse_build_cache_size(&self.build_cache_size)
            .context("CI host profile build_cache_size is invalid")
    }

    pub(crate) fn sccache_slot_size(&self) -> Result<String> {
        let Some(digits) = self.sccache_size.strip_suffix('G') else {
            bail!("CI host profile sccache_size must be a positive whole number followed by G");
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("CI host profile sccache_size must be a positive whole number followed by G");
        }
        let gigabytes = digits
            .parse::<usize>()
            .context("CI host profile sccache_size must fit in usize gigabytes")?;
        // Divided down, not required to divide evenly. Demanding a multiple
        // made the slot count a property of a hand-written file on the host:
        // raising `job_concurrency` would fail every job on a machine
        // whose budget no longer divided, until somebody edited a file that
        // lives nowhere in this repository. A gigabyte lost to rounding is the
        // cheaper trade.
        let slot = gigabytes / self.job_concurrency;
        if slot == 0 {
            bail!(
                "CI host profile sccache_size must leave a whole gigabyte to each of {} jobs",
                self.job_concurrency
            );
        }
        Ok(format!("{slot}G"))
    }

    /// The headroom a job insists on before it starts. The host stops handing
    /// out work once the CI volume passes `reject_bytes`, so the room left at
    /// that point is what the policy already considers too little to start on.
    pub(crate) const fn free_bytes_for_a_job(&self) -> u64 {
        self.quota_bytes.saturating_sub(self.reject_bytes)
    }

    /// `tart` resolves VM names under `TART_HOME`, and the configured bundle
    /// is `<TART_HOME>/vms/<name>`. A launch agent inherits none of the
    /// shell's environment, so without this it looks in `~/.tart` and cannot
    /// see the base image at all.
    pub(crate) fn tart_home(&self) -> Result<&Path> {
        self.macos_vm_bundle
            .parent()
            .and_then(Path::parent)
            .context("macos_vm_bundle must be <TART_HOME>/vms/<name>")
    }

    /// The guest mounts this bundle instead of carrying its own Xcode, so the
    /// pinned version is whatever the host has, and the image stays small.
    pub(crate) fn host_xcode_app(&self) -> Result<&Path> {
        self.host_xcode_developer_dir
            .parent()
            .and_then(Path::parent)
            .filter(|bundle| bundle.extension().is_some_and(|kind| kind == "app"))
            .context("host_xcode_developer_dir must be <Xcode>.app/Contents/Developer")
    }

    /// `launchd` starts agents with a minimal PATH, so any agent that shells
    /// out to a Homebrew or Cargo tool has to be told where they live.
    pub(crate) fn agent_path(&self, home: &Path) -> String {
        format!(
            "{}:{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
            self.brew_root.join("bin").display(),
            self.brew_root.join("sbin").display(),
            home.join(".cargo/bin").display()
        )
    }

    pub(crate) fn brew_tool(&self, name: &str) -> PathBuf {
        self.brew_root.join("bin").join(name)
    }

    pub(crate) fn java_home(&self) -> PathBuf {
        self.brew_root
            .join("opt/openjdk@17/libexec/openjdk.jdk/Contents/Home")
    }
}

#[derive(Debug)]
pub(crate) struct BuildCacheSizeError {
    value: String,
}

impl fmt::Display for BuildCacheSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "build_cache_size {:?} must be a positive whole number followed by GB and fit in u64 bytes",
            self.value
        )
    }
}

impl Error for BuildCacheSizeError {}

/// Budget a profile inherits when it predates the field, and a floor rather
/// than a fleet's working set: it is one ceiling for every target directory
/// the host holds, so a machine serving more than a couple of runners has to
/// name its own.
///
/// It did not read that way, and the Linux host inherited it. Twenty-five
/// runner caches whose natural total is 524 GB were held to 25 GB, so every
/// hourly pass freed 470 GB and every job that followed compiled the workspace
/// from `unicode-ident` — seven minutes of build in front of two minutes of
/// tests. A profile that serves a fleet sets `build_cache_size` from what the
/// fleet actually builds and what its volume can spare.
pub(crate) fn default_build_cache_size() -> String {
    "25GB".to_owned()
}

/// The Mac mini this fleet was sized on.
const fn default_cores() -> usize {
    10
}

/// Two admitted jobs: what the disk budget and the core count agree on. Two
/// keep the required parallelism while leaving four Cargo workers to each on
/// this ten-core host; three measured slower and evicted a third test tree.
const fn default_job_concurrency() -> usize {
    2
}

const fn default_active_lease_hours() -> u64 {
    12
}

fn default_always_on_agents() -> Vec<String> {
    ["colima", "gitlab-runner"].map(String::from).to_vec()
}

fn default_colima_profile() -> String {
    "kithara".to_owned()
}

const fn default_log_limit_bytes() -> u64 {
    20_000_000
}

fn default_removable_roots() -> Vec<String> {
    ["cache", "logs", "vm", "workspaces"]
        .map(String::from)
        .to_vec()
}

/// The sccache slot control directory is named once, in
/// [`SCCACHE_SLOT_CONTROL_NAMESPACE`], so a profile that never overrides this
/// key cannot spell it a second way.
fn default_cache_namespaces() -> Vec<String> {
    [
        SCCACHE_SLOT_CONTROL_NAMESPACE,
        "bootstrap",
        "gitlab-runner",
        "quarantine",
        "review",
        "trusted",
    ]
    .map(String::from)
    .to_vec()
}

pub(crate) fn parse_build_cache_size(value: &str) -> Result<u64, BuildCacheSizeError> {
    const BYTES_PER_GIGABYTE: u64 = 1_000_000_000;

    let invalid = || BuildCacheSizeError {
        value: value.to_owned(),
    };
    let digits = value.strip_suffix("GB").ok_or_else(&invalid)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|gigabytes| *gigabytes > 0)
        .and_then(|gigabytes| gigabytes.checked_mul(BYTES_PER_GIGABYTE))
        .ok_or_else(invalid)
}

fn safe_account(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cache_size_converts_decimal_gigabytes_to_bytes() {
        assert_eq!(parse_build_cache_size("25GB").unwrap(), 25_000_000_000);
    }

    #[test]
    fn build_cache_size_rejects_garbage() {
        assert!(parse_build_cache_size("twenty-five").is_err());
    }

    #[test]
    fn ci_host_rejects_an_empty_build_cache_size() {
        let mut host = super::super::fixture().host;
        host.build_cache_size.clear();

        assert!(host.validate().is_err());
    }

    #[test]
    fn sccache_budget_is_split_across_host_jobs() {
        let mut host = super::super::fixture().host;
        host.sccache_size = "60G".to_owned();

        assert_eq!(
            host.sccache_slot_size().unwrap(),
            format!("{}G", 60 / host.job_concurrency)
        );
    }

    /// A budget that does not divide evenly is rounded down, not rejected: the
    /// slot count belongs to this repository, and a host profile written by
    /// hand must not be able to veto a change to it.
    #[test]
    fn an_indivisible_sccache_budget_is_rounded_down() {
        let mut host = super::super::fixture().host;
        host.job_concurrency = 3;
        host.sccache_size = "50G".to_owned();

        assert_eq!(host.sccache_slot_size().unwrap(), "16G");
        assert!(host.validate().is_ok());
    }

    #[test]
    fn ci_host_rejects_a_budget_too_small_to_share() {
        let mut host = super::super::fixture().host;
        host.sccache_size = format!("{}G", host.job_concurrency - 1);

        assert!(host.validate().is_err());
    }

    #[test]
    fn ci_host_rejects_a_non_whole_g_sccache_budget() {
        let mut host = super::super::fixture().host;
        host.sccache_size = "50GB".to_owned();

        assert!(host.validate().is_err());
    }

    #[test]
    fn ci_host_load_accepts_a_profile_without_a_build_cache_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.toml");
        let host = super::super::fixture().host;
        host.write(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let without: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("build_cache_size"))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(&path, without).unwrap();

        assert_eq!(
            CiHost::load(&path).unwrap().build_cache_size,
            default_build_cache_size()
        );
    }

    /// Every installed profile predates the guest, and a mac that serves no
    /// Windows lane never grows the section. Refusing to load without it would
    /// take cleanup down on hosts that have nothing to do with Windows.
    #[test]
    fn ci_host_load_accepts_a_profile_with_no_windows_guest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.toml");
        super::super::fixture().host.write(&path).unwrap();

        assert!(CiHost::load(&path).unwrap().windows.is_none());
    }

    #[test]
    fn a_profile_that_declares_a_windows_guest_carries_its_data_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.toml");
        let mut host = super::super::fixture().host;
        host.windows = Some(WindowsGuest {
            vcpus: 4,
            memory_mib: 8192,
            data_disk_gib: 80,
        });
        host.write(&path).unwrap();

        assert_eq!(
            CiHost::load(&path).unwrap().windows.unwrap().data_disk_gib,
            80
        );
    }

    #[test]
    fn ci_host_load_rejects_garbage_build_cache_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.toml");
        let mut host = super::super::fixture().host;
        host.build_cache_size = "25GiB".to_owned();
        host.write(&path).unwrap();

        let error = CiHost::load(&path).unwrap_err();

        assert!(format!("{error:#}").contains("positive whole number followed by GB"));
    }

    #[test]
    fn macos_build_root_must_be_absolute() {
        let mut host = super::super::fixture().host;
        host.build_root = Some(PathBuf::from("case-folding-build-root"));

        let error = host.validate_macos_layout().unwrap_err();

        assert!(format!("{error:#}").contains("build_root must be an absolute macOS path"));
    }

    #[test]
    fn account_names_are_bounded() {
        assert!(safe_account("kithara-ci"));
        assert!(!safe_account("../root"));
        assert!(!safe_account("ci user"));
    }

    /// A job cannot see the CI volume the way the host does, so it asks for
    /// room instead of occupancy. The room it asks for is the same policy read
    /// from the other end, and the ordering `validate` enforces keeps it above
    /// zero.
    #[test]
    fn a_job_asks_for_the_room_the_host_policy_reserves() {
        let mut host = super::super::fixture().host;
        host.soft_cleanup_bytes = 240;
        host.aggressive_cleanup_bytes = 270;
        host.reject_bytes = 285;
        host.quota_bytes = 300;
        assert_eq!(host.free_bytes_for_a_job(), 15);
        assert!(host.validate().is_ok());
        assert!(host.free_bytes_for_a_job() > 0);
    }

    /// Seeded away from the defaults on both sides: a baseline read from the
    /// default cannot prove the check fires.
    #[test]
    fn a_profile_with_fewer_usable_cores_than_jobs_is_refused() {
        let mut host = super::super::fixture().host;
        host.cores = 4;
        host.job_concurrency = 5;

        let error = host
            .validate()
            .expect_err("a machine that cannot give each job a worker must not load");

        assert!(format!("{error:#}").contains("job_concurrency"));
    }

    #[test]
    fn a_profile_that_admits_no_job_at_all_is_refused() {
        let mut host = super::super::fixture().host;
        host.job_concurrency = 0;

        assert!(host.validate().is_err());
    }

    /// Zero is not a short lease: `older_than` reads every marker as expired,
    /// so the sweep takes the workspace the running job is building in.
    #[test]
    fn a_profile_whose_lease_expires_immediately_is_refused() {
        let mut host = super::super::fixture().host;
        host.active_lease_hours = 0;

        let error = host
            .validate()
            .expect_err("a lease that is stale on creation must not load");

        assert!(format!("{error:#}").contains("active_lease_hours"));
    }

    /// An unnamed profile does not fail: it addresses `.colima/docker.sock`,
    /// which is some other instance's socket or nothing at all.
    #[test]
    fn a_profile_without_a_colima_instance_name_is_refused() {
        let mut host = super::super::fixture().host;
        host.colima_profile = "  ".to_owned();

        let error = host
            .validate()
            .expect_err("an unnamed colima instance must not load");

        assert!(format!("{error:#}").contains("colima_profile"));
    }

    #[test]
    fn a_profile_that_leaves_each_job_a_worker_loads() {
        let mut host = super::super::fixture().host;
        host.cores = 4;
        host.job_concurrency = 3;

        host.validate()
            .expect("three jobs fit in three spare cores");
    }

    /// The tracked fixture still describes the machine the constants described.
    #[test]
    fn the_fixture_profile_keeps_todays_topology() {
        let host = super::super::fixture().host;

        assert_eq!(host.cores, 10);
        assert_eq!(host.job_concurrency, 2);
    }

    /// The tracked fixture is the operator's field catalogue
    /// (`docs/guides/ci-host.md`), so it spells the sccache slot directory a
    /// second time. This is the only place that spelling can drift from
    /// [`SCCACHE_SLOT_CONTROL_NAMESPACE`] without a compiler error.
    #[test]
    fn the_fixture_names_the_slot_control_directory_the_way_the_code_does() {
        let host = super::super::fixture().host;

        assert_eq!(
            host.cache_namespaces.first().map(String::as_str),
            Some(SCCACHE_SLOT_CONTROL_NAMESPACE)
        );
    }

    /// An installed profile predating the fields must still load: the binary
    /// reaches every host before its profile does, and one that refused would
    /// take every lane down with it.
    #[test]
    fn a_profile_without_a_topology_still_loads() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("mac-host.toml");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ci-mac-host.toml");
        let text = fs::read_to_string(&source).expect("fixture profile");
        let without: String = text
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                !line.starts_with("cores") && !line.starts_with("job_concurrency")
            })
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(
            !without.contains("cores") && !without.contains("job_concurrency"),
            "the stripped profile must really lack the keys, or the defaults below \
             prove nothing: the fixture already carries today's values"
        );
        fs::write(&path, without).expect("write profile");

        let host = CiHost::load(&path).expect("a profile without a topology loads");

        assert_eq!(host.cores, 10);
        assert_eq!(host.job_concurrency, 2);
    }

    /// Every moved value keeps today's setting, so a profile that configures
    /// none of them behaves exactly as the constants did.
    ///
    /// Installed profiles predate all six keys, so the defaults are not a
    /// corner case: they are what every host on the fleet actually loads.
    #[test]
    fn the_storage_policy_defaults_match_the_constants_they_replace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ci-host.toml");
        let host = super::super::fixture().host;
        host.write(&path).unwrap();
        let mut emitted: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for key in [
            "always_on_agents",
            "colima_profile",
            "active_lease_hours",
            "log_limit_bytes",
            "removable_roots",
            "cache_namespaces",
        ] {
            assert!(
                emitted.remove(key).is_some(),
                "the profile must round-trip {key} so an operator can override it"
            );
        }
        fs::write(&path, toml::to_string_pretty(&emitted).unwrap()).unwrap();

        let host = CiHost::load(&path).unwrap();

        assert_eq!(host.always_on_agents, ["colima", "gitlab-runner"]);
        assert_eq!(host.colima_profile, "kithara");
        assert_eq!(host.active_lease_hours, 12);
        assert_eq!(host.log_limit_bytes, 20_000_000);
        assert_eq!(host.removable_roots, ["cache", "logs", "vm", "workspaces"]);
        assert_eq!(
            host.cache_namespaces,
            [
                SCCACHE_SLOT_CONTROL_NAMESPACE,
                "bootstrap",
                "gitlab-runner",
                "quarantine",
                "review",
                "trusted"
            ]
        );
    }
}
