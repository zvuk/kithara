use std::path::{Path, PathBuf};

use anyhow::Result;

use super::profile::{LinuxHost, LinuxRunner, RunnerFlavor};
use crate::{
    ci::{config::CiPins, image::floating_tag},
    consts,
};

/// What one runner's container is, independent of who starts it.
///
/// systemd starts these through a `docker run` line and Compose through a
/// service block. Both are renderings of this: a second description would drift
/// from the first, and the drift would be a runner that quietly ran with the
/// wrong cores, the wrong image, or no device.
pub(super) struct Container<'a> {
    pub(super) name: String,
    /// The floating tag, not the pin: the pin says what to build, and a
    /// container that named it would die the moment the pin moved ahead of
    /// what this machine has built.
    pub(super) image: String,
    pub(super) network: &'a str,
    /// Which cores its jobs may use, as a set rather than a share. See
    /// [`super::services::cpuset`].
    pub(super) cpuset: String,
    pub(super) memory: &'a str,
    /// The slice that caps the fleet as a whole. A runner's own `memory` never
    /// fired: the host livelocked on the sum of ceilings several times its
    /// memory, so every container draws on one budget instead.
    pub(super) cgroup_parent: &'static str,
    pub(super) devices: &'a [PathBuf],
    pub(super) groups: &'a [u32],
    /// Where the just-in-time registration is left for it. Minted per start and
    /// accepted once, so it is written before the container comes up and is
    /// gone when it stops.
    pub(super) env_file: String,
    /// Volumes this runner mounts, in the order the job sees them.
    pub(super) mounts: Vec<(String, &'static str)>,
}

