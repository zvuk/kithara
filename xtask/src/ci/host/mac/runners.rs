use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::services::launchd;
use crate::{
    ci::{cache::client_environment, config::CiConfig, kache, process::Process},
    consts,
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
        let tokens = Tokens::load(&config_root, self.config.host.job_concurrency)?;
        for path in [
            &config_root,
            &runner_root,
            &self.config.host.host_root.join("cache/gitlab-runner"),
            &self.config.host.host_root.join("toolchains/shared-bin"),
            &self.config.host.build_root().join("workspaces/gitlab"),
            &self.config.host.build_root().join(consts::CONTAINER_BUILDS),
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

        for (path, contents) in self.kache_host_files()? {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            // Readable by the container's user: the file names no secret.
            fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        }
        write_secure(
            &runner_root.join("config.toml"),
            &self.runner_config(&home, &tokens)?,
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
    /// It is registered once per admitted job, and each registration takes one
    /// job at a time, so it checks every job out at the same path: a lane bound
    /// to one finds the build root beside that checkout as it left it, where a
    /// registration taking two jobs put the second at another path and a lane
    /// landed on whichever was free.
    ///
    /// Every runner names one Cargo home and keeps it whatever the job's trust:
    /// what it holds is fetched by checksum, and a home per trust gave the
    /// compiler cache a key per home for every dependency. The shell runners
    /// share theirs; the container's sits on the cache share, because Cargo's
    /// package lock does not reach across the virtual machine. The container
    /// also binds its builds directory from the host, so the checkouts and the
    /// build roots beside them outlive the job the way the shell runners' do.
    /// It binds a tree of its own there: a lock taken in the virtual machine is
    /// invisible on the host and the reverse, so a reclaim on either side walks
    /// only the build directories whose leases it sees.
    ///
    /// Each host runner names a compiler-cache socket of its own, which only
    /// `production/main`'s jobs read while the host serves them: a job of this
    /// layout names one per checkout, and a job's variable outranks the
    /// runner's.
    fn runner_config(&self, home: &Path, tokens: &Tokens) -> Result<String> {
        let concurrency = self.config.host.job_concurrency;
        let cargo_build_jobs = self.cargo_build_jobs_env();
        let root = self.config.host.host_root.display();
        let preflight = toml::Value::String(
            [
                format!("{root}/toolchains/shared-bin/kithara-ci"),
                "ci".to_owned(),
                "host".to_owned(),
                "mac".to_owned(),
                "--config".to_owned(),
                format!("{root}/services/mac-host.toml"),
                "--pins".to_owned(),
                format!("{root}/services/pins.toml"),
                "preflight".to_owned(),
            ]
            .into_iter()
            .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" "),
        );
        let builds = self.config.host.build_root().display();
        let container_builds = self.config.host.build_root().join(consts::CONTAINER_BUILDS);
        let container_builds = container_builds.display();
        let url = self.config.host.gitlab_origin();
        let cache = self.config.host.cache_root_linux.display();
        let lane_config = consts::MAC_CONFIG_PATH;
        let cargo_home = consts::CARGO_HOME_DIR;
        let image = consts::LINUX_LATEST_IMAGE;
        let sccache_environment = self.store_environment()?;
        let kache = |file: PathBuf| {
            sccache_environment
                .as_ref()
                .map(|_| ("KACHE_HOST_CONFIG".to_owned(), file.display().to_string()))
        };
        let shell_kache = kache(self.kache_root().join(consts::KACHE_HOST_FILE));
        let docker_kache = kache(
            self.config
                .host
                .cache_root_linux
                .join(consts::KACHE_DIR)
                .join(consts::KACHE_DOCKER_HOST_FILE),
        );
        let sccache_s3 = sccache_environment
            .clone()
            .into_iter()
            .flatten()
            .chain(shell_kache)
            .chain([
                (
                    consts::KACHE_LOCAL_ROOT_ENV.to_owned(),
                    self.config
                        .host
                        .host_root
                        .join("cache")
                        .display()
                        .to_string(),
                ),
                (
                    "SCCACHE_CACHE_SIZE".to_owned(),
                    self.config.host.sccache_size.clone(),
                ),
            ])
            .map(|(name, value)| format!(", \"{name}={value}\""))
            .collect::<String>();
        let docker_sccache_s3 = sccache_environment
            .map(docker_client_environment)
            .into_iter()
            .flatten()
            .chain(docker_kache)
            .chain([
                (
                    consts::KACHE_LOCAL_ROOT_ENV.to_owned(),
                    consts::KACHE_CONTAINER_ROOT.to_owned(),
                ),
                (
                    "SCCACHE_CACHE_SIZE".to_owned(),
                    self.config.host.sccache_size.clone(),
                ),
            ])
            .map(|(name, value)| format!(", \"{name}={value}\""))
            .collect::<String>();
        let macos = tokens
            .macos
            .iter()
            .zip(1..)
            .map(|(token, number)| {
                format!(
                    "[[runners]]\n  name = \"kithara-mac-mini-macos-{number}\"\n  url = \"{url}\"\n  token = \"{token}\"\n  limit = 1\n  executor = \"shell\"\n  shell = \"bash\"\n  pre_get_sources_script = {preflight}\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"CARGO_HOME={builds}/{cargo_home}\", \"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"KITHARA_CI_PINS={root}/services/pins.toml\", \"KITHARA_CI_HOST_BINARY={root}/toolchains/shared-bin/kithara-ci\", \"SCCACHE_SERVER_UDS=/tmp/kithara-mac-mini-macos-{number}-sccache.sock\", \"{cargo_build_jobs}\"{sccache_s3}]\n\n"
                )
            })
            .collect::<String>();
        Ok(format!(
            "concurrent = {concurrency}\ncheck_interval = 3\nshutdown_timeout = 30\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-linux\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"docker\"\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"CARGO_HOME={cache}/{cargo_home}\", \"KITHARA_CI_CACHE_ROOT={cache}\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"RUSTUP_HOME=/usr/local/rustup\", \"{cargo_build_jobs}\"{docker_sccache_s3}]\n\
             [runners.docker]\n    host = \"{}\"\n    image = \"{image}\"\n    pull_policy = \"never\"\n    allowed_pull_policies = [\"never\"]\n    allowed_images = [\"{image}\"]\n    cpus = \"5\"\n    memory = \"6500m\"\n    privileged = false\n    disable_cache = true\n    shm_size = 1073741824\n    volumes = [\"{root}/cache:{cache}:rw\", \"{root}/cache/gitlab-runner:/cache:rw\", \"{root}/services/mac-host.toml:{lane_config}:ro\", \"{container_builds}:{builds}/workspaces/gitlab:rw\"]\n\n\
             {macos}\
             [[runners]]\n  name = \"kithara-mac-mini-android\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"shell\"\n  shell = \"bash\"\n  pre_get_sources_script = {preflight}\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"CARGO_HOME={builds}/{cargo_home}\", \"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"KITHARA_CI_PINS={root}/services/pins.toml\", \"KITHARA_CI_HOST_BINARY={root}/toolchains/shared-bin/kithara-ci\", \"SCCACHE_SERVER_UDS=/tmp/kithara-mac-mini-android-sccache.sock\", \"{cargo_build_jobs}\"{sccache_s3}]\n\n\
             [[runners]]\n  name = \"kithara-mac-mini-release\"\n  url = \"{url}\"\n  token = \"{}\"\n  executor = \"shell\"\n  shell = \"bash\"\n  pre_get_sources_script = {preflight}\n  builds_dir = \"{builds}/workspaces/gitlab\"\n  output_limit = 16384\n  environment = [\"CARGO_HOME={builds}/{cargo_home}\", \"KITHARA_CI_CACHE_ROOT={root}/cache\", \"KITHARA_CI_HOST_CONFIG={lane_config}\", \"KITHARA_CI_PINS={root}/services/pins.toml\", \"KITHARA_CI_HOST_BINARY={root}/toolchains/shared-bin/kithara-ci\", \"SCCACHE_SERVER_UDS=/tmp/kithara-mac-mini-release-sccache.sock\", \"{cargo_build_jobs}\"{sccache_s3}]\n",
            tokens.linux,
            docker_host(home, &self.config.host.colima_profile),
            tokens.android,
            tokens.release,
        ))
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
    /// inside another. The container's builds directory sits on the build
    /// volume, beside none of them, and apart from the tree the host's runners
    /// build in.
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
            format!(
                "{}:w",
                self.config
                    .host
                    .build_root()
                    .join(consts::CONTAINER_BUILDS)
                    .display()
            ),
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

    /// kache's host files, by where they are written: one for the shell
    /// runners and one for the container runner, which reaches the store and
    /// the cache endpoint by other paths. Without a scoped store there is no
    /// remote to name, and kache keeps its defaults.
    ///
    /// They sit in a namespace of the cache share, not in a trust's: a runner
    /// serves every trust, and a job that places its own store does so in
    /// its own configuration, which outranks this layer.
    pub(super) fn kache_host_files(&self) -> Result<Vec<(PathBuf, String)>> {
        let Some(environment) = self.store_environment()? else {
            return Ok(Vec::new());
        };
        let shared = self.kache_root();
        let size = &self.config.host.sccache_size;
        Ok(vec![
            (
                shared.join(consts::KACHE_HOST_FILE),
                kache::host_config(
                    &environment,
                    &self
                        .config
                        .host
                        .host_root
                        .join("cache/bootstrap")
                        .join(format!("macos-{}", std::env::consts::ARCH))
                        .join(consts::KACHE_DIR),
                    size,
                )?,
            ),
            (
                shared.join(consts::KACHE_DOCKER_HOST_FILE),
                kache::host_config(
                    &docker_client_environment(environment),
                    &Path::new(consts::KACHE_CONTAINER_ROOT)
                        .join("bootstrap")
                        .join(format!("linux-{}", std::env::consts::ARCH))
                        .join(consts::KACHE_DIR),
                    size,
                )?,
            ),
        ])
    }

    fn kache_root(&self) -> PathBuf {
        self.config
            .host
            .host_root
            .join("cache")
            .join(consts::KACHE_DIR)
    }

    pub(super) fn store_environment(&self) -> Result<Option<BTreeMap<String, String>>> {
        self.config
            .host
            .sccache_s3_env_file
            .as_deref()
            .map(client_environment)
            .transpose()
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

pub(super) fn docker_client_environment(
    mut environment: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    if let Some(endpoint) = environment.get_mut("SCCACHE_ENDPOINT") {
        for prefix in ["http://127.0.0.1:", "https://127.0.0.1:"] {
            if let Some(port) = endpoint.strip_prefix(prefix) {
                let scheme = prefix
                    .split_once("://")
                    .map_or("http", |(scheme, _)| scheme);
                *endpoint = format!("{scheme}://host.docker.internal:{port}");
                break;
            }
        }
    }
    environment
}

pub(super) struct Tokens {
    pub(super) macos: Vec<String>,
    linux: String,
    android: String,
    release: String,
}

impl Tokens {
    /// Reads the platform tokens and one for each of `registrations` macOS
    /// runners, numbered from one.
    pub(super) fn load(root: &Path, registrations: usize) -> Result<Self> {
        Ok(Self {
            macos: (1..=registrations)
                .map(|number| read_token(root, &format!("macos-{number}")))
                .collect::<Result<_>>()?,
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

pub(crate) fn read_secret(path: &Path) -> Result<String> {
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

pub(crate) fn write_secure(path: &Path, contents: &str) -> Result<()> {
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::{OsStr, OsString},
    };

    use super::*;
    use crate::{ci::config::fixture, testing::install_double};

    /// A token for every registration the host renders: one per macOS job it
    /// admits, and one for each other platform.
    fn tokens(config: &CiConfig) -> Tokens {
        Tokens {
            macos: (1..=config.host.job_concurrency)
                .map(|number| format!("glrt-macos-{number}"))
                .collect(),
            linux: "glrt-linux".into(),
            android: "glrt-android".into(),
            release: "glrt-release".into(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_jobs_stop_before_sources_when_host_preflight_rejects() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = fixture();
        config.host.host_root = directory.path().join("host root");
        let bin = config.host.host_root.join("toolchains/shared-bin");
        fs::create_dir_all(&bin).unwrap();
        install_double(&bin, "kithara-ci");
        let process = Process::new(directory.path(), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens(&config))
                .unwrap(),
        )
        .unwrap();
        for runner in rendered["runners"].as_array().unwrap() {
            if runner["executor"].as_str() != Some("shell") {
                continue;
            }
            let hook = runner
                .get("pre_get_sources_script")
                .and_then(toml::Value::as_str)
                .expect("native jobs must check host admission before fetching sources");
            for code in [0, 73] {
                let trace = directory.path().join("preflight-arguments");
                let output = std::process::Command::new("bash")
                    .args(["-e", "-c", &format!("{hook}\nprintf 'admitted\\n'")])
                    .env("KITHARA_TEST_RULES", format!("kithara-ci:ci:*={code}"))
                    .env("KITHARA_TEST_TRACE", &trace)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(code), "{output:?}");
                assert_eq!(
                    output.stdout,
                    if code == 0 {
                        b"admitted\n".as_slice()
                    } else {
                        b""
                    },
                    "a rejected host must not start the source step"
                );
                assert!(
                    fs::read_to_string(&trace)
                        .unwrap()
                        .ends_with(" preflight\n")
                );
            }
        }
    }

    /// The budget has to leave the host a core. It is derived rather than
    /// written down because the two numbers drifted apart the one time they
    /// were separate: admission went from two jobs to three while the budget
    /// stayed at four, and twelve compilers on ten cores left the runner,
    /// sccache, and the linkers nothing to run on. Dividing admission into
    /// `cores - 1` is what keeps the remainder, and a formula dividing into
    /// `cores` would hand every core away and still render.
    ///
    /// That every rendered block carries the budget is
    /// `rendered_runner_configs_are_valid_toml_and_yaml`'s claim.
    #[test]
    fn the_cargo_budget_leaves_the_host_a_spare_core() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = fixture();
        let process = Process::new(&root, BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);

        let admitted = config.host.job_concurrency;
        let workers = manager.cargo_build_jobs();
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

        assert!(
            error.to_string().contains("retiring legacy macOS runner"),
            "unexpected retirement error: {error:#}"
        );
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

        manager
            .retire_legacy_macos_runner("gui/501", &launchctl)
            .expect_err("an unclassified probe failure must stop migration");

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
        let tokens = tokens(&config);

        let rendered: toml::Value =
            toml::from_str(&manager.runner_config(&home, &tokens).unwrap()).unwrap();
        let runners = rendered["runners"].as_array().unwrap();
        let runner = |token: &str| {
            runners
                .iter()
                .find(|runner| runner["token"].as_str() == Some(token))
                .unwrap_or_else(|| panic!("{token} has no runner, so its jobs will never be taken"))
        };
        for token in [&tokens.linux, &tokens.android, &tokens.release] {
            runner(token);
        }
        for token in &tokens.macos {
            assert_eq!(runner(token)["executor"].as_str(), Some("shell"));
        }
    }

    /// The host takes one macOS job at a time per registration, and registers
    /// as many as it admits jobs. A registration holding one job checks it out
    /// at the same path every time, so a lane bound to it finds the build root
    /// beside that checkout as it left it; a registration holding two puts the
    /// second at another path, and a lane lands on whichever is free.
    #[test]
    fn the_macos_host_registers_one_single_job_runner_per_admitted_job() {
        let config = fixture();
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let tokens = tokens(&config);
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens)
                .expect("render runner config"),
        )
        .expect("runner config is TOML");

        let macos: Vec<&toml::Value> = rendered["runners"]
            .as_array()
            .expect("runners")
            .iter()
            .filter(|runner| {
                tokens
                    .macos
                    .iter()
                    .any(|token| runner["token"].as_str() == Some(token))
            })
            .collect();
        assert_eq!(
            macos.len(),
            config.host.job_concurrency,
            "one macOS registration per admitted job"
        );
        for runner in &macos {
            assert_eq!(
                runner.get("limit").and_then(toml::Value::as_integer),
                Some(1),
                "{} takes more than one job at a time",
                runner["name"]
            );
        }
        let names: BTreeSet<&str> = macos
            .iter()
            .filter_map(|runner| runner["name"].as_str())
            .collect();
        assert_eq!(
            names.len(),
            macos.len(),
            "two macOS registrations share a name"
        );
    }

    /// A lane bound to a registration with no token waits `pending` for a
    /// runner that never comes, so a missing one stops the configuration.
    #[test]
    fn every_macos_registration_reads_a_token_of_its_own() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let write = |name: &str| {
            write_secure(
                &directory.path().join(format!("runner-{name}.token")),
                &format!("glrt-{name}"),
            )
            .expect("write a runner token");
        };
        for name in ["linux", "android", "release", "macos-1"] {
            write(name);
        }

        let error = Tokens::load(directory.path(), 2)
            .err()
            .expect("the second registration has no token");
        assert!(
            format!("{error:#}").contains("runner-macos-2.token"),
            "{error:#}"
        );

        write("macos-2");
        let tokens = Tokens::load(directory.path(), 2).expect("every registration has a token");
        assert_eq!(tokens.macos, ["glrt-macos-1", "glrt-macos-2"]);
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
        let tokens = tokens(&config);

        let rendered: toml::Value =
            toml::from_str(&manager.runner_config(&home, &tokens).unwrap()).unwrap();
        assert_eq!(
            rendered["concurrent"].as_integer(),
            Some(config.host.job_concurrency as i64)
        );
        let runners = rendered["runners"].as_array().unwrap();
        assert_eq!(
            runners.len(),
            3 + config.host.job_concurrency,
            "one registration per runner token"
        );
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
            2 + config.host.job_concurrency,
            "the host registrations share the global job limit"
        );
        let shell_cache_roots: Vec<&str> = runners
            .iter()
            .filter(|runner| runner["executor"].as_str() == Some("shell"))
            .flat_map(|runner| runner["environment"].as_array().into_iter().flatten())
            .filter_map(toml::Value::as_str)
            .filter(|entry| entry.starts_with("KITHARA_CI_CACHE_ROOT="))
            .collect();
        assert_eq!(shell_cache_roots.len(), 2 + config.host.job_concurrency);
        assert!(
            shell_cache_roots
                .windows(2)
                .all(|roots| roots[0] == roots[1]),
            "all shell registrations must contend on one host-global slot root"
        );
        let deployed_inputs = [
            (
                "KITHARA_CI_PINS=",
                config.host.host_root.join("services/pins.toml"),
            ),
            (
                "KITHARA_CI_HOST_BINARY=",
                config
                    .host
                    .host_root
                    .join("toolchains/shared-bin/kithara-ci"),
            ),
        ];
        for runner in runners {
            let environment = runner["environment"].as_array().unwrap();
            for (prefix, path) in &deployed_inputs {
                let values: Vec<&str> = environment
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .filter(|entry| entry.starts_with(prefix))
                    .collect();
                if runner["executor"].as_str() == Some("shell") {
                    assert_eq!(
                        values,
                        [format!("{prefix}{}", path.display())],
                        "{} must use deployed host inputs",
                        runner["name"]
                    );
                } else {
                    assert!(
                        values.is_empty(),
                        "the guest cannot read host service paths"
                    );
                }
            }
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
            Some(consts::LINUX_LATEST_IMAGE)
        );
        assert_eq!(linux["docker"]["pull_policy"].as_str(), Some("never"));
        assert_eq!(
            linux["docker"]["allowed_pull_policies"][0].as_str(),
            Some("never")
        );
        assert_eq!(
            linux["docker"]["allowed_images"][0].as_str(),
            Some(consts::LINUX_LATEST_IMAGE)
        );
    }

    fn scoped_config(directory: &Path) -> CiConfig {
        use std::os::unix::fs::PermissionsExt;

        let env_file = directory.join("cache.env");
        fs::write(
            &env_file,
            "SCCACHE_BUCKET=cache\nSCCACHE_ENDPOINT=http://127.0.0.1:19000\nAWS_ACCESS_KEY_ID=key\nAWS_SECRET_ACCESS_KEY=secret\n",
        )
        .expect("write cache environment");
        fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600))
            .expect("restrict cache environment");
        let mut config = fixture();
        config.host.sccache_s3_env_file = Some(env_file);
        config
    }

    /// A job that compiles before it places its own store — the xtask
    /// bootstrap, a host job — reaches kache's remote and a store on the cache
    /// share through the host file its runner names. The container reaches
    /// both through its own paths.
    #[test]
    fn every_runner_names_a_kache_host_file_it_can_reach() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = scoped_config(directory.path());
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let files = manager.kache_host_files().expect("the host files render");
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens(&config))
                .expect("render runner config"),
        )
        .expect("runner config is TOML");

        let shared = config.host.host_root.join("cache").join(consts::KACHE_DIR);
        let inside = config.host.cache_root_linux.join(consts::KACHE_DIR);
        for runner in rendered["runners"].as_array().expect("runners") {
            let docker = runner.get("docker").is_some();
            let (named, store, endpoint) = if docker {
                (
                    inside.join(consts::KACHE_DOCKER_HOST_FILE),
                    Path::new(consts::KACHE_CONTAINER_ROOT)
                        .join("bootstrap")
                        .join(format!("linux-{}", std::env::consts::ARCH))
                        .join(consts::KACHE_DIR),
                    "http://host.docker.internal:19000",
                )
            } else {
                (
                    shared.join(consts::KACHE_HOST_FILE),
                    config
                        .host
                        .host_root
                        .join("cache/bootstrap")
                        .join(format!("macos-{}", std::env::consts::ARCH))
                        .join(consts::KACHE_DIR),
                    "http://127.0.0.1:19000",
                )
            };
            let entry = format!("KACHE_HOST_CONFIG={}", named.display());
            assert!(
                runner["environment"]
                    .as_array()
                    .expect("runner environment")
                    .iter()
                    .any(|value| value.as_str() == Some(entry.as_str())),
                "{} does not name {entry}: {runner}",
                runner["name"]
            );
            let written = shared.join(named.file_name().expect("a file name"));
            let (_, contents) = files
                .iter()
                .find(|(path, _)| *path == written)
                .unwrap_or_else(|| panic!("{} is not written", written.display()));
            let file: toml::Table = toml::from_str(contents).expect("the host file is TOML");
            assert_eq!(
                file["cache"]["local_store"].as_str().map(PathBuf::from),
                Some(store)
            );
            assert_eq!(
                file["cache"]["local_max_size"].as_str(),
                Some(config.host.sccache_size.as_str())
            );
            assert_eq!(file["cache"]["remote"]["endpoint"].as_str(), Some(endpoint));
            assert_eq!(file["cache"]["remote"]["bucket"].as_str(), Some("cache"));
        }
    }

    #[test]
    fn bootstrap_compiler_stores_belong_to_their_execution_kernel() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = scoped_config(directory.path());
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        for (path, contents) in manager.kache_host_files().expect("host files") {
            let config: toml::Value = toml::from_str(&contents).expect("host configuration");
            let store = Path::new(
                config["cache"]["local_store"]
                    .as_str()
                    .expect("local store"),
            );
            if path.file_name() == Some(OsStr::new(consts::KACHE_DOCKER_HOST_FILE)) {
                assert!(
                    !store.starts_with(&manager.config.host.cache_root_linux),
                    "the container's SQLite index must not live on the host's virtiofs share"
                );
            } else {
                assert!(
                    store
                        .components()
                        .any(|part| part.as_os_str().to_string_lossy().starts_with("macos-")),
                    "a native bootstrap must not open the guest's SQLite index"
                );
            }
        }
    }

    #[test]
    fn every_runner_inherits_the_configured_s3_cache_environment() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let env_file = directory.path().join("cache.env");
        fs::write(
            &env_file,
            "SCCACHE_BUCKET=cache\nSCCACHE_S3_KEY_PREFIX=sccache\nSCCACHE_ENDPOINT=http://127.0.0.1:19000\nSCCACHE_REGION=us-east-1\nSCCACHE_S3_USE_SSL=false\nAWS_ACCESS_KEY_ID=key\nAWS_SECRET_ACCESS_KEY=secret\nAWS_EC2_METADATA_DISABLED=true\n",
        )
        .expect("write cache environment");
        fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600))
            .expect("restrict cache environment");

        let mut config = fixture();
        config.host.sccache_s3_env_file = Some(env_file);
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let tokens = tokens(&config);
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens)
                .expect("render runner config"),
        )
        .expect("runner config is TOML");

        for (index, runner) in rendered["runners"]
            .as_array()
            .expect("runners")
            .iter()
            .enumerate()
        {
            assert!(
                runner["environment"]
                    .as_array()
                    .expect("runner environment")
                    .iter()
                    .any(|value| value.as_str() == Some("SCCACHE_BUCKET=cache")),
                "{} does not inherit the S3 cache: {runner}",
                runner["name"]
            );
            let endpoint = if index == 0 {
                "SCCACHE_ENDPOINT=http://host.docker.internal:19000"
            } else {
                "SCCACHE_ENDPOINT=http://127.0.0.1:19000"
            };
            assert!(
                runner["environment"]
                    .as_array()
                    .expect("runner environment")
                    .iter()
                    .any(|value| value.as_str() == Some(endpoint)),
                "{} cannot reach the S3 endpoint: {runner}",
                runner["name"]
            );
        }
    }

    /// A registration names what is the same for every checkout it holds:
    /// one Cargo home per side of the VM boundary, kept on a cache cleanup
    /// leaves alone. Each host runner also names a compiler-cache socket of
    /// its own for `production/main`'s jobs, which name none, while the host
    /// serves main: a job of this layout names one per checkout, and a job's
    /// variable outranks the runner's.
    #[test]
    fn every_runner_names_one_cargo_home_and_a_compiler_cache_socket_of_its_own() {
        let config = fixture();
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let tokens = tokens(&config);
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens)
                .expect("render runner config"),
        )
        .expect("runner config is TOML");

        let mut shell_homes = BTreeMap::new();
        let mut shell_sockets = BTreeMap::new();
        for runner in rendered["runners"].as_array().expect("runners") {
            let name = runner["name"].as_str().expect("a runner has a name");
            let environment: Vec<&str> = runner["environment"]
                .as_array()
                .expect("runner environment")
                .iter()
                .filter_map(toml::Value::as_str)
                .collect();
            let home = environment
                .iter()
                .find_map(|entry| entry.strip_prefix("CARGO_HOME="))
                .unwrap_or_else(|| panic!("{name} names no Cargo home"));
            if runner.get("docker").is_none() {
                shell_homes.insert(name, home);
                let socket = environment
                    .iter()
                    .find_map(|entry| entry.strip_prefix("SCCACHE_SERVER_UDS="))
                    .unwrap_or_else(|| panic!("{name} names no compiler-cache socket"));
                shell_sockets.insert(socket, name);
                continue;
            }
            let namespace = Path::new(home)
                .strip_prefix(&config.host.cache_root_linux)
                .unwrap_or_else(|_| panic!("{name} keeps its Cargo home off the cache share"))
                .components()
                .next()
                .expect("the home is a namespace of the share");
            assert!(
                config
                    .host
                    .cache_namespaces
                    .iter()
                    .any(|kept| OsStr::new(kept) == namespace.as_os_str()),
                "{name} keeps its Cargo home in {namespace:?}, which cleanup takes whole"
            );
        }
        let homes: Vec<&&str> = shell_homes.values().collect();
        assert_eq!(
            homes.len(),
            2 + config.host.job_concurrency,
            "every host runner names its home"
        );
        assert_eq!(
            shell_sockets.len(),
            homes.len(),
            "two host runners share a compiler-cache socket: {shell_sockets:?}"
        );
        assert!(
            homes.windows(2).all(|pair| pair[0] == pair[1]),
            "the host runners fetch into one home: {shell_homes:?}"
        );
        assert!(
            Path::new(homes[0]).starts_with(config.host.build_root()),
            "the host runners' home is not on the build volume: {shell_homes:?}"
        );
    }

    /// A lock taken inside the virtual machine is invisible on the host, and
    /// the reverse, so a reclaim on either side may walk only build
    /// directories whose leases it can see. The container's checkouts, and the
    /// build roots beside them, sit in a host tree of their own: no host runner
    /// builds in it, and the virtual machine sees no host runner's tree.
    #[test]
    fn the_container_builds_in_a_tree_no_host_runner_shares() {
        let config = fixture();
        let process = Process::new(Path::new("/"), BTreeMap::new());
        let manager = RunnerManager::new(&config, &process);
        let tokens = tokens(&config);
        let rendered: toml::Value = toml::from_str(
            &manager
                .runner_config(&manager.ci_home(), &tokens)
                .expect("render runner config"),
        )
        .expect("runner config is TOML");
        let runners = rendered["runners"].as_array().expect("runners");
        let builds = |runner: &toml::Value| -> PathBuf {
            PathBuf::from(runner["builds_dir"].as_str().expect("a builds directory"))
        };
        let host_trees: Vec<PathBuf> = runners
            .iter()
            .filter(|runner| runner.get("docker").is_none())
            .map(builds)
            .collect();
        assert!(!host_trees.is_empty(), "no runner builds on the host");
        let shares = |path: &Path| {
            host_trees
                .iter()
                .any(|tree| path.starts_with(tree) || tree.starts_with(path))
        };

        let args = manager.colima_args("colima");
        for mount in args
            .windows(2)
            .filter(|pair| pair[0] == "--mount")
            .map(|pair| Path::new(pair[1].trim_end_matches(":w")))
        {
            assert!(
                !shares(mount),
                "the virtual machine mounts {}, where a host runner builds",
                mount.display()
            );
        }
        let mut containers = 0;
        for runner in runners
            .iter()
            .filter(|runner| runner.get("docker").is_some())
        {
            let name = runner["name"].as_str().expect("a runner has a name");
            let destination = format!(":{}:rw", builds(runner).display());
            let source = runner["docker"]["volumes"]
                .as_array()
                .expect("docker volumes")
                .iter()
                .filter_map(toml::Value::as_str)
                .find_map(|volume| volume.strip_suffix(&destination))
                .unwrap_or_else(|| panic!("{name} builds in a directory the host never sees"));
            assert!(
                !shares(Path::new(source)),
                "{name} builds in {source}, where a host runner builds"
            );
            containers += 1;
        }
        assert!(containers > 0, "no runner builds in a container");
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
        let tokens = tokens(&config);
        let rendered: toml::Value = toml::from_str(&manager.runner_config(&home, &tokens).unwrap())
            .expect("runner config is TOML");
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
        let tokens = tokens(&config);

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

        let rendered = manager.runner_config(&home, &tokens).unwrap();
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
        let tokens = tokens(&config);

        let runner = manager.runner_config(&home, &tokens).unwrap();
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
