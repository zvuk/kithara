use std::{
    collections::BTreeSet,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ci::{
    config::{default_build_cache_size, parse_build_cache_size},
    environment::CacheTrust,
};

/// Machine profile of one Linux CI host: the runners it serves and what each of
/// them may consume. It is provisioned per machine and never tracked in the
/// repository; the reviewed build contract lives in
/// [`crate::ci::config::CiPins`].
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinuxHost {
    /// Positive decimal gigabytes, such as `25GB`. Defaulted: installed
    /// profiles predate it, and refusing to load would kill the cleanup.
    #[serde(default = "default_build_cache_size")]
    pub(crate) build_cache_size: String,
    /// Directory holding the caches jobs keep between runs.
    pub(crate) cache_root: PathBuf,
    /// Docker network the runners are confined to.
    pub(crate) network: String,
    /// Address block of that network, fenced off from the rest of the machine.
    pub(crate) subnet: String,
    /// Serialised last: TOML requires tables after plain values.
    ///
    /// The repositories this machine serves, each with the token that mints
    /// registrations for it. A GitHub runner registration is bound to exactly
    /// one repository, so a machine serving two of them holds two credentials
    /// and runs separate runners against each.
    pub(crate) repositories: Vec<RepositoryCredential>,
    pub(crate) runners: Vec<LinuxRunner>,
    /// The Windows guest this machine hosts, if it hosts one.
    #[serde(default)]
    pub(crate) windows: Option<WindowsGuest>,
}

/// One repository this machine serves, and the token that speaks for it.
///
/// Kept apart from the runners so the path to a secret is written once per
/// repository rather than once per runner, and so reading the profile answers
/// "whose work does this machine take" without counting runner entries.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryCredential {
    /// `owner/name` of the repository.
    pub(crate) name: String,
    /// File holding the token that mints registrations for it.
    pub(crate) token_file: PathBuf,
}

/// A Windows virtual machine serving the lane that needs a real Windows.
///
/// Cross-compiling to MSVC answers whether the code builds; only a guest
/// answers whether it runs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowsGuest {
    pub(crate) name: String,
    /// Which repository of [`LinuxHost::repositories`] this guest enrols with.
    pub(crate) repository: String,
    pub(crate) vcpus: u32,
    pub(crate) memory_mib: u32,
    pub(crate) disk_gib: u32,
    /// Labels a workflow selects this guest by.
    pub(crate) labels: Vec<String>,
    /// libvirt network the guest is attached to. This is not the Docker
    /// network the containers use: the two live in different worlds and a
    /// name that exists in one means nothing in the other.
    #[serde(default = "default_libvirt_network")]
    pub(crate) network: String,
}

fn default_libvirt_network() -> String {
    "default".to_owned()
}

/// One runner served by this machine.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinuxRunner {
    /// Identifies the runner's service, registration, and container.
    pub(crate) name: String,
    /// Which repository of [`LinuxHost::repositories`] this runner serves. A
    /// registration reaches exactly one, so the choice belongs to the runner
    /// rather than to the machine underneath it.
    pub(crate) repository: String,
    /// Mode-0600 S3 credentials scoped to this runner's cache namespace.
    pub(crate) sccache_s3_env_file: PathBuf,
    /// Whether these credentials may publish immutable target snapshots.
    pub(crate) cache_trust: CacheTrust,
    /// How many cores one job may use. Handed to the container as a set of
    /// core numbers rather than a share of the machine: a share is a CFS
    /// quota, which throttles a job without telling it anything, so `nproc`
    /// still reports the whole host and Cargo still starts one compilation
    /// per host core. A set is what makes the container's own view true.
    pub(crate) cpus: u32,
    pub(crate) memory: String,
    /// Labels a workflow selects this runner by.
    pub(crate) labels: Vec<String>,
    /// Host devices the job needs, such as `/dev/kvm` for an emulator.
    #[serde(default)]
    pub(crate) devices: Vec<PathBuf>,
    /// Groups the job joins on top of its own. A device node is typically
    /// owned by one, and its number is a property of the machine rather than
    /// of the image, which is why it is named here.
    #[serde(default)]
    pub(crate) groups: Vec<u32>,
    /// Which image this runner starts from.
    #[serde(default)]
    pub(crate) flavor: RunnerFlavor,
}

/// The images a runner can be built on. A plain runner carries the workspace
/// toolchain; an Android one carries an emulator on top of it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) enum RunnerFlavor {
    #[default]
    Plain,
    Android,
}