impl Container<'_> {
    /// The volumes this runner mounts.
    ///
    /// The build root is the runner's own: one job runs per container, and the
    /// lane runner re-points the alias inside it at the lane's build, which
    /// two jobs sharing one root would race. The lanes' builds are not: every
    /// runner of a trust takes its lane's slots in one directory, so a lane
    /// built on one runner is warm for the next that runs it. Cargo's home is shared by the
    /// runners of one trust, so a crate is downloaded once per trust, while a
    /// review job never writes what a trusted job reads. It is mounted whole
    /// rather than as its registry and its git checkouts separately: cargo
    /// guards both with a lock file kept beside them, and mounting the data
    /// without the lock leaves two jobs unpacking one crate into one directory.
    /// Host paths keep these write-heavy caches on the disk the machine profile
    /// selects instead of wherever Docker stores named volumes.
    pub(super) fn mounts(host: &LinuxHost, runner: &LinuxRunner) -> Vec<(String, &'static str)> {
        vec![
            (
                Self::cargo_home(host, runner)
                    .to_string_lossy()
                    .into_owned(),
                consts::CARGO_HOME_MOUNT,
            ),
            (
                host.cache_root
                    .join("workspaces")
                    .join(&runner.name)
                    .to_string_lossy()
                    .into_owned(),
                "/runner/_work",
            ),
            (
                Self::build_root(host, runner)
                    .to_string_lossy()
                    .into_owned(),
                consts::BUILD_ROOT_MOUNT,
            ),
            (
                Self::build_slots(host, runner)
                    .to_string_lossy()
                    .into_owned(),
                consts::BUILD_SLOTS_MOUNT,
            ),
            (
                Self::kache(host, runner).to_string_lossy().into_owned(),
                consts::KACHE_MOUNT,
            ),
            ("kithara-ci-fixtures".to_owned(), "/cache/fixtures"),
            // Every runner shares the one directory `production/main` claims
            // lane directories in, as main mounts it.
            (
                host.cache_root
                    .join(consts::PREVIOUS_LANES)
                    .to_string_lossy()
                    .into_owned(),
                consts::PREVIOUS_LANES_MOUNT,
            ),
        ]
    }

    /// Where a runner's jobs build: every lane's directory, the alias pointing
    /// at the current one, and the xtask bootstrap.
    pub(super) fn build_root(host: &LinuxHost, runner: &LinuxRunner) -> PathBuf {
        host.cache_root.join("target").join(&runner.name)
    }

    /// Where the runners of one trust take their lanes' build slots.
    pub(super) fn build_slots(host: &LinuxHost, runner: &LinuxRunner) -> PathBuf {
        host.cache_root
            .join(consts::BUILD_SLOTS_DIR)
            .join(runner.cache_trust.as_str())
    }

    /// Where the runners of one trust keep kache's host file and store.
    pub(super) fn kache(host: &LinuxHost, runner: &LinuxRunner) -> PathBuf {
        host.cache_root
            .join(consts::KACHE_DIR)
            .join(runner.cache_trust.as_str())
    }

    fn cargo_home(host: &LinuxHost, runner: &LinuxRunner) -> PathBuf {
        host.cache_root
            .join("cargo")
            .join(runner.cache_trust.as_str())
    }

    pub(super) fn mount_type(source: &str) -> &'static str {
        if Path::new(source).is_absolute() {
            "bind"
        } else {
            "volume"
        }
    }

    /// What the job is told about where to build and what to reuse.
    ///
    /// Cargo is told the build root, the same path in every container; a lane
    /// builds behind the alias in it, so every lane's compilations are keyed
    /// alike fleet-wide and the compiler cache serves one runner's entries to
    /// another. The lane is told where its trust's slots are. The store itself is the one the runner's credentials name;
    /// nothing of it is kept on this disk. `production/main` is told where it
    /// claims lane directories too, until this layout replaces its own.
    ///
    /// The linker entries come from [`LINUX_LINKER_ENV`](consts::LINUX_LINKER_ENV), which the GitLab lane
    /// executor reads too: one statement of what a Linux job links with rather
    /// than one per way of starting a job.
    pub(super) fn environment(runner: &LinuxRunner) -> Vec<String> {
        let root = Path::new(consts::BUILD_ROOT_MOUNT);
        let mut environment: Vec<String> = consts::CACHE_ENVIRONMENT
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect();
        environment.extend([
            // Containers own CPU admission; their kache store is shared.
            "KACHE_SCHEDULER=0".to_owned(),
            format!("{}={}", consts::KACHE_LOCAL_ROOT_ENV, consts::KACHE_MOUNT),
            format!("CARGO_HOME={}", consts::CARGO_HOME_MOUNT),
            format!("CARGO_TARGET_DIR={}", root.display()),
            format!("{}={}", consts::BUILD_SLOTS_ENV, consts::BUILD_SLOTS_MOUNT),
            format!("KITHARA_CI_TARGET_ROOT={}", consts::PREVIOUS_LANES_MOUNT),
            format!(
                "KITHARA_XTASK_TARGET={}",
                root.join(consts::XTASK_BUILD).display()
            ),
            format!(
                "KACHE_HOST_CONFIG={}",
                Path::new(consts::KACHE_MOUNT)
                    .join(consts::KACHE_HOST_FILE)
                    .display()
            ),
            format!(
                "SCCACHE_IDLE_TIMEOUT={SCCACHE_IDLE_TIMEOUT}",
                SCCACHE_IDLE_TIMEOUT = consts::SCCACHE_IDLE_TIMEOUT
            ),
            // Each runner needs its own daemon endpoint. An explicit socket
            // lets the lane start that daemon before Cargo's parallel
            // compilers can race to start it.
            format!("SCCACHE_SERVER_UDS=/tmp/{}.sock", runner.name),
        ]);
        environment.extend(
            consts::LINUX_LINKER_ENV
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
        environment
    }

    pub(super) const PIDS_LIMIT: u32 = 8192;

    /// `RLIMIT_NICE` for every process of a job: 20 minus the lowest nice
    /// value a thread may set for itself, so 40 allows -20, as Android's init
    /// grants its processes. The player runs the thread that feeds a deck's
    /// output ring at nice -16. At 0 the kernel refuses that, and on a machine
    /// full of compile jobs the thread waits for a CPU longer than the ring
    /// lasts.
    pub(super) const NICE_LIMIT: u8 = 40;
}

