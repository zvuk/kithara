use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tracing::info;

use super::{
    container::{Container, container},
    profile::{LinuxHost, LinuxRunner, RunnerFlavor},
};
use crate::{
    ci::{cache::client_environment, config::CiPins, image::floating_tag, process::Process},
    consts,
};

/// Write one service per runner and hand them to systemd.
///
/// Each service configures its runner just before starting it, so a machine
/// that has been off for a week still comes back with credentials that were
/// minted seconds ago rather than ones that expired while it slept. A machine
/// that hosts the Windows guest also gets the timer that rebuilds it from the
/// repository at `root`.
pub(super) fn install(
    process: &Process,
    host: &LinuxHost,
    pins: &CiPins,
    executable: &str,
    root: &Path,
) -> Result<()> {
    require_pinned_images(process, host, pins)?;
    if Path::new(executable) != Path::new(consts::SERVICE_EXECUTABLE) {
        std::fs::copy(executable, consts::SERVICE_EXECUTABLE)
            .with_context(|| format!("installing {}", consts::SERVICE_EXECUTABLE))?;
    }
    super::permissions::set_mode(Path::new(consts::SERVICE_EXECUTABLE), consts::EXECUTABLE)
        .with_context(|| format!("making {} executable", consts::SERVICE_EXECUTABLE))?;

    let cores = std::thread::available_parallelism()
        .context("reading this machine's core count")?
        .get();
    for (index, runner) in host.runners.iter().enumerate() {
        client_environment(&runner.sccache_s3_env_file).with_context(|| {
            format!(
                "validating S3 cache environment for Linux runner {}",
                runner.name
            )
        })?;
        let path = PathBuf::from(consts::SERVICE_SYSTEMD_ROOT).join(runner.service());
        let cpuset = cpuset(index, runner.cpus, cores);
        std::fs::write(
            &path,
            unit(host, runner, &cpuset, pins, consts::SERVICE_EXECUTABLE)?,
        )
        .with_context(|| format!("writing {}", path.display()))?;
        info!(
            service = runner.service(),
            cpuset, "runner service installed"
        );
    }
    install_slice()?;
    install_cleanup_timer(&installed_images(host, pins)?)?;
    if host.windows.is_some() {
        install_renewal_timer(root)?;
    }
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
        &["enable", "--now", consts::SERVICE_CLEANUP_TIMER],
        "enable the cleanup timer",
    )?;
    if host.windows.is_some() {
        process.run(
            "systemctl",
            &["enable", "--now", consts::SERVICE_RENEWAL_TIMER],
            "enable the Windows renewal timer",
        )?;
    }
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

/// Which images this profile's runners start from, each named once. The units
/// name the floating tag, so that is what has to be on the machine.
fn required_images(host: &LinuxHost, pins: &CiPins) -> Result<Vec<String>> {
    flavor_images(host, pins)
        .into_iter()
        .map(|(_, runner)| floating_tag(runner))
        .collect()
}

