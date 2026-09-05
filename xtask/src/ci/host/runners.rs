use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::services::launchd;
use crate::ci::{
    config::{CiConfig, MAC_CONFIG_PATH},
    environment::PROVISIONED_LINUX_IMAGE_ENV,
    process::Process,
};

pub(super) struct RunnerManager<'a> {
    pub(super) config: &'a CiConfig,
    pub(super) process: &'a Process,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaunchdServiceState {
    Loaded,
    Absent,
}

impl<'a> RunnerManager<'a> {
    const LEGACY_MACOS_RUNNER_LABEL: &'static str = "com.zvuk.kithara-ci.macos-runner";

    /// Cargo workers one admitted job may occupy.
    ///
    /// Derived rather than written down, because the two numbers drifted apart
    /// the one time they were: admission went from two jobs to three while this
    /// stayed at four, and twelve compilers on ten cores left nothing for the
    /// runner, sccache, and the linkers. The spare core is that remainder.
    fn cargo_build_jobs(&self) -> usize {
        self.config
            .host
            .cores
            .saturating_sub(1)
            .div_euclid(self.config.host.job_concurrency)
    }

    fn cargo_build_jobs_env(&self) -> String {
        format!("CARGO_BUILD_JOBS={}", self.cargo_build_jobs())
    }

    pub(super) const fn new(config: &'a CiConfig, process: &'a Process) -> Self {
        Self { config, process }
    }

    pub(super) fn configure(&self) -> Result<()> {
        require_macos()?;
        self.require_ci_user()?;
        let home = self.ci_home();
        let config_root = home.join(".config/kithara-ci");
        let runner_root = home.join(".gitlab-runner");
        let image_digest_path = config_root.join("linux-image.digest");
        let expected_linux = read_trimmed(&image_digest_path)?;
        let actual_linux = self.linux_image_digest(&home)?;
        if expected_linux != actual_linux {
            bail!("Linux CI image digest changed: expected {expected_linux}, found {actual_linux}");
        }
        let tokens = Tokens::load(&config_root)?;
        for path in [
            &config_root,
            &runner_root,
            &self.config.host.host_root.join("cache/gitlab-runner"),
            &self.config.host.host_root.join("toolchains/shared-bin"),
            &self.config.host.build_root().join("workspaces/gitlab"),
            &self.agent_root(),
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("creating runner directory {}", path.display()))?;
        }
        self.copy_shared_tool("xcodegen")?;
        let current = std::env::current_exe().context("resolving CI executable")?;
        replace_file(
            &current,
            &self
                .config
                .host
                .host_root
                .join("toolchains/shared-bin/kithara-ci"),
        )
        .context("installing CI executable for macOS guests")?;
        for name in ["mac-host.toml", "pins.toml"] {
            replace_file(
                &self.config.host.host_root.join("services").join(name),
                &self
                    .config
                    .host
                    .host_root
                    .join("toolchains/shared-bin")
                    .join(name),
            )
            .with_context(|| format!("installing macOS guest {name}"))?;
        }