/// The image a runner's unit starts from: the floating tag of its flavour's
/// pin, which is the tag the machine holds.
pub(super) fn image(runner: &LinuxRunner, pins: &CiPins) -> Result<String> {
    floating_tag(match runner.flavor {
        RunnerFlavor::Plain => &pins.linux_runner_image,
        RunnerFlavor::Android => &pins.linux_android_runner_image,
    })
}

pub(super) fn container<'a>(
    host: &'a LinuxHost,
    runner: &'a LinuxRunner,
    cpuset: String,
    pins: &'a CiPins,
) -> Result<Container<'a>> {
    Ok(Container {
        name: format!("kithara-ci-{}", runner.name),
        image: image(runner, pins)?,
        network: &host.network,
        cpuset,
        memory: &runner.memory,
        cgroup_parent: consts::SERVICE_SLICE,
        devices: &runner.devices,
        groups: &runner.groups,
        env_file: super::services::env_file(runner),
        mounts: Container::mounts(host, runner),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{ffi::OsStr, fs};

    use serde_yaml_ng::Value;

    use super::*;
    use crate::ci::{
        config::workspace_root, environment::CacheTrust, host::linux::profile::tests::host_fixture,
        previous_layout::previous_layout,
    };

    /// The value a runner's job is told for `name`.
    pub(crate) fn told(runner: &LinuxRunner, name: &str) -> PathBuf {
        let prefix = format!("{name}=");
        Container::environment(runner)
            .iter()
            .find_map(|entry| entry.strip_prefix(&prefix).map(PathBuf::from))
            .unwrap_or_else(|| panic!("{} is not told {name}", runner.name))
    }

    /// The host directory or volume a runner mounts at `destination`.
    pub(crate) fn mounted_at(host: &LinuxHost, runner: &LinuxRunner, destination: &Path) -> String {
        Container::mounts(host, runner)
            .into_iter()
            .find(|(_, at)| Path::new(at) == destination)
            .unwrap_or_else(|| {
                panic!(
                    "{} mounts nothing at {}",
                    runner.name,
                    destination.display()
                )
            })
            .0
    }

    /// Every container tells one build root, so every compilation of every
    /// lane is keyed the same fleet-wide. Behind it is the runner's own: the
    /// lane runner re-points the alias inside it at the lane's build, and one
    /// job per container means re-pointing it never races another job.
    #[test]
    fn a_job_is_told_its_runners_own_build_root() {
        let host = host_fixture();
        let [first, second, ..] = host.runners.as_slice() else {
            panic!("the host fixture serves more than one runner");
        };
        let root = told(first, "CARGO_TARGET_DIR");

        assert_eq!(root, told(second, "CARGO_TARGET_DIR"));
        assert_ne!(
            mounted_at(&host, first, &root),
            mounted_at(&host, second, &root),
            "two jobs re-point one alias"
        );
    }

    /// A job of the previous layout builds and caches where its workflows and
    /// its container name. A path no mount holds is the container's own
    /// throwaway disk, and creating a directory under `/cache` there fails.
    #[test]
    fn a_job_finds_every_path_the_previous_layout_names_on_a_mount() {
        let host = host_fixture();
        let layout = previous_layout().linux;
        for runner in &host.runners {
            let mounts = Container::mounts(&host, runner);
            let named = std::iter::once(&layout.named.cache_root).chain(&layout.named.paths);
            for path in named.chain(layout.told.values()) {
                assert!(
                    mounts.iter().any(|(_, at)| path.starts_with(at)),
                    "{} mounts nothing holding {}",
                    runner.name,
                    path.display()
                );
            }
        }
    }

    /// A job of the previous layout is told what it was told: its scripts
    /// derive where to build and lease from these, and the compiler cache keys
    /// every compilation by Cargo's variables.
    #[test]
    fn a_job_is_told_what_the_previous_layout_told_it() {
        let host = host_fixture();
        for runner in &host.runners {
            for (name, value) in previous_layout().linux.told {
                assert_eq!(told(runner, &name), value, "{} tells {name}", runner.name);
            }
        }
    }

    /// The xtask bootstrap is one more build in the runner's build root, under
    /// the one name no lane may take, so the budget and the evictor see it like
    /// any lane's.
    #[test]
    fn a_job_bootstraps_xtask_beside_its_lanes() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");
        let root = told(runner, "CARGO_TARGET_DIR");
        let xtask = told(runner, "KITHARA_XTASK_TARGET");

        assert_eq!(xtask.parent(), Some(root.as_path()));
        assert_eq!(xtask.file_name(), Some(OsStr::new(consts::XTASK_BUILD)));
    }

    /// Cargo's home holds what jobs download and what they unpack it into, and
    /// a review job must never write what a trusted job reads: two runners
    /// share a home exactly when they share a trust.
    #[test]
    fn runners_share_a_cargo_home_exactly_when_they_share_a_trust() {
        let mut host = host_fixture();
        host.runners[0].cache_trust = CacheTrust::Trusted;
        let home = |runner: &LinuxRunner| mounted_at(&host, runner, &told(runner, "CARGO_HOME"));

        for first in &host.runners {
            for second in &host.runners {
                assert_eq!(
                    home(first) == home(second),
                    first.cache_trust == second.cache_trust,
                    "{} and {}",
                    first.name,
                    second.name
                );
            }
        }
    }

    /// A lane's build is reused by whichever runner of its trust runs the lane
    /// next, so runners keep their lanes' slots where every runner of a trust
    /// sees them, and a review job never builds in a slot a trusted job reads.
    #[test]
    fn runners_share_lane_slots_exactly_when_they_share_a_trust() {
        let mut host = host_fixture();
        host.runners[0].cache_trust = CacheTrust::Trusted;
        let slots = |runner: &LinuxRunner| {
            mounted_at(&host, runner, &told(runner, consts::BUILD_SLOTS_ENV))
        };

        for first in &host.runners {
            for second in &host.runners {
                assert_eq!(
                    slots(first) == slots(second),
                    first.cache_trust == second.cache_trust,
                    "{} and {}",
                    first.name,
                    second.name
                );
            }
        }
    }

    /// Every compile in a container reaches kache's store and remote through
    /// the host file the runner writes, whatever command started it: a stress
    /// run or the xtask bootstrap compiled cold into a store thrown away with
    /// the container. The store is one per trust, as the Cargo home is.
    #[test]
    fn every_compile_reaches_its_trusts_kache() {
        let mut host = host_fixture();
        host.runners[0].cache_trust = CacheTrust::Trusted;
        let store =
            |runner: &LinuxRunner| mounted_at(&host, runner, Path::new(consts::KACHE_MOUNT));

        for runner in &host.runners {
            assert_eq!(
                told(runner, "KACHE_HOST_CONFIG"),
                Path::new(consts::KACHE_MOUNT).join(consts::KACHE_HOST_FILE),
                "{}",
                runner.name
            );
            assert_eq!(
                store(runner),
                host.cache_root
                    .join(consts::KACHE_DIR)
                    .join(runner.cache_trust.as_str())
                    .to_string_lossy()
            );
        }
    }

    #[test]
    fn containers_own_compiler_cpu_admission() {
        for runner in &host_fixture().runners {
            assert!(
                Container::environment(runner)
                    .iter()
                    .any(|entry| entry == "KACHE_SCHEDULER=0"),
                "{} shares kache's CPU scheduler with other containers",
                runner.name
            );
        }
    }

    /// The compiler cache is the store the runner's credentials name. A local
    /// directory beside it was never read and only held a disk.
    #[test]
    fn a_job_keeps_no_compiler_cache_on_disk() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");

        assert!(
            !Container::environment(runner)
                .iter()
                .any(|entry| entry.starts_with("SCCACHE_DIR=")),
            "{:?}",
            Container::environment(runner)
        );
    }

    /// Where a job's caches live is the container's to say. A workflow that
    /// names one of those paths again overrides the container for every job
    /// it runs: the role runner named the build root itself, so its xtask
    /// built into the root, beside the lanes rather than as one of them.
    #[test]
    fn no_workflow_restates_where_the_container_keeps_a_cache() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");
        let mounts = Container::mounts(&host, runner);
        let inside = |value: &str| {
            mounts
                .iter()
                .any(|(_, at)| Path::new(value).starts_with(at))
        };
        let environment = Container::environment(runner);
        let said: Vec<&str> = environment
            .iter()
            .filter_map(|entry| entry.split_once('='))
            .filter(|(_, value)| inside(value))
            .map(|(name, _)| name)
            .collect();
        let workflows = workspace_root().join(".github/workflows");

        for entry in fs::read_dir(&workflows).expect("the workflows are readable") {
            let path = entry.expect("a workflow entry").path();
            let text = fs::read_to_string(&path).expect("a workflow is readable");
            let workflow: Value = serde_yaml_ng::from_str(&text).expect("a workflow is YAML");
            for (name, value) in environments(&workflow) {
                assert!(
                    !(said.contains(&name) && inside(value)),
                    "{} names {name}={value}, which the container says",
                    path.display()
                );
            }
        }
    }

    /// Every variable an `env` block anywhere in `workflow` sets.
    fn environments(workflow: &Value) -> Vec<(&str, &str)> {
        let mut found = Vec::new();
        let mut pending = vec![workflow];
        while let Some(value) = pending.pop() {
            match value {
                Value::Mapping(mapping) => {
                    for (key, value) in mapping {
                        if key.as_str() == Some("env")
                            && let Some(block) = value.as_mapping()
                        {
                            found.extend(
                                block
                                    .iter()
                                    .filter_map(|(name, value)| name.as_str().zip(value.as_str())),
                            );
                        }
                        pending.push(value);
                    }
                }
                Value::Sequence(values) => pending.extend(values),
                _ => {}
            }
        }
        found
    }

    /// The linker a Linux job links with is part of what a job is told, not a
    /// property of whichever image happened to be built: an unnamed linker is
    /// `bfd`, and `bfd` is where a test job spends more time than it spends
    /// testing.
    #[test]
    fn a_job_is_told_which_linker_to_use() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");
        let environment = Container::environment(runner);

        for (name, value) in consts::LINUX_LINKER_ENV {
            assert!(
                environment.contains(&format!("{name}={value}")),
                "{name} is missing from {environment:?}"
            );
        }
    }

    /// A lane's build directory moves between runners, and it records the
    /// downloaded beat models by path and modification time. Models kept where
    /// only one runner sees them are fetched again by the next, newer than the
    /// build, and everything that embeds them is rebuilt.
    #[test]
    fn a_job_keeps_the_beat_models_on_a_mount_every_runner_shares() {
        let host = host_fixture();
        let [first, second, ..] = host.runners.as_slice() else {
            panic!("the host fixture serves more than one runner");
        };
        let environment = Container::environment(first);
        let models = environment
            .iter()
            .find_map(|entry| entry.strip_prefix("KITHARA_BEAT_MODEL_CACHE="))
            .expect("a job is told where the beat models live");

        let shared = Container::mounts(&host, first)
            .into_iter()
            .zip(Container::mounts(&host, second))
            .filter(|(mine, theirs)| mine == theirs)
            .map(|((_, destination), _)| destination);
        assert!(
            shared
                .into_iter()
                .any(|destination| Path::new(models).starts_with(destination)),
            "{models} is not on a mount every runner shares"
        );
    }

    #[test]
    fn a_runner_keeps_its_ready_cache_daemon_available_for_its_job() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");

        assert!(Container::environment(runner).contains(&format!(
            "SCCACHE_IDLE_TIMEOUT={SCCACHE_IDLE_TIMEOUT}",
            SCCACHE_IDLE_TIMEOUT = consts::SCCACHE_IDLE_TIMEOUT
        )));
    }
}
