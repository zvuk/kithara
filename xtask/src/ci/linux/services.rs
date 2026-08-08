use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::{
    container::{Container, container},
    profile::{LINUX_CONFIG_PATH, LinuxHost, LinuxRunner, RunnerFlavor},
};
use crate::ci::{config::CiPins, process::Process};

/// Where the services live and what they call.
///
/// The executable is copied out of the build tree: run from there it expects
/// to find the repository around it, and a service starting from no particular
/// directory would not.
struct Layout {
    systemd_root: &'static str,
    executable: &'static str,
    cleanup_unit: &'static str,
    cleanup_timer: &'static str,
}

const LAYOUT: Layout = Layout {
    systemd_root: "/etc/systemd/system",
    executable: "/usr/local/bin/kithara-ci",
    cleanup_unit: "kithara-ci-cleanup.service",
    cleanup_timer: "kithara-ci-cleanup.timer",
};

/// Write one service per runner and hand them to systemd.
///
/// Each service configures its runner just before starting it, so a machine
/// that has been off for a week still comes back with credentials that were
/// minted seconds ago rather than ones that expired while it slept.
pub(super) fn install(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    executable: &str,
) -> Result<()> {
    require_pinned_images(process, host, pins)?;
    if Path::new(executable) != Path::new(LAYOUT.executable) {
        std::fs::copy(executable, LAYOUT.executable)
            .with_context(|| format!("installing {}", LAYOUT.executable))?;
    }
    std::fs::set_permissions(
        LAYOUT.executable,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .with_context(|| format!("making {} executable", LAYOUT.executable))?;

    let cores = std::thread::available_parallelism()
        .context("reading this machine's core count")?
        .get();
    for (index, runner) in host.runners.iter().enumerate() {
        let path = PathBuf::from(LAYOUT.systemd_root).join(runner.service());
        let cpuset = cpuset(index, runner.cpus, cores);
        std::fs::write(&path, unit(host, runner, &cpuset, pins, LAYOUT.executable)?)
            .with_context(|| format!("writing {}", path.display()))?;
        info!(
            service = runner.service(),
            cpuset, "runner service installed"
        );
    }
    install_cleanup_timer(&installed_images(host, pins))?;
    process.run("systemctl", &["daemon-reload"], "reload systemd")?;
    for runner in &host.runners {
        process.run(
            "systemctl",
            &["enable", "--now", &runner.service()],
            "enable runner service",
        )?;
    }
    process.run(
        "systemctl",
        &["enable", "--now", LAYOUT.cleanup_timer],
        "enable the cleanup timer",
    )?;
    Ok(())
}

/// The toolchain and runner image of every flavour this profile uses, each
/// pair named once. A runner image is built on its toolchain, so a machine
/// serving a lane depends on both.
fn flavor_images<'a>(host: &LinuxHost, pins: &'a CiPins) -> Vec<(&'a str, &'a str)> {
    let mut images = Vec::new();
    for runner in &host.runners {
        let pair = match runner.flavor {
            RunnerFlavor::Plain => (pins.linux_image.as_str(), pins.linux_runner_image.as_str()),
            RunnerFlavor::Android => (
                pins.linux_android_image.as_str(),
                pins.linux_android_runner_image.as_str(),
            ),
        };
        if !images.contains(&pair) {
            images.push(pair);
        }
    }
    images
}

/// Which images this profile's runners start from, each named once.
fn required_images<'a>(host: &LinuxHost, pins: &'a CiPins) -> Vec<&'a str> {
    flavor_images(host, pins)
        .into_iter()
        .map(|(_, runner)| runner)
        .collect()
}

/// Everything the installed fleet depends on, in the order cleanup is told it.
fn installed_images<'a>(host: &LinuxHost, pins: &'a CiPins) -> Vec<&'a str> {
    flavor_images(host, pins)
        .into_iter()
        .flat_map(|(toolchain, runner)| [toolchain, runner])
        .collect()
}

