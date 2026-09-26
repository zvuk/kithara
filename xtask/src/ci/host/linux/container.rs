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
    /// The cargo home is mounted whole rather than as its registry and its git
    /// checkouts separately: cargo guards both with a lock file kept beside
    /// them, and jobs on this machine run at the same time. Mounting the data
    /// without the lock leaves two of them unpacking one crate into one
    /// directory.
    /// The cache mounts every runner shares, and the build directory it keeps
    /// to itself under the host's configured cache root.
    ///
    /// The registry of downloaded crates is shared because that is what it is
    /// for, and the compiler cache because `sccache` keys on the inputs of a
    /// compilation, so one runner's entry is another's hit.
    ///
    /// The build root is shared, and a lane claims the directory named after it
    /// underneath. Build artefacts are valid only for the exact features,
    /// profile and toolchain that produced them, which is why one directory for
    /// every job reuses nothing — but a lane asks for the same shape on every
    /// run, so the directory it claims is warm whichever runner picked the job
    /// up. A runner-owned directory instead decided reuse by which runner
    /// happened to be free, and a lane that moved compiled the workspace again.
    /// A host path keeps that write-heavy cache on the disk selected by the
    /// machine profile instead of wherever Docker stores named volumes.
    pub(super) fn mounts(host: &LinuxHost, runner: &LinuxRunner) -> Vec<(String, &'static str)> {
        vec![
            ("kithara-ci-cargo-home".to_owned(), "/home/runner/.cargo"),
            (
                host.cache_root
                    .join("workspaces")
                    .join(&runner.name)
                    .to_string_lossy()
                    .into_owned(),
                "/runner/_work",
            ),
            (
                Self::target_dir(host, runner)
                    .to_string_lossy()
                    .into_owned(),
                "/cache/target",
            ),
            (
                Self::lane_root(host).to_string_lossy().into_owned(),
                "/cache/lanes",
            ),
            ("kithara-ci-sccache".to_owned(), "/cache/sccache"),
            ("kithara-ci-fixtures".to_owned(), "/cache/fixtures"),
        ]
    }

    /// Where a job that claims no lane directory builds. One per runner, which
    /// is what such a job reused before the lane keying existed.
    pub(super) fn target_dir(host: &LinuxHost, runner: &LinuxRunner) -> PathBuf {
        host.cache_root.join("target").join(&runner.name)
    }

    /// The one build root every runner mounts. A lane owns one directory under
    /// it; the budget is enforced over the root.
    pub(super) fn lane_root(host: &LinuxHost) -> PathBuf {
        host.cache_root.join("lanes")
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
    /// `sccache` is in the image and was reaching nothing: without
    /// `RUSTC_WRAPPER` every job compiled the workspace from source, and the
    /// only thing the runners shared was the registry of downloaded crates and
    /// one build directory that had grown past two hundred gigabytes. A build
    /// directory is the wrong thing to share — its artefacts are valid only for
    /// the exact features, profile and toolchain that produced them, so
    /// twenty-four jobs of different shapes pile up beside each other and reuse
    /// nothing.
    /// `sccache` keys on the inputs of a compilation instead, which is what
    /// makes sharing it across runners sound rather than merely concurrent.
    ///
    /// The linker entries come from [`LINUX_LINKER_ENV`](consts::LINUX_LINKER_ENV), which the GitLab lane
    /// executor reads too: one statement of what a Linux job links with rather
    /// than one per way of starting a job.
    pub(super) fn environment(runner: &LinuxRunner) -> Vec<String> {
        let mut environment: Vec<String> = consts::CACHE_ENVIRONMENT
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect();
        environment.push(format!(
            "SCCACHE_IDLE_TIMEOUT={SCCACHE_IDLE_TIMEOUT}",
            SCCACHE_IDLE_TIMEOUT = consts::SCCACHE_IDLE_TIMEOUT
        ));
        // The S3 backend is shared, but each runner needs its own daemon
        // endpoint. An explicit socket lets the lane start that daemon before
        // Cargo's parallel compilers can race to start it.
        environment.push(format!("SCCACHE_DIR=/cache/sccache/{}", runner.name));
        environment.push(format!("SCCACHE_SERVER_UDS=/tmp/{}.sock", runner.name));
        environment.extend(
            consts::LINUX_LINKER_ENV
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
        environment
    }

    pub(super) const PIDS_LIMIT: u32 = 8192;
}

pub(super) fn container<'a>(
    host: &'a LinuxHost,
    runner: &'a LinuxRunner,
    cpuset: String,
    pins: &'a CiPins,
) -> Result<Container<'a>> {
    Ok(Container {
        name: format!("kithara-ci-{}", runner.name),
        image: floating_tag(match runner.flavor {
            RunnerFlavor::Plain => &pins.linux_runner_image,
            RunnerFlavor::Android => &pins.linux_android_runner_image,
        })?,
        network: &host.network,
        cpuset,
        memory: &runner.memory,
        devices: &runner.devices,
        groups: &runner.groups,
        env_file: super::services::env_file(runner),
        mounts: Container::mounts(host, runner),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The linker a Linux job links with is part of what a job is told, not a
    /// property of whichever image happened to be built: an unnamed linker is
    /// `bfd`, and `bfd` is where a test job spends more time than it spends
    /// testing.
    #[test]
    fn a_job_is_told_which_linker_to_use() {
        let host = super::super::profile::tests::host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");
        let environment = Container::environment(runner);

        for (name, value) in consts::LINUX_LINKER_ENV {
            assert!(
                environment.contains(&format!("{name}={value}")),
                "{name} is missing from {environment:?}"
            );
        }
    }

    #[test]
    fn a_runner_keeps_its_ready_cache_daemon_available_for_its_job() {
        let host = super::super::profile::tests::host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");

        assert!(Container::environment(runner).contains(&format!(
            "SCCACHE_IDLE_TIMEOUT={SCCACHE_IDLE_TIMEOUT}",
            SCCACHE_IDLE_TIMEOUT = consts::SCCACHE_IDLE_TIMEOUT
        )));
    }
}