        write_secure(
            &runner_root.join("config.toml"),
            &self.runner_config(&home, &tokens),
        )?;
        self.install_runner_agents(&home)?;
        self.process.run(
            path_text(&self.config.host.brew_tool("gitlab-runner"))?,
            &[
                "verify",
                "--config",
                path_text(&runner_root.join("config.toml"))?,
            ],
            "verify GitLab runners",
        )?;
        let uid = self.process.capture("/usr/bin/id", &["-u"], "CI user id")?;
        self.retire_legacy_macos_runner(&format!("gui/{uid}"), Path::new("/bin/launchctl"))?;
        info!("GitLab runner configuration installed");
        Ok(())
    }

    fn retire_legacy_macos_runner(&self, domain: &str, launchctl: &Path) -> Result<()> {
        let service = format!("{domain}/{}", Self::LEGACY_MACOS_RUNNER_LABEL);
        let status = self
            .process
            .command(launchctl)
            .args(["bootout", &service])
            .status()
            .context("starting legacy macOS runner retirement")?;
        if !status.success() {
            match self.legacy_runner_state(launchctl, &service)? {
                LaunchdServiceState::Absent => {}
                LaunchdServiceState::Loaded => {
                    bail!(
                        "retiring legacy macOS runner failed with exit code {}",
                        status.code().unwrap_or(-1)
                    );
                }
            }
        }
        let plist = self
            .agent_root()
            .join(format!("{}.plist", Self::LEGACY_MACOS_RUNNER_LABEL));
        match fs::remove_file(&plist) {
            Ok(()) => info!(path = %plist.display(), "removed legacy macOS runner agent"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing legacy runner agent {}", plist.display()));
            }
        }
        Ok(())
    }

    fn legacy_runner_state(&self, launchctl: &Path, service: &str) -> Result<LaunchdServiceState> {
        let output = self
            .process
            .command(launchctl)
            .args(["print", service])
            .output()
            .context("starting probe for legacy macOS runner")?;
        if output.status.success() {
            return Ok(LaunchdServiceState::Loaded);
        }
        if launchctl_reports_absent(
            service,
            output.status.code(),
            &output.stdout,
            &output.stderr,
        ) {
            return Ok(LaunchdServiceState::Absent);
        }
        bail!(
            "probe legacy macOS runner failed with exit code {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    pub(super) fn activate(&self) -> Result<()> {
        require_macos()?;
        self.require_ci_user()?;
        let uid = self.process.capture("/usr/bin/id", &["-u"], "CI user id")?;
        let domain = format!("gui/{uid}");
        if !self.launchctl_knows(&domain) {
            bail!(
                "the {} GUI session is not active; log in locally first",
                self.config.host.ci_user
            );
        }
        for name in ["cleanup", "health", "colima", "gitlab-runner"] {
            let label = format!("com.zvuk.kithara-ci.{name}");
            let plist = self.agent_root().join(format!("{label}.plist"));
            if !plist.is_file() {
                continue;
            }
            let service = format!("{domain}/{label}");
            let _ = self
                .process
                .command("/bin/launchctl")
                .args(["bootout", &service])
                .status();
            self.await_unload(&service)?;
            self.process.run(
                "/bin/launchctl",
                &["bootstrap", &domain, path_text(&plist)?],
                "load CI launch agent",
            )?;
            self.process.run(
                "/bin/launchctl",
                &["enable", &format!("{domain}/{label}")],
                "enable CI launch agent",
            )?;
            self.process.run(
                "/bin/launchctl",
                &["kickstart", "-k", &format!("{domain}/{label}")],
                "start CI launch agent",
            )?;
        }
        info!("CI user services activated");
        Ok(())
    }

    /// `launchctl print` answers for a domain or a loaded service and fails
    /// otherwise. Its output is enormous, so keep it off the console.
    fn launchctl_knows(&self, target: &str) -> bool {
        self.process
            .command("/bin/launchctl")
            .args(["print", target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// launchd tears a service down asynchronously, and bootstrapping one
    /// that is still on its way out fails with `EIO` — which would leave the
    /// service unloaded and its process stopped.
    fn await_unload(&self, service: &str) -> Result<()> {
        const ATTEMPTS: u32 = 120;
        const POLL: Duration = Duration::from_millis(500);
        for _ in 0..ATTEMPTS {
            if !self.launchctl_knows(service) {
                return Ok(());
            }
            thread::sleep(POLL);
        }
        bail!("{service} is still loaded after being booted out")
    }

    /// The Linux container's ceiling is deliberately below the VM's eight
    /// gigabytes, so a runaway job cannot take the VM down with it. Five was
    /// too low: one `rustc` compiling the largest test crate reached 3.3
    /// gibibytes on its own and the kernel killed it, which Cargo reported
    /// only as "could not compile" with no diagnostic at all.
    ///
    /// Two jobs at a time, not one. The machine has ten cores and twenty-four
    /// gigabytes; a full workspace build peaks around six to eight of them, so
    /// two fit with headroom and three do not. What this does not loosen is
    /// measurement: `kithara-suite` still admits one suite, browser run or perf
    /// lane at a time, so what runs beside another job is lint, a scan or a
    /// framework build — never two things timing themselves.
    ///
    /// macOS is a shell runner on the host rather than a throwaway guest. The
    /// guest cost more than the isolation was worth: it held twelve of the
    /// machine's twenty-four gigabytes, its build tree started empty after every
    /// recycle, and sccache died inside it often enough that jobs compiled
    /// locally — a suite that runs in three minutes took an hour to reach.
    fn runner_config(&self, home: &Path, tokens: &Tokens) -> String {
        let concurrency = self.config.host.job_concurrency;
        let cargo_build_jobs = self.cargo_build_jobs_env();
        let root = self.config.host.host_root.display();
        let builds = self.config.host.build_root().display();
        let url = self.config.host.gitlab_origin();
        let cache = self.config.host.cache_root_linux.display();
        let lane_config = MAC_CONFIG_PATH;
        let image = &self.config.pins.linux_image;
        let provisioned_image = format!("{PROVISIONED_LINUX_IMAGE_ENV}={image}");
        format!(
            "concurrent = {concurrency}\ncheck_interval = 3\nshutdown_timeout = 30\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-linux\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"docker\"\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"KITHARA_CI_CACHE_ROOT={cache}\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"{provisioned_image}\", \"RUSTUP_HOME=/usr/local/rustup\", \"{cargo_build_jobs}\"]\n\
             [runners.docker]\n    host = \"{}\"\n    image = \"{image}\"\n    pull_policy = \"never\"\n    allowed_pull_policies = [\"never\"]\n    allowed_images = [\"{image}\"]\n    cpus = \"5\"\n    memory = \"6500m\"\n    privileged = false\n    disable_cache = true\n    shm_size = 1073741824\n    volumes = [\"{root}/cache:{cache}:rw\", \"{root}/cache/gitlab-runner:/cache:rw\", \"{root}/services/mac-host.toml:{lane_config}:ro\"]\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-macos\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"shell\"\n  shell = \"bash\"\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"{cargo_build_jobs}\"]\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-android\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"shell\"\n  shell = \"bash\"\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"{cargo_build_jobs}\"]\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-release\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"shell\"\n  shell = \"bash\"\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"{cargo_build_jobs}\"]\n",
            tokens.linux,
            docker_host(home, &self.config.host.colima_profile),
            tokens.macos,
            tokens.android,
            tokens.release,
        )
    }

    /// A bind mount is resolved by the Docker daemon, which lives inside
    /// colima's virtual machine — not on the Mac. colima mounts the CI home
    /// and nothing else, so every other source the runner binds was missing
    /// there, and Docker substitutes an empty directory for a source it cannot
    /// find rather than refusing. That is how the Linux lane ran with no host
    /// profile and no shared cache, reporting only that the profile it was
    /// handed had somehow become a directory.
    ///
    /// The mounts name siblings of the CI home rather than the volume root:
    /// colima already mounts the home, and lima rejects one mount nested
    /// inside another.
    fn colima_args(&self, colima: &str) -> Vec<String> {
        let root = &self.config.host.host_root;
        let mut args: Vec<String> = [
            colima,
            "start",
            "--profile",
            self.config.host.colima_profile.as_str(),
            "--foreground",
            "--cpus",
            "5",
            // Linking is this workspace's memory peak, not compiling it. At
            // five gigabytes the kernel killed `ld` outright in both the test
            // and the coverage lane, leaving only "terminated with signal 9"
            // behind. The macOS guest keeps twelve of the host's twenty-four
            // and the two rarely peak together.
            "--memory",
            "8",
            "--disk",
            "100",
            "--vm-type",
            "vz",
            "--vz-rosetta",
            "--mount-type",
            "virtiofs",
        ]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
        for mount in [
            format!("{}:w", root.join("cache").display()),
            format!("{}:w", root.join("services").display()),
        ] {
            args.push("--mount".to_string());
            args.push(mount);
        }
        args
    }

    fn install_runner_agents(&self, home: &Path) -> Result<()> {
        let logs = self.config.host.host_root.join("logs");
        let colima = self.config.host.brew_tool("colima").display().to_string();
        let colima_args = self.colima_args(&colima);
        let colima_args: Vec<&str> = colima_args.iter().map(String::as_str).collect();
        let gitlab_runner = self
            .config
            .host
            .brew_tool("gitlab-runner")
            .display()
            .to_string();
        let agent_path = self.config.host.agent_path(home);
        let agents = [
            (
                "colima",
                launchd(
                    "com.zvuk.kithara-ci.colima",
                    &colima_args,
                    &logs.join("colima.log"),
                    &agent_path,
                    "<key>KeepAlive</key><true/><key>ProcessType</key><string>Background</string>",
                ),
            ),
            (
                "gitlab-runner",
                launchd(
                    "com.zvuk.kithara-ci.gitlab-runner",
                    &[
                        &gitlab_runner,
                        "run",
                        "--config",
                        &home
                            .join(".gitlab-runner/config.toml")
                            .display()
                            .to_string(),
                        "--working-directory",
                        &self
                            .config
                            .host
                            .build_root()
                            .join("workspaces/gitlab")
                            .display()
                            .to_string(),
                    ],
                    &logs.join("gitlab-runner.log"),
                    &agent_path,
                    "<key>KeepAlive</key><true/><key>ProcessType</key><string>Interactive</string>\
                     <key>SoftResourceLimits</key><dict>\
                     <key>NumberOfFiles</key><integer>65536</integer></dict>",
                ),
            ),
        ];
        fs::create_dir_all(self.agent_root())?;
        for (name, contents) in agents {
            let path = self
                .agent_root()
                .join(format!("com.zvuk.kithara-ci.{name}.plist"));
            fs::write(&path, contents)
                .with_context(|| format!("writing runner agent {}", path.display()))?;
        }
        Ok(())
    }

    fn copy_shared_tool(&self, name: &str) -> Result<()> {
        replace_file(
            &self.config.host.brew_tool(name),
            &self
                .config
                .host
                .host_root
                .join("toolchains/shared-bin")
                .join(name),
        )
        .with_context(|| format!("installing shared {name}"))
    }

    pub(super) fn require_ci_user(&self) -> Result<()> {
        let user = self
            .process
            .capture("/usr/bin/id", &["-un"], "current user")?;
        if user != self.config.host.ci_user {
            bail!(
                "run this command as {}, without sudo",
                self.config.host.ci_user
            );
        }
        Ok(())
    }

    pub(super) fn ci_home(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("home")
            .join(&self.config.host.ci_user)
    }

    fn agent_root(&self) -> PathBuf {
        self.ci_home().join("Library/LaunchAgents")
    }
}

pub(super) fn docker_socket(home: &Path, profile: &str) -> PathBuf {
    home.join(".colima").join(profile).join("docker.sock")
}

pub(super) fn docker_host(home: &Path, profile: &str) -> String {
    format!("unix://{}", docker_socket(home, profile).display())
}

pub(super) struct Tokens {
    pub(super) macos: String,
    linux: String,
    android: String,
    release: String,
}

impl Tokens {
    pub(super) fn load(root: &Path) -> Result<Self> {
        Ok(Self {
            macos: read_token(root, "macos")?,
            linux: read_token(root, "linux")?,
            android: read_token(root, "android")?,
            release: read_token(root, "release")?,
        })
    }
}

fn read_token(root: &Path, name: &str) -> Result<String> {
    let path = root.join(format!("runner-{name}.token"));
    let token = read_secret(&path)?;
    if !token.starts_with("glrt-") || token.chars().any(char::is_whitespace) {
        bail!("invalid runner authentication token in {}", path.display());
    }
    Ok(token)
}

pub(super) fn read_secret(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("secret path must be a regular file: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("secret file must have mode 0600: {}", path.display());
        }
    }
    read_trimmed(path)
}

pub(super) fn read_trimmed(path: &Path) -> Result<String> {
    let value = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("file is empty: {}", path.display());
    }
    Ok(trimmed.to_owned())
}

pub(super) fn write_secure(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

/// Provisioning reruns, and some sources are mode `0555` (Homebrew binaries),
/// so the destination cannot be reopened for writing. Stage beside it and
/// rename over it: renaming needs no write permission on the file itself, and
/// it never leaves the destination missing — clearing it first destroyed the
/// only copy whenever a command installed the executable it was running from.
///
/// Renaming is also what keeps a signed executable runnable. macOS validates a
/// signature once per inode and caches the verdict; rewriting the bytes in
/// place leaves that verdict attached to content it no longer describes, and
/// the kernel answers the next exec with SIGKILL. A rename installs a new
/// inode, so the next exec is validated afresh.
pub(super) fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    let name = destination
        .file_name()
        .with_context(|| format!("no file name in destination {}", destination.display()))?;
    let mut staged = name.to_os_string();
    staged.push(format!(".incoming.{}", std::process::id()));
    let staged = destination.with_file_name(staged);

    fs::copy(source, &staged).with_context(|| {
        format!(
            "staging {} beside {}",
            source.display(),
            destination.display()
        )
    })?;
    fs::rename(&staged, destination)
        .inspect_err(|_| {
            let _ = fs::remove_file(&staged);
        })
        .with_context(|| format!("installing {}", destination.display()))
}

fn launchctl_reports_absent(
    service: &str,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> bool {
    // Observed `launchctl print` result for a missing GUI-domain service.
    const NOT_FOUND_EXIT: i32 = 113;
    let Some((domain, label)) = service.rsplit_once('/') else {
        return false;
    };
    let Some(uid) = domain.strip_prefix("gui/") else {
        return false;
    };
    let expected =
        format!("Bad request.\nCould not find service \"{label}\" in domain for user gui: {uid}\n");
    code == Some(NOT_FOUND_EXIT) && stdout.is_empty() && stderr == expected.as_bytes()
}

pub(super) fn require_macos() -> Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("runner host command supports macOS only");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsString};

    use super::*;
    use crate::ci::{config::fixture, host::testing::install_double};

    /// What the runner is actually handed has to fit the machine. Admission and
    /// the per-job Cargo budget are rendered into the same file from two
    /// different constants, and the machine is what pays when they disagree:
    /// raising admission from two jobs to three while the budget stayed at four
    /// put twelve compilers on ten cores, leaving the runner, sccache, and the
    /// linkers nothing to run on.
    #[test]
    fn the_rendered_runner_config_fits_its_jobs_on_the_host() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = fixture();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };

        let rendered: toml::Value = toml::from_str(&manager.runner_config(&home, &tokens)).unwrap();
        let admitted = usize::try_from(rendered["concurrent"].as_integer().unwrap()).unwrap();
        let workers: usize = rendered["runners"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|runner| runner["environment"].as_array().into_iter().flatten())
            .filter_map(toml::Value::as_str)
            .find_map(|entry| entry.strip_prefix("CARGO_BUILD_JOBS="))
            .expect("every runner carries a per-job Cargo budget")
            .parse()
            .expect("the Cargo budget is a worker count");

        let cores = config.host.cores;
        assert!(
            admitted * workers < cores,
            "{admitted} jobs of {workers} workers claim every one of the host's {cores} cores"
        );
    }

    #[test]
    fn installing_an_executable_over_itself_keeps_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("kithara-ci");
        fs::write(&path, b"payload").expect("seed the destination");

        replace_file(&path, &path).expect("install in place");

        assert_eq!(fs::read(&path).expect("read the destination"), b"payload");
    }

    #[cfg(unix)]
    #[test]
    fn installing_over_a_read_only_destination_replaces_it() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"new").expect("seed the source");
        fs::write(&destination, b"old").expect("seed the destination");
        fs::set_permissions(&destination, PermissionsExt::from_mode(0o555))
            .expect("make the destination read-only");

        replace_file(&source, &destination).expect("install over a read-only file");

        assert_eq!(
            fs::read(&destination).expect("read the destination"),
            b"new"
        );
    }

    #[test]
    fn shell_jobs_inherit_interactive_process_priority() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut config = fixture();
        config.host.host_root = directory.path().join("ci");
        let process = Process::new(directory.path(), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);

        manager
            .install_runner_agents(&manager.ci_home())
            .expect("render runner agents");

        let agents = manager.agent_root();
        let colima = fs::read_to_string(agents.join("com.zvuk.kithara-ci.colima.plist"))
            .expect("read colima agent");
        let runner = fs::read_to_string(agents.join("com.zvuk.kithara-ci.gitlab-runner.plist"))
            .expect("read GitLab runner agent");
        assert!(colima.contains("<key>ProcessType</key><string>Background</string>"));
        assert!(runner.contains("<key>ProcessType</key><string>Interactive</string>"));
        assert!(!runner.contains("<key>ProcessType</key><string>Background</string>"));
    }

    #[test]
    fn upgrade_boots_out_and_removes_the_legacy_macos_runner() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut config = fixture();
        config.host.host_root = directory.path().join("ci");
        let asked = directory.path().join("launchctl-arguments");
        let mut vars = BTreeMap::new();
        vars.insert(
            OsString::from("KITHARA_TEST_TRACE"),
            asked.clone().into_os_string(),
        );
        let process = Process::new(directory.path(), vars);
        let manager = RunnerManager::new(&config, &process);
        fs::create_dir_all(manager.agent_root()).expect("create launch agent directory");
        let legacy = manager
            .agent_root()
            .join("com.zvuk.kithara-ci.macos-runner.plist");
        fs::write(&legacy, "legacy").expect("seed legacy launch agent");
        let launchctl = install_double(directory.path(), "launchctl");

        manager
            .retire_legacy_macos_runner("gui/501", &launchctl)
            .expect("retire legacy runner");

        assert!(!legacy.exists());
        assert_eq!(
            fs::read_to_string(asked).expect("read launchctl arguments"),
            "bootout gui/501/com.zvuk.kithara-ci.macos-runner\n"
        );
    }

    fn legacy_runner_fixture(
        vars: BTreeMap<OsString, OsString>,
    ) -> (tempfile::TempDir, CiConfig, Process, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut config = fixture();
        config.host.host_root = directory.path().join("ci");
        let process = Process::new(directory.path(), vars);
        let manager = RunnerManager::new(&config, &process);
        fs::create_dir_all(manager.agent_root()).expect("create launch agent directory");
        let legacy = manager
            .agent_root()
            .join("com.zvuk.kithara-ci.macos-runner.plist");
        fs::write(&legacy, "legacy").expect("seed legacy launch agent");
        let launchctl = install_double(directory.path(), "launchctl");
        (directory, config, process, legacy, launchctl)
    }

    #[test]
    fn failed_legacy_bootout_preserves_a_loaded_service_plist() {
        let (_directory, config, process, legacy, launchctl) =
            legacy_runner_fixture(BTreeMap::from([(
                OsString::from("KITHARA_TEST_RULES"),
                OsString::from("launchctl:bootout:*=7"),
            )]));
        let manager = RunnerManager::new(&config, &process);

        let error = manager
            .retire_legacy_macos_runner("gui/501", &launchctl)
            .expect_err("a loaded service must make the failed bootout fatal");

        assert!(error.to_string().contains("retiring legacy macOS runner"));
        assert!(legacy.is_file());
    }

    #[test]
    fn failed_legacy_bootout_removes_a_confirmed_absent_service_plist() {
        let (_directory, config, process, legacy, launchctl) =
            legacy_runner_fixture(BTreeMap::from([
                (
                    OsString::from("KITHARA_TEST_RULES"),
                    OsString::from("launchctl:bootout:*=7,launchctl:print:*=113"),
                ),
                (
                    OsString::from("KITHARA_TEST_STDERR"),
                    OsString::from(
                        "Bad request.\n\
                         Could not find service \"com.zvuk.kithara-ci.macos-runner\" \
                         in domain for user gui: 501\n",
                    ),
                ),
            ]));
        let manager = RunnerManager::new(&config, &process);

        manager
            .retire_legacy_macos_runner("gui/501", &launchctl)
            .expect("a confirmed absent service completes the migration");

        assert!(!legacy.exists());
    }

    #[test]
    fn legacy_runner_probe_failure_preserves_the_plist() {
        let (_directory, config, process, legacy, launchctl) =
            legacy_runner_fixture(BTreeMap::from([
                (
                    OsString::from("KITHARA_TEST_RULES"),
                    OsString::from("launchctl:bootout:*=7,launchctl:print:*=42"),
                ),
                (
                    OsString::from("KITHARA_TEST_STDERR"),
                    OsString::from("launchctl probe failed\n"),
                ),
            ]));
        let manager = RunnerManager::new(&config, &process);

        let error = manager
            .retire_legacy_macos_runner("gui/501", &launchctl)
            .expect_err("an unclassified probe failure must stop migration");

        assert!(error.to_string().contains("probe legacy macOS runner"));
        assert!(legacy.is_file());
    }

    #[test]
    fn runner_process_works_from_the_selected_checkout_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut config = fixture();
        config.host.host_root = directory.path().join("host");
        config.host.build_root = Some(directory.path().join("builds"));
        let process = Process::new(directory.path(), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);

        manager
            .install_runner_agents(&manager.ci_home())
            .expect("render runner agents");

        let runner = fs::read_to_string(
            manager
                .agent_root()
                .join("com.zvuk.kithara-ci.gitlab-runner.plist"),
        )
        .expect("read GitLab runner agent");
        let selected = config
            .host
            .build_root()
            .join("workspaces/gitlab")
            .display()
            .to_string();
        let stale = config
            .host
            .host_root
            .join("workspaces/gitlab")
            .display()
            .to_string();
        assert!(runner.contains(&selected));
        assert!(!runner.contains(&stale));
    }

    /// Every token gets a runner, and macOS runs on the host. A tag with no
    /// runner behind it does not fail: its jobs sit `pending` until someone
    /// notices, which is how an evening went.
    #[test]
    fn every_token_has_a_runner_and_macos_runs_on_the_host() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = fixture();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };

        let rendered: toml::Value = toml::from_str(&manager.runner_config(&home, &tokens)).unwrap();
        let runners = rendered["runners"].as_array().unwrap();
        for token in ["glrt-macos", "glrt-linux", "glrt-android", "glrt-release"] {
            assert!(
                runners
                    .iter()
                    .any(|runner| runner["token"].as_str() == Some(token)),
                "{token} has no runner, so its jobs will never be taken"
            );
        }

        let macos = runners
            .iter()
            .find(|runner| runner["token"].as_str() == Some("glrt-macos"))
            .unwrap();
        assert_eq!(macos["executor"].as_str(), Some("shell"));
    }

    #[test]
    fn rendered_runner_configs_are_valid_toml_and_yaml() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = fixture();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };

        let rendered: toml::Value = toml::from_str(&manager.runner_config(&home, &tokens)).unwrap();
        assert_eq!(
            rendered["concurrent"].as_integer(),
            Some(config.host.job_concurrency as i64)
        );
        let runners = rendered["runners"].as_array().unwrap();
        assert_eq!(runners.len(), 4, "one registration per runner token");
        assert_eq!(
            runners
                .iter()
                .filter(|runner| runner["executor"].as_str() == Some("docker"))
                .count(),
            1,
            "the disposable Linux platform has one registration"
        );
        assert_eq!(
            runners
                .iter()
                .filter(|runner| runner["executor"].as_str() == Some("shell"))
                .count(),
            3,
            "the host registrations share the global job limit"
        );
        let shell_cache_roots: Vec<&str> = runners
            .iter()
            .filter(|runner| runner["executor"].as_str() == Some("shell"))
            .flat_map(|runner| runner["environment"].as_array().into_iter().flatten())
            .filter_map(toml::Value::as_str)
            .filter(|entry| entry.starts_with("KITHARA_CI_CACHE_ROOT="))
            .collect();
        assert_eq!(shell_cache_roots.len(), 3);
        assert!(
            shell_cache_roots
                .windows(2)
                .all(|roots| roots[0] == roots[1]),
            "all shell registrations must contend on one host-global slot root"
        );
        for runner in runners {
            assert!(
                runner["environment"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| {
                        value.as_str() == Some(manager.cargo_build_jobs_env().as_str())
                    }),
                "{} has no per-job Cargo CPU budget",
                runner["name"]
            );
        }
        let linux = runners
            .iter()
            .find(|runner| runner["name"].as_str() == Some("kithara-mac-mini-linux"))
            .unwrap();
        assert_eq!(
            linux["docker"]["image"].as_str(),
            Some(config.pins.linux_image.as_str())
        );
        assert_eq!(linux["docker"]["pull_policy"].as_str(), Some("never"));
        assert_eq!(
            linux["docker"]["allowed_pull_policies"][0].as_str(),
            Some("never")
        );
        assert_eq!(
            linux["docker"]["allowed_images"][0].as_str(),
            Some(config.pins.linux_image.as_str())
        );
        assert!(
            linux["environment"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| {
                    value.as_str().is_some_and(|entry| {
                        entry
                            == format!("{PROVISIONED_LINUX_IMAGE_ENV}={}", config.pins.linux_image)
                    })
                })
        );
    }

    /// The runner config and the colima agent are written by two different
    /// functions and read by two different programs; nothing but this test
    /// says they have to agree. When they stopped agreeing, Docker filled the
    /// gap with empty directories and the lane failed several steps later,
    /// describing a file as a directory.
    #[test]
    fn colima_mounts_every_source_the_docker_runner_binds() {
        let config = fixture();
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };
        let rendered: toml::Value =
            toml::from_str(&manager.runner_config(&home, &tokens)).expect("runner config is TOML");
        // The pipeline builds `SCCACHE_DIR` out of this, so a runner that
        // leaves it unset would resolve the compiler cache against the
        // filesystem root and fail every build on that executor.
        for runner in rendered["runners"].as_array().expect("runners is an array") {
            let environment = runner["environment"]
                .as_array()
                .expect("every runner declares an environment");
            assert!(
                environment
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|entry| entry.starts_with("KITHARA_CI_CACHE_ROOT=")),
                "{} does not name a cache root",
                runner["name"]
            );
        }

        let volumes = rendered["runners"]
            .as_array()
            .expect("runners is an array")
            .iter()
            .filter_map(|runner| runner.get("docker")?.get("volumes")?.as_array())
            .flatten()
            .filter_map(toml::Value::as_str);

        let args = manager.colima_args("colima");
        let mounts: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "--mount")
            .map(|pair| pair[1].trim_end_matches(":w"))
            .collect();
        assert!(!mounts.is_empty(), "the agent declares no mounts");

        let mut checked = 0;
        for volume in volumes {
            let source = volume.split(':').next().expect("a volume has a source");
            if !source.starts_with('/') || source.starts_with(home.to_str().expect("UTF-8 home")) {
                continue;
            }
            assert!(
                mounts.iter().any(|mount| source.starts_with(mount)),
                "{source} is bound into containers but colima does not mount it"
            );
            checked += 1;
        }
        assert!(checked > 0, "no host-side bind sources were checked");
    }

    /// One colima instance carries the Linux lane, and the profile naming it is
    /// read at three sites: the call that creates the instance, the socket the
    /// runner config hands the Docker executor, and the socket cleanup prunes
    /// the build cache through. A profile left at its default cannot tell a
    /// site that reads the key from one that spells the name out, so this seeds
    /// it away from the default.
    #[test]
    fn the_configured_colima_profile_reaches_the_instance_and_its_socket() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let mut config = fixture();
        config.host.colima_profile = "kithara-probe".to_owned();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };

        let args = manager.colima_args("colima");
        let named = args
            .windows(2)
            .find(|pair| pair[0] == "--profile")
            .map(|pair| pair[1].as_str());
        assert_eq!(named, Some("kithara-probe"));

        let socket = docker_socket(&home, &config.host.colima_profile);
        assert!(
            socket.ends_with(".colima/kithara-probe/docker.sock"),
            "{}",
            socket.display()
        );

        let rendered = manager.runner_config(&home, &tokens);
        assert!(
            rendered.contains(&docker_host(&home, &config.host.colima_profile)),
            "the runner config does not point at the configured profile's socket: {rendered}"
        );
        assert!(
            !rendered.contains(".colima/kithara/docker.sock"),
            "the runner config still names the default profile's socket: {rendered}"
        );
    }

    #[test]
    fn generated_configs_trust_the_platform_store_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = fixture();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let home = config
            .host
            .host_root
            .join("home")
            .join(&config.host.ci_user);
        let tokens = Tokens {
            macos: "glrt-macos".into(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        };

        let runner = manager.runner_config(&home, &tokens);
        for rendered in [&runner] {
            for forbidden in [
                "tls-ca-file",
                "tls_ca_file",
                "ca.crt",
                "kithara-certs",
                "shared-certs",
                "tls-skip-verify",
                "insecure",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "rendered CI configuration must not carry {forbidden}"
                );
            }
        }
        assert!(runner.contains(&config.host.gitlab_origin()));
    }

    /// Homebrew ships tools as mode `0555`, so a plain `fs::copy` onto a
    /// previous provisioning pass fails with EACCES.
    #[cfg(unix)]
    #[test]
    fn replacing_a_read_only_file_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, "new").unwrap();
        fs::write(&destination, "old").unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o555)).unwrap();

        replace_file(&source, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn secret_files_must_not_be_group_or_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        fs::write(&path, "value\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_secret(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_secret(&path).unwrap(), "value");
    }
}