impl LinuxHost {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading Linux CI profile {}", path.display()))?;
        let host: Self = toml::from_str(&text)
            .with_context(|| format!("parsing Linux CI profile {}", path.display()))?;
        host.validate()?;
        Ok(host)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.build_cache_size.trim().is_empty() {
            bail!("Linux CI profile build_cache_size must not be empty");
        }
        self.build_cache_budget_bytes()?;
        if !self.cache_root.is_absolute() {
            bail!("Linux CI profile cache_root must be an absolute path");
        }
        if !safe_name(&self.network) {
            bail!("Linux CI profile network contains unsupported characters");
        }
        parse_subnet(&self.subnet)?;
        if self.repositories.is_empty() {
            bail!("Linux CI profile must define at least one repository");
        }
        let mut repositories = BTreeSet::new();
        for credential in &self.repositories {
            credential.validate()?;
            if !repositories.insert(credential.name.as_str()) {
                bail!(
                    "Linux CI profile defines repository {} twice",
                    credential.name
                );
            }
        }
        if self.runners.is_empty() {
            bail!("Linux CI profile must define at least one runner");
        }
        if let Some(windows) = &self.windows {
            windows.validate()?;
            self.credential(&windows.repository)?;
        }
        let mut seen = BTreeSet::new();
        for runner in &self.runners {
            runner.validate()?;
            self.credential(&runner.repository)?;
            if !seen.insert(runner.name.as_str()) {
                bail!("Linux CI profile defines runner {} twice", runner.name);
            }
        }
        Ok(())
    }

    pub(crate) fn build_cache_budget_bytes(&self) -> Result<u64> {
        parse_build_cache_size(&self.build_cache_size)
            .context("Linux CI profile build_cache_size is invalid")
    }

    /// The credential a runner or guest registers with, by the repository it
    /// names. Unknown names fail here rather than reaching GitHub as a 404.
    pub(crate) fn credential(&self, repository: &str) -> Result<&RepositoryCredential> {
        self.repositories
            .iter()
            .find(|credential| credential.name == repository)
            .with_context(|| {
                format!("Linux CI profile has no credential for repository {repository}")
            })
    }

    pub(crate) fn runner(&self, name: &str) -> Result<&LinuxRunner> {
        self.runners
            .iter()
            .find(|runner| runner.name == name)
            .with_context(|| format!("Linux CI profile has no runner named {name}"))
    }
}

impl RepositoryCredential {
    fn validate(&self) -> Result<()> {
        if !valid_repository(&self.name) {
            bail!(
                "Linux CI profile repository {} must read owner/name",
                self.name
            );
        }
        if !self.token_file.is_absolute() {
            bail!(
                "Linux CI profile repository {} must name its token file by absolute path",
                self.name
            );
        }
        Ok(())
    }
}

impl WindowsGuest {
    fn validate(&self) -> Result<()> {
        if !safe_name(&self.name) {
            bail!("Linux CI profile's Windows guest name is unusable");
        }
        // Windows 11 refuses to install below these, and a guest that installs
        // but cannot compile is worse than one that never started.
        if self.vcpus < 2 || self.memory_mib < 4096 || self.disk_gib < 64 {
            bail!("the Windows guest needs at least 2 vCPUs, 4 GiB, and 64 GiB");
        }
        if !safe_name(&self.network) {
            bail!("the Windows guest's network name is unusable");
        }
        if self.labels.is_empty() || self.labels.iter().any(|label| !safe_label(label)) {
            bail!("the Windows guest has an unusable label");
        }
        Ok(())
    }
}

impl LinuxRunner {
    fn validate(&self) -> Result<()> {
        if !safe_name(&self.name) {
            bail!("Linux CI runner name contains unsupported characters");
        }
        if self.cpus == 0 || self.memory.trim().is_empty() {
            bail!(
                "Linux CI runner {} must bound its CPU and memory",
                self.name
            );
        }
        if !self.sccache_s3_env_file.is_absolute() {
            bail!(
                "Linux CI runner {} must name its S3 cache environment by absolute path",
                self.name
            );
        }
        if self.labels.is_empty() || self.labels.iter().any(|label| !safe_label(label)) {
            bail!("Linux CI runner {} has an unusable label", self.name);
        }
        if self.devices.iter().any(|device| !device.is_absolute()) {
            bail!("Linux CI runner {} names a relative device", self.name);
        }
        Ok(())
    }

    /// The systemd unit and container both carry the runner's own name.
    pub(crate) fn service(&self) -> String {
        format!("kithara-ci-{}.service", self.name)
    }

    pub(crate) fn labels(&self) -> String {
        self.labels.join(",")
    }
}