/// Everything the installed fleet depends on, in the order cleanup is told it.
/// Both spellings of each image are kept: the pin is what a rebuild compares
/// against, and the floating tag is what the running containers hold.
fn installed_images(host: &LinuxHost, pins: &CiPins) -> Result<Vec<String>> {
    let mut images = Vec::new();
    for (toolchain, runner) in flavor_images(host, pins) {
        for image in [toolchain, runner] {
            images.push(image.to_owned());
            images.push(floating_tag(image)?);
        }
    }
    Ok(images)
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
    let missing: Vec<String> = required_images(host, pins)?
        .into_iter()
        .filter(|image| {
            process
                .capture(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", image.as_str()],
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
fn cleanup_unit(keep: &[String]) -> String {
    format!(
        "[Unit]\n\
         Description=Kithara CI cleanup\n\
         After=docker.service\n\
         Requires=docker.service\n\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={executable} ci host linux --config {config} cleanup{keep}\n",
        executable = consts::SERVICE_EXECUTABLE,
        config = consts::LINUX_CONFIG_PATH,
        keep = keep
            .iter()
            .map(|image| format!(" --keep {image}"))
            .collect::<String>(),
    )
}

/// Every two hours, because the fleet fills faster than a day. Measured across
/// one day of this timer on the Linux host: the build caches waiting at a pass
/// ranged from 20 GB to 515 GB, each amount accumulated inside a single
/// two-hour window, against 3.0 TB free. A daily timer would meet twelve such
/// windows at once, which a busy day does not fit on the volume — so the host
/// this shipped to had been carrying a hand-written drop-in overriding the
/// cadence, a second source of truth this file could not see. A pass over a
/// volume already inside its budget frees nothing and costs seconds, so the
/// frequency is only paid for when it is needed.
fn cleanup_timer() -> &'static str {
    "[Unit]\n\
     Description=Kithara CI cleanup\n\n\
     [Timer]\n\
     OnCalendar=*-*-* 00/2:00:00\n\
     Persistent=true\n\n\
     [Install]\n\
     WantedBy=timers.target\n"
}

/// The guest is built from the answers, the provisioning script and the pins
/// the repository tracks, so the unit runs in the checkout it was installed
/// from. The command itself decides whether the guest needs rebuilding; the
/// timer only asks once a day.
fn renewal_unit(root: &Path) -> Result<String> {
    let root = root
        .to_str()
        .with_context(|| format!("the repository path {} is not UTF-8", root.display()))?;
    Ok(format!(
        "[Unit]\n\
         Description=Kithara CI Windows guest renewal\n\
         After=libvirtd.service network-online.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         Type=oneshot\n\
         WorkingDirectory={root}\n\
         ExecStart={executable} ci host linux --config {config} renew-windows\n",
        executable = consts::SERVICE_EXECUTABLE,
        config = consts::LINUX_CONFIG_PATH,
    ))
}

/// Daily: a licence runs for weeks and the rebuild waits for a free guest, so
/// a day between looks leaves several chances inside the last week.
fn renewal_timer() -> &'static str {
    "[Unit]\n\
     Description=Kithara CI Windows guest renewal\n\n\
     [Timer]\n\
     OnCalendar=daily\n\
     Persistent=true\n\n\
     [Install]\n\
     WantedBy=timers.target\n"
}

/// The budget the whole fleet shares, generated here so that rewriting the
/// units can never again leave them outside it.
///
/// The host livelocked three times in September with swap exhausted and
/// nothing in the logs: twenty-seven runners carried 488G of individual
/// ceilings on a 123G machine, and none of those ceilings ever fired, because
/// the machine died of their sum. Past `MemoryHigh` the kernel throttles and
/// reclaims inside the slice; at `MemoryMax` it kills inside the fleet, never
/// the services the machine runs besides it. Both are shares of the machine's
/// memory, so the budget follows the hardware. Swap takes no share, so half of
/// what the machine has is named in bytes, and the other half stays with the
/// rest of the machine.
fn slice(meminfo: &str) -> Result<String> {
    let swap = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("SwapTotal:"))
        .and_then(|value| value.trim().strip_suffix("kB"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .context("reading SwapTotal from /proc/meminfo")?;
    Ok(format!(
        "[Unit]\n\
         Description=Kithara CI runner fleet\n\
         Before=slices.target\n\n\
         [Slice]\n\
         MemoryHigh=65%\n\
         MemoryMax=72%\n\
         MemorySwapMax={half}K\n",
        half = swap / 2,
    ))
}

/// Writes the fleet's memory budget, which every runner container is started
/// under whichever way the fleet is brought up. systemd reads it on its next
/// reload.
pub(super) fn install_slice() -> Result<()> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").context("reading /proc/meminfo")?;
    let path = PathBuf::from(consts::SERVICE_SYSTEMD_ROOT).join(consts::SERVICE_SLICE);
    std::fs::write(&path, slice(&meminfo)?)
        .with_context(|| format!("writing {}", path.display()))?;
    info!(
        unit = consts::SERVICE_SLICE,
        "fleet memory budget installed"
    );
    Ok(())
}

fn install_cleanup_timer(keep: &[String]) -> Result<()> {
    let service = cleanup_unit(keep);
    for (name, body) in [
        (consts::SERVICE_CLEANUP_UNIT, service.as_str()),
        (consts::SERVICE_CLEANUP_TIMER, cleanup_timer()),
    ] {
        let path = PathBuf::from(consts::SERVICE_SYSTEMD_ROOT).join(name);
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        info!(unit = name, "cleanup unit installed");
    }
    Ok(())
}

fn install_renewal_timer(root: &Path) -> Result<()> {
    let service = renewal_unit(root)?;
    for (name, body) in [
        (consts::SERVICE_RENEWAL_UNIT, service.as_str()),
        (consts::SERVICE_RENEWAL_TIMER, renewal_timer()),
    ] {
        let path = PathBuf::from(consts::SERVICE_SYSTEMD_ROOT).join(name);
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        info!(unit = name, "Windows renewal unit installed");
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
/// starts the next one. The runner keeps its checkout and caches; checkout
/// cleans generated files and updates tracked files without retouching unchanged
/// build-script inputs. Cargo still checks those inputs by modification time.
///
/// The cargo home is mounted whole rather than as its registry and its git
/// checkouts separately: cargo guards both with a lock file kept beside them,
/// and jobs on this machine run at the same time. Mounting the data without
/// the lock leaves two of them unpacking one crate into one directory.
///
/// `RuntimeDirectoryPreserve=yes` is not an optimisation. Every runner on the
/// machine declares the same `RuntimeDirectory=kithara-ci`, and systemd removes
/// a runtime directory when the unit that declared it stops — so one runner
/// going down deletes the directory the others are still using, and restarting
/// the fleet has them delete it from under each other until none can start.
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
         RuntimeDirectoryMode=0700\n\
         RuntimeDirectoryPreserve=yes\n\n\
         ExecStartPre={executable} ci host linux --config {config} firewall\n\
         ExecStartPre={executable} ci host linux --config {config} configure --runner {name} \
         --env-file {env_file}\n",
        name = runner.name,
        config = consts::LINUX_CONFIG_PATH,
        env_file = env_file(runner),
    )?;

    let job = container(host, runner, cpuset.to_owned(), pins)?;
    write!(
        unit,
        "\nExecStart=/usr/bin/docker run --init --rm --name {name} \
         --network {network} \
         --cpuset-cpus {cpuset} \
         --memory {memory} \
         --cgroup-parent {cgroup_parent} \
         --pids-limit {pids} \
         --ulimit nice={nice}:{nice} \
         --security-opt no-new-privileges \
         --env-file {env_file}",
        name = job.name,
        network = job.network,
        cpuset = job.cpuset,
        memory = job.memory,
        cgroup_parent = job.cgroup_parent,
        pids = Container::PIDS_LIMIT,
        nice = Container::NICE_LIMIT,
        env_file = job.env_file,
    )?;
    for entry in Container::environment(runner) {
        write!(unit, " --env {entry}")?;
    }
    for (volume, target) in &job.mounts {
        let mount_type = Container::mount_type(volume);
        write!(
            unit,
            " --mount type={mount_type},source={volume},target={target}"
        )?;
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
        ci::{config::fixture, host::linux::profile::tests::host_fixture},
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
    /// is named once however many emulator runners there are. What is required
    /// is the floating tag: the units run that, and a pin the machine has not
    /// built yet is not what would be missing at start.
    #[test]
    fn the_profile_asks_for_every_image_its_runners_start_from() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let images = required_images(&host, pins).expect("the pins carry tags");
        for pin in [&pins.linux_runner_image, &pins.linux_android_runner_image] {
            let floating = floating_tag(pin).expect("the pin carries a tag");
            assert!(images.contains(&floating), "{floating}: {images:?}");
        }
        assert_eq!(images.len(), 2, "each image is named once: {images:?}");
    }

    /// The timer runs from no particular directory, so its command must be one
    /// this executable accepts as written. This unit spent every night failing
    /// on a repository-relative path systemd gave it no repository to resolve.
    #[test]
    fn the_cleanup_unit_is_a_command_this_executable_accepts() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let text = cleanup_unit(&installed_images(&host, pins).expect("the pins carry tags"));

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

    /// The guest is built from files the repository tracks, so the timer that
    /// rebuilds it has to run where they are and say so: a unit runs from no
    /// particular directory unless it is told one.
    #[test]
    fn the_renewal_unit_runs_a_command_this_executable_accepts_in_the_repository() {
        let text = renewal_unit(Path::new("/srv/kithara")).expect("the unit must render");

        assert!(text.contains("WorkingDirectory=/srv/kithara\n"), "{text}");
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

    /// The cadence is the whole of the disk policy: the cleaner runs when the
    /// timer says so and at no other time. A daily pass was measured arriving
    /// at up to 515 GB of build caches, and that was one two-hour window's
    /// worth.
    #[test]
    fn the_cleanup_timer_outpaces_what_the_fleet_accumulates() {
        assert!(
            cleanup_timer().contains("OnCalendar=*-*-* 00/2:00:00"),
            "{}",
            cleanup_timer()
        );
        assert!(
            !cleanup_timer().contains("OnCalendar=daily"),
            "{}",
            cleanup_timer()
        );
    }

    /// Both lanes' images, and the toolchain each was built on. A runner image
    /// left without its base is rebuilt from scratch; a base kept without its
    /// runner is the fleet's image deleted out from under it.
    #[test]
    fn the_cleanup_unit_names_every_image_the_fleet_runs() {
        let host = host_fixture();
        let pins = &fixture().pins;
        let text = cleanup_unit(&installed_images(&host, pins).expect("the pins carry tags"));
        for pin in [
            &pins.linux_image,
            &pins.linux_runner_image,
            &pins.linux_android_image,
            &pins.linux_android_runner_image,
        ] {
            // The pin is what a rebuild compares against and the floating tag
            // is what the containers hold; reclaiming either one takes the
            // fleet's image out from under it.
            for image in [
                pin.clone(),
                floating_tag(pin).expect("the pin carries a tag"),
            ] {
                assert!(
                    text.contains(&format!("--keep {image}")),
                    "{image}:\n{text}"
                );
            }
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
        for entry in Container::environment(host.runner("kithara-ci-octocat").expect("runner")) {
            assert!(text.contains(&format!("--env {entry}")), "{entry}:\n{text}");
        }
        assert!(
            text.contains("--env SCCACHE_BASEDIRS=/runner/_work/kithara/kithara"),
            "{text}"
        );
        assert!(
            text.contains("--env SCCACHE_SERVER_UDS=/tmp/kithara-ci-octocat.sock"),
            "{text}"
        );
        assert!(text.contains("--env RUSTC_WRAPPER=kache"), "{text}");
        assert!(text.contains("--env KACHE_FALLBACK=sccache"), "{text}");
    }

    /// The store's host file is completed where the runner starts, so the unit
    /// must not hand it to docker as it is: a file written before a key existed
    /// would reach the job without it.
    #[test]
    fn a_unit_leaves_the_store_to_the_runtime_environment() {
        let host = host_fixture();
        let runner = host.runner("kithara-ci-octocat").expect("runner");
        let text = unit(
            &host,
            runner,
            "0,1,2",
            &fixture().pins,
            "/usr/local/bin/kithara-ci",
        )
        .expect("the unit must render");

        assert!(
            !text.contains(&runner.sccache_s3_env_file.display().to_string()),
            "{text}"
        );
        assert!(
            text.contains(&format!("--env-file {}", env_file(runner))),
            "{text}"
        );
    }

    #[test]
    fn every_runner_reaps_orphaned_job_processes() {
        let host = host_fixture();
        for runner in &host.runners {
            let text = unit(
                &host,
                runner,
                "0,1,2",
                &fixture().pins,
                "/usr/local/bin/kithara-ci",
            )
            .expect("the unit must render");
            assert!(text.contains("docker run --init "), "{text}");
        }
    }

    /// No single runner's ceiling ever fired: the host died of the sum of them.
    /// Every runner therefore lives in the one slice that caps the fleet.
    #[test]
    fn every_runner_draws_on_the_fleet_memory_budget() {
        let host = host_fixture();
        for runner in &host.runners {
            let text = unit(
                &host,
                runner,
                "0,1,2",
                &fixture().pins,
                "/usr/local/bin/kithara-ci",
            )
            .expect("the unit must render");
            assert!(
                text.contains(&format!("--cgroup-parent {}", consts::SERVICE_SLICE)),
                "{text}"
            );
        }
    }

    /// The fleet may throttle and then die inside its slice, never take the
    /// memory or the swap the rest of the machine runs on.
    #[test]
    fn the_fleet_slice_leaves_memory_and_swap_to_the_rest_of_the_machine() {
        let text = slice(
            "MemTotal:       128848504 kB\n\
             SwapTotal:       33554428 kB\n",
        )
        .expect("the slice must render");

        let percent = |key: &str| -> u32 {
            let line = text
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| panic!("{key} missing:\n{text}"));
            line.trim_end_matches('%')
                .parse()
                .unwrap_or_else(|_| panic!("{key} is not a share of memory:\n{text}"))
        };
        let high = percent("MemoryHigh=");
        let max = percent("MemoryMax=");
        assert!(high < max && max < 100, "{text}");
        assert!(
            text.contains(&format!("MemorySwapMax={}K", 33_554_428 / 2)),
            "{text}"
        );
    }

    /// The whole fleet declares one runtime directory, so systemd must be told
    /// to keep it: without this, stopping any single runner removes the
    /// directory every other runner is still using.
    #[test]
    fn the_shared_runtime_directory_outlives_a_single_runner() {
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

        assert!(text.contains("RuntimeDirectoryPreserve=yes"), "{text}");
    }

    /// A runner keeps its build root and its workspace to itself: a lane's
    /// directory lives behind an alias the job re-points, and the checkout is
    /// the one the alias's builds were keyed against.
    #[test]
    fn every_runner_keeps_its_own_build_root_and_workspace() {
        let host = host_fixture();
        let first = Container::mounts(&host, host.runner("kithara-ci-octocat").expect("runner"));
        let second = Container::mounts(&host, host.runner("kithara-ci-hubot").expect("runner"));

        let mount = |mounts: &[(String, &str)], at: &str| {
            mounts
                .iter()
                .find(|(_, mounted)| *mounted == at)
                .unwrap_or_else(|| panic!("a mount at {at}"))
                .0
                .clone()
        };
        for at in [consts::BUILD_ROOT_MOUNT, "/runner/_work"] {
            assert_ne!(mount(&first, at), mount(&second, at), "{at}");
        }
        assert_eq!(
            mount(&first, consts::BUILD_ROOT_MOUNT),
            "/var/lib/kithara-ci/target/kithara-ci-octocat"
        );
        assert_eq!(
            mount(&first, "/runner/_work"),
            "/var/lib/kithara-ci/workspaces/kithara-ci-octocat"
        );
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
        let emulator =
            floating_tag(&pins.linux_android_runner_image).expect("the pin carries a tag");
        assert!(android.contains(&emulator), "{emulator}: {android}");
        for unit in [&plain, &gpu, &android] {
            assert!(unit.contains("--security-opt no-new-privileges"), "{unit}");
            // The player's audio feed thread asks for nice -16; a job that
            // may not lower a nice value keeps it at the compile jobs' level.
            assert!(unit.contains("--ulimit nice=40:40"), "{unit}");
            assert!(!unit.contains("docker.sock"), "{unit}");
        }
    }
}