/// Refuse to install services the machine cannot run.
///
/// A unit whose image is absent starts, fails to pull, and is restarted — for
/// as long as anyone leaves it. The fleet reports nothing except that every
/// runner is `activating (auto-restart)`, which reads like a runner problem
/// and is a missing build. The pins move with the repository and the images are
/// built by hand here, so the two drift apart on their own; this is where the
/// drift becomes a sentence instead of a symptom.
fn require_pinned_images(process: &Process, host: &LinuxHost, pins: &CiPins) -> Result<()> {
    let missing: Vec<&str> = required_images(host, pins)
        .into_iter()
        .filter(|image| {
            process
                .capture(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", image],
                    "look for a pinned runner image",
                )
                .is_err()
        })
        .collect();
    if !missing.is_empty() {
        bail!(
            "this machine has no image for {}; the pins name it but nothing built it here. \
             Run `kithara-ci ci image toolchain` then `runner` (and `android`, \
             `android-runner` for the emulator lane) before installing services",
            missing.join(" or ")
        );
    }
    Ok(())
}

/// The host keeps every image generation it has ever built, and nothing else
/// reclaims them: the machine is shared, so a blanket prune is not available.
/// Generations share their base layers, so removing five of six freed single
/// gigabytes rather than the hundreds `docker images` reports per image — the
/// point is that the count stops growing, not that a run reclaims much.
///
/// The unit names the images to keep rather than reading the pins, because it
/// runs from a timer with no repository around it — and because what must
/// survive is what this machine was installed to run, not what the checkout
/// happens to pin by the time the timer next fires.
fn cleanup_unit(keep: &[&str]) -> String {
    format!(
        "[Unit]\n\
         Description=Kithara CI cleanup\n\
         After=docker.service\n\
         Requires=docker.service\n\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={executable} ci linux --config {config} cleanup{keep}\n",
        executable = LAYOUT.executable,
        config = LINUX_CONFIG_PATH,
        keep = keep
            .iter()
            .map(|image| format!(" --keep {image}"))
            .collect::<String>(),
    )
}

fn install_cleanup_timer(keep: &[&str]) -> Result<()> {
    let service = cleanup_unit(keep);
    let timer = "[Unit]\n\
                 Description=Kithara CI cleanup\n\n\
                 [Timer]\n\
                 OnCalendar=daily\n\
                 Persistent=true\n\n\
                 [Install]\n\
                 WantedBy=timers.target\n";
    for (name, body) in [
        (LAYOUT.cleanup_unit, service.as_str()),
        (LAYOUT.cleanup_timer, timer),
    ] {
        let path = PathBuf::from(LAYOUT.systemd_root).join(name);
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        info!(unit = name, "cleanup unit installed");
    }
    Ok(())
}