/// Docker accepts any CIDR here, so the parse doubles as the check that the
/// firewall rules built from it cannot be widened by a malformed profile.
fn parse_subnet(value: &str) -> Result<(Ipv4Addr, u32)> {
    let (address, prefix) = value
        .split_once('/')
        .with_context(|| format!("Linux CI profile subnet {value} is not a CIDR block"))?;
    let address: Ipv4Addr = address
        .parse()
        .with_context(|| format!("Linux CI profile subnet {value} has no IPv4 address"))?;
    let prefix: u32 = prefix
        .parse()
        .with_context(|| format!("Linux CI profile subnet {value} has no prefix length"))?;
    if !(8..=30).contains(&prefix) {
        bail!("Linux CI profile subnet {value} must have a prefix between 8 and 30");
    }
    Ok((address, prefix))
}

fn valid_repository(value: &str) -> bool {
    value
        .split_once('/')
        .is_some_and(|(owner, name)| safe_name(owner) && safe_name(name))
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn safe_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An installed profile predating the budget must still load: the binary
    /// reaches every host before its profile does, and one that refused would
    /// take the cleanup down on each of them.
    #[test]
    fn a_profile_without_a_build_cache_size_still_loads() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("linux-host.toml");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ci-linux-host.toml");
        let text = fs::read_to_string(&source).expect("fixture profile");
        let without: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("build_cache_size"))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(&path, without).expect("write profile");

        assert_eq!(
            LinuxHost::load(&path)
                .expect("profile loads")
                .build_cache_size,
            default_build_cache_size()
        );
    }

    /// A profile shaped like the machines this command provisions: one plain
    /// runner and one that reaches the hardware a plain one must not.
    pub(crate) fn host_fixture() -> LinuxHost {
        LinuxHost::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ci-linux-host.toml"),
        )
        .expect("the fixture profile must load")
    }

    #[test]
    fn the_fixture_profile_matches_the_machine_contract() {
        let host = host_fixture();
        assert!(host.runner("kithara-ci-octocat").is_ok());
        assert!(host.runner("absent").is_err());
    }

    /// The point of the split: one machine, two repositories, and each runner
    /// reaching the token of the repository it named — neither is the default.
    #[test]
    fn a_runner_registers_with_the_repository_it_names() {
        let host = host_fixture();

        let octocat = host
            .credential(
                &host
                    .runner("kithara-ci-octocat")
                    .expect("runner")
                    .repository,
            )
            .expect("credential");
        assert_eq!(octocat.name, "octocat/kithara");
        assert_eq!(
            octocat.token_file,
            Path::new("/etc/kithara-ci/tokens/octocat.token")
        );

        let hubot = host
            .credential(&host.runner("kithara-ci-hubot").expect("runner").repository)
            .expect("credential");
        assert_eq!(hubot.name, "hubot/kithara");
        assert_eq!(
            hubot.token_file,
            Path::new("/etc/kithara-ci/tokens/hubot.token")
        );
    }

    /// A runner naming a repository the machine holds no token for must fail
    /// while the profile is read. Left to run, it would mint a registration
    /// against whatever credential happened to be at hand.
    #[test]
    fn a_runner_naming_an_unknown_repository_is_rejected() {
        let mut host = host_fixture();
        "someone/else".clone_into(&mut host.runners[0].repository);
        let error = host
            .validate()
            .expect_err("an unknown repository must fail");
        assert!(
            error.to_string().contains("someone/else"),
            "the error must name the repository that has no credential: {error}"
        );
    }

    #[test]
    fn one_repository_declared_twice_is_rejected() {
        let mut host = host_fixture();
        let duplicate = host.repositories[0].clone();
        host.repositories.push(duplicate);
        let error = host.validate().expect_err("a duplicate must fail");
        assert!(
            error.to_string().contains("twice"),
            "the error must say the repository is declared twice: {error}"
        );
    }

    #[test]
    fn a_repository_must_name_its_token_by_absolute_path() {
        let mut host = host_fixture();
        host.repositories[0].token_file = PathBuf::from("github.token");
        let error = host
            .validate()
            .expect_err("a relative token file must fail");
        assert!(
            error.to_string().contains("absolute"),
            "the error must say the path has to be absolute: {error}"
        );
    }

    #[test]
    fn subnets_are_bounded() {
        assert!(parse_subnet("172.16.240.0/24").is_ok());
        assert!(parse_subnet("172.16.240.0").is_err());
        assert!(parse_subnet("172.16.240.0/4").is_err());
        assert!(parse_subnet("not-an-address/24").is_err());
    }

    #[test]
    fn repositories_name_an_owner() {
        assert!(valid_repository("octocat/kithara"));
        assert!(!valid_repository("kithara"));
        assert!(!valid_repository("owner/kithara/extra"));
        assert!(!valid_repository("owner/../etc"));
    }
}