/// Which cores this runner's jobs may use.
///
/// Blocks are handed out in order and wrap around the machine, so runners
/// overlap once they outnumber the cores. The overlap is deliberate: what a set
/// buys over a share is that `nproc` inside the container reports the size of
/// the set, so Cargo starts that many compilations rather than one per host
/// core. Sharing a core between two runners costs throughput; misreporting the
/// count costs memory, and memory is what the kernel kills for.
pub(super) fn cpuset(index: usize, cpus: u32, cores: usize) -> String {
    let cores = u32::try_from(cores).unwrap_or(u32::MAX).max(1);
    let cpus = cpus.clamp(1, cores);
    let first = u32::try_from(index).unwrap_or(0).saturating_mul(cpus) % cores;
    (0..cpus)
        .map(|offset| ((first + offset) % cores).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// The runner takes one job and exits, the container goes with it, and systemd
/// starts the next one. Nothing survives a job except the caches.
///
/// The cargo home is mounted whole rather than as its registry and its git
/// checkouts separately: cargo guards both with a lock file kept beside them,
/// and jobs on this machine run at the same time. Mounting the data without
/// the lock leaves two of them unpacking one crate into one directory.
fn unit(
    host: &LinuxHost,
    runner: &LinuxRunner,
    cpuset: &str,
    pins: &CiPins,
    executable: &str,
) -> Result<String> {
    let mut unit = String::new();
    writeln!(
        unit,
        "[Unit]\n\
         Description=Kithara CI runner {name} (GitHub Actions, ephemeral)\n\
         After=docker.service\n\
         Requires=docker.service\n\n\
         [Service]\n\
         Type=simple\n\
         Restart=always\n\
         RestartSec=10\n\
         RuntimeDirectory=kithara-ci\n\
         RuntimeDirectoryMode=0700\n\n\
         ExecStartPre={executable} ci linux --config {config} firewall\n\
         ExecStartPre={executable} ci linux --config {config} configure --runner {name} \
         --env-file {env_file}\n",
        name = runner.name,
        config = LINUX_CONFIG_PATH,
        env_file = env_file(runner),
    )?;

    let job = container(host, runner, cpuset.to_owned(), pins);
    write!(
        unit,
        "\nExecStart=/usr/bin/docker run --rm --name {name} \
         --network {network} \
         --cpuset-cpus {cpuset} \
         --memory {memory} \
         --pids-limit {pids} \
         --security-opt no-new-privileges \
         --env-file {env_file}",
        name = job.name,
        network = job.network,
        cpuset = job.cpuset,
        memory = job.memory,
        pids = Container::PIDS_LIMIT,
        env_file = job.env_file,
    )?;
    for entry in Container::ENVIRONMENT {
        write!(unit, " --env {entry}")?;
    }
    for (volume, target) in &job.mounts {
        write!(unit, " --mount type=volume,source={volume},target={target}")?;
    }
    for device in job.devices {
        write!(unit, " --device {}", device.display())?;
    }
    for group in job.groups {
        write!(unit, " --group-add {group}")?;
    }
    writeln!(unit, " {}", job.image)?;

    writeln!(
        unit,
        "\nExecStopPost=-/usr/bin/docker rm -f kithara-ci-{name}\n\n\
         [Install]\n\
         WantedBy=multi-user.target",
        name = runner.name,
    )?;
    Ok(unit)
}

/// systemd creates the runtime directory before the first `ExecStartPre`, so
/// the configuration lands somewhere that is wiped when the service stops.
pub(super) fn env_file(runner: &LinuxRunner) -> String {
    format!("/run/kithara-ci/{}.env", runner.name)
}

/// Report what the machine is serving. A runner that is up but unregistered
/// looks identical to a healthy one from the host's side, so the report names
/// the service state rather than claiming a verdict it cannot reach.
pub(super) fn health(process: &Process, host: &LinuxHost) -> Result<()> {
    // Without this, a missing systemctl would read as every runner being down.
    process.require_tools(&["systemctl"])?;
    for runner in &host.runners {
        let state = process
            .capture(
                "systemctl",
                &["is-active", &runner.service()],
                "read a runner service state",
            )
            .unwrap_or_else(|_| "inactive".to_owned());
        info!(
            runner = runner.name,
            labels = runner.labels(),
            state,
            "runner service"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{
        Cli,
        ci::{config::fixture, linux::profile::tests::host_fixture},
    };

    /// The container must see as many cores as it was given, because that
    /// count is what Cargo turns into concurrent compilations.
    #[test]
    fn a_runner_is_given_exactly_the_cores_it_asked_for() {
        for index in 0..12 {
            let set = cpuset(index, 3, 32);
            let cores = set.split(',').collect::<Vec<_>>();
            assert_eq!(cores.len(), 3, "index {index}: {set}");
            assert_eq!(
                cores
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                3,
                "index {index} repeats a core: {set}"
            );
        }
    }

    /// A machine serving both lanes needs both images, and the emulator image
    /// is named once however many emulator runners there are.
    #[test]
    fn the_profile_asks_for_every_image_its_runners_start_from() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let images = required_images(&host, pins);
        assert!(
            images.contains(&pins.linux_runner_image.as_str()),
            "{images:?}"
        );
        assert!(
            images.contains(&pins.linux_android_runner_image.as_str()),
            "{images:?}"
        );
        assert_eq!(images.len(), 2, "each image is named once: {images:?}");
    }

    /// The timer runs from no particular directory, so its command must be one
    /// this executable accepts as written. This unit spent every night failing
    /// on a repository-relative path systemd gave it no repository to resolve.
    #[test]
    fn the_cleanup_unit_is_a_command_this_executable_accepts() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let text = cleanup_unit(&installed_images(&host, pins));

        let command = text
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .expect("the unit must start something")
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>();
        let argv = std::iter::once("xtask").chain(command.iter().copied());
        assert!(Cli::try_parse_from(argv).is_ok(), "{command:?}");
    }

    /// Both lanes' images, and the toolchain each was built on. A runner image
    /// left without its base is rebuilt from scratch; a base kept without its
    /// runner is the fleet's image deleted out from under it.
    #[test]
    fn the_cleanup_unit_names_every_image_the_fleet_runs() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let text = cleanup_unit(&installed_images(&host, pins));
        for image in [
            &pins.linux_image,
            &pins.linux_runner_image,
            &pins.linux_android_image,
            &pins.linux_android_runner_image,
        ] {
            assert!(
                text.contains(&format!("--keep {image}")),
                "{image}:\n{text}"
            );
        }
    }

    /// The same contract as the Compose rendering: a unit that does not name
    /// the wrapper leaves `sccache` installed and unused.
    #[test]
    fn a_unit_is_told_to_use_the_compiler_cache() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let text = unit(
            &host,
            host.runner("kithara-ci-octocat").expect("runner"),
            "0,1,2",
            pins,
            "/usr/local/bin/kithara-ci",
        )
        .expect("the unit must render");
        for entry in Container::ENVIRONMENT {
            assert!(text.contains(&format!("--env {entry}")), "{entry}:\n{text}");
        }
    }

    /// A build directory holds artefacts valid only for the configuration that
    /// made them, so runners must not share one. The registry and the compiler
    /// cache are shared on purpose: both are keyed by content.
    #[test]
    fn each_runner_builds_in_a_directory_of_its_own() {
        let host = host_fixture();
        let first = Container::mounts(host.runner("kithara-ci-octocat").expect("runner"));
        let second = Container::mounts(host.runner("kithara-ci-hubot").expect("runner"));

        let target = |mounts: &[(String, &str)]| {
            mounts
                .iter()
                .find(|(_, at)| *at == "/cache/target")
                .expect("a build directory")
                .0
                .clone()
        };
        assert_ne!(target(&first), target(&second));

        for shared in ["/home/runner/.cargo", "/cache/sccache"] {
            let name = |mounts: &[(String, &str)]| {
                mounts
                    .iter()
                    .find(|(_, at)| *at == shared)
                    .expect(shared)
                    .0
                    .clone()
            };
            assert_eq!(name(&first), name(&second), "{shared} must be shared");
        }
    }

    /// More runners than cores is the point of the exercise: an idle listener
    /// costs nothing, so the machine carries more of them than it has cores.
    #[test]
    fn the_sets_wrap_instead_of_running_out() {
        let set = cpuset(11, 3, 32);
        assert_eq!(set, "1,2,3", "{set}");
        assert_eq!(cpuset(0, 4, 4), "0,1,2,3");
        // A runner may not ask for more of the machine than it has.
        assert_eq!(cpuset(0, 9, 4), "0,1,2,3");
    }

    /// A unit is a command line, and one this crate cannot parse fails only
    /// once systemd has already started the service it belongs to.
    #[test]
    fn the_generated_commands_are_ones_this_executable_accepts() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let unit = unit(
            &host,
            host.runner("kithara-ci-octocat").unwrap(),
            "0,1,2,3",
            pins,
            "/usr/local/bin/kithara-ci",
        )
        .unwrap();

        let commands = unit
            .lines()
            .filter_map(|line| line.strip_prefix("ExecStartPre="))
            .map(|line| line.split_whitespace().skip(1).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), 2, "{unit}");
        for command in commands {
            let argv = std::iter::once("xtask").chain(command.iter().copied());
            assert!(Cli::try_parse_from(argv).is_ok(), "{command:?}");
        }
    }

    #[test]
    fn a_gpu_runner_reaches_the_devices_a_plain_one_does_not() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let plain = unit(
            &host,
            host.runner("kithara-ci-octocat").unwrap(),
            "0,1,2,3",
            pins,
            "/usr/bin/xtask",
        )
        .unwrap();
        let gpu = unit(
            &host,
            host.runner("kithara-ci-octocat-gpu").unwrap(),
            "0,1,2,3",
            pins,
            "/usr/bin/xtask",
        )
        .unwrap();

        let android = unit(
            &host,
            host.runner("kithara-ci-octocat-android").unwrap(),
            "0,1,2,3",
            pins,
            "/usr/bin/xtask",
        )
        .unwrap();

        assert!(!plain.contains("--device"), "{plain}");
        assert!(!plain.contains("--group-add"), "{plain}");
        assert!(gpu.contains("--device /dev/dri"), "{gpu}");
        // A graphics device is useless to a job that may not open it.
        assert!(gpu.contains("--group-add 993"), "{gpu}");
        assert!(android.contains("--device /dev/kvm"), "{android}");
        // Without it the emulator interprets the guest instead of virtualising
        // it, which is the whole reason the lane runs on this machine.
        assert!(android.contains("--group-add 994"), "{android}");
        assert!(
            android.contains(pins.linux_android_runner_image.as_str()),
            "{android}"
        );
        for unit in [&plain, &gpu, &android] {
            assert!(unit.contains("--security-opt no-new-privileges"), "{unit}");
            assert!(!unit.contains("docker.sock"), "{unit}");
        }
    }
}
