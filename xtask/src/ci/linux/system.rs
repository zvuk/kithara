use anyhow::{Context, Result, bail};
use tracing::info;

use super::profile::LinuxHost;
use crate::ci::{
    config::{CiPins, PINS_PATH},
    process::Process,
};

/// Host packages a runner machine needs beyond Docker itself. Everything a job
/// compiles with lives in the image; these are the pieces that must sit on the
/// host because they reach hardware.
///
/// Most of them are what a Windows guest costs: the daemon that owns it, the
/// resolver its network cannot start without, the tool that creates its disk,
/// firmware it can boot from, a
/// software TPM it refuses to install without, the tool that creates it, and
/// one to build the answer file that installs it unattended.
struct Consts;
impl Consts {
    /// The uid the runner image runs its jobs as. It is the image's, not this
    /// machine's, so it is written beside the code that mounts into that image.
    const JOB_USER: u32 = 1000;

    const HOST_PACKAGES: [&'static str; 10] = [
        "iptables",
        "dnsmasq-base",
        "qemu-utils",
        "nvidia-container-toolkit",
        "qemu-system-x86",
        "libvirt-daemon-system",
        "ovmf",
        "swtpm-tools",
        "virtinst",
        "xorriso",
    ];
}

/// Prepare the machine a runner will live on: its caches, its network, and the
/// packages that cannot live in an image.
pub(super) fn bootstrap(process: &Process, host: &LinuxHost) -> Result<()> {
    require_linux()?;
    process.require_tools(&["docker"])?;

    std::fs::create_dir_all(&host.cache_root)
        .with_context(|| format!("creating {}", host.cache_root.display()))?;

    // Docker refuses to create a network that exists, and refusing to continue
    // over that would make every later run of this command fail.
    let existing = process.capture(
        "docker",
        &["network", "ls", "--format", "{{.Name}}"],
        "list Docker networks",
    )?;
    if !existing.lines().any(|name| name == host.network) {
        process.run(
            "docker",
            &["network", "create", "--subnet", &host.subnet, &host.network],
            "create the runner network",
        )?;
    }

    let mut volumes: Vec<String> = host
        .runners
        .iter()
        .flat_map(|runner| {
            super::container::Container::mounts(host, runner)
                .into_iter()
                .map(|(name, _)| name)
        })
        .collect();
    volumes.sort();
    volumes.dedup();
    let pins = CiPins::load(std::path::Path::new(PINS_PATH))?;
    for volume in &volumes {
        if std::path::Path::new(volume).is_absolute() {
            std::fs::create_dir_all(volume)
                .with_context(|| format!("creating runner cache directory {volume}"))?;
        } else {
            process.run(
                "docker",
                &["volume", "create", volume],
                "create a runner cache volume",
            )?;
        }
        give_to_the_job(process, volume, &pins)?;
    }
    info!(network = host.network, "runner machine prepared");
    Ok(())
}

/// Install the host packages. GPU access needs the container toolkit and an
/// emulator needs QEMU, and neither can be carried in the image that uses them.
pub(super) fn install_tools(process: &Process) -> Result<()> {
    require_linux()?;

    // Only what is missing. Naming a package that is already installed invites
    // apt to upgrade it, and upgrading the GPU stack underneath a machine that
    // is serving other work is not this command's business.
    let missing: Vec<&str> = Consts::HOST_PACKAGES
        .into_iter()
        .filter(|package| {
            // A package dpkg cannot describe at all is missing just as surely
            // as one it describes as not installed.
            process
                .capture(
                    "dpkg-query",
                    &["-W", "-f=${Status}", package],
                    "read a package's state",
                )
                .ok()
                .is_none_or(|status| !status.starts_with("install ok installed"))
        })
        .collect();
    if missing.is_empty() {
        info!("host packages already present");
        return Ok(());
    }
    info!(packages = missing.join(", "), "installing host packages");

    process.run("apt-get", &["update"], "refresh the package index")?;
    let mut install = process.command("apt-get");
    install
        .args(["install", "-y", "--no-install-recommends"])
        .args(&missing);
    process.run_command(&mut install, "install host packages")?;

    // The toolkit ships the runtime but does not register it, and a GPU runner
    // that starts without it fails on its first job rather than at setup.
    //
    // Only when it was this command that installed it: restarting Docker stops
    // every container on the machine, including ones this repository does not
    // own, and doing that to re-apply a configuration that is already in place
    // would be a poor trade.
    if missing.contains(&"nvidia-container-toolkit") {
        process.run(
            "nvidia-ctk",
            &["runtime", "configure", "--runtime=docker"],
            "register the GPU container runtime",
        )?;
        process.run("systemctl", &["restart", "docker"], "restart Docker")?;
    }
    Ok(())
}

fn require_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("this command provisions a Linux CI machine and must run on one");
    }
    Ok(())
}

/// Hand a cache mount to the user the job runs as.
///
/// Docker fills a fresh named volume from the image, ownership included — but
/// only where the image has that directory. A mount point the image does not
/// carry gets an empty volume owned by root, and a job that is not root then
/// cannot write to its own cache. That is not a cache being temperamental: it
/// is a volume nobody gave away. Both existing volumes worked by accident of
/// their paths existing in the image; this makes it true on purpose, for every
/// cache mount, on every machine that bootstraps.
fn give_to_the_job(process: &Process, volume: &str, pins: &CiPins) -> Result<()> {
    let mount_type = super::container::Container::mount_type(volume);
    let mount = format!("type={mount_type},source={volume},target=/volume");
    let owner = format!("chown {user}:{user} /volume", user = Consts::JOB_USER);
    process.run(
        "docker",
        &[
            "run",
            "--rm",
            // As root, because the point is to give the directory away, and the
            // image starts as the user being given it.
            "--user",
            "0:0",
            "--mount",
            &mount,
            "--entrypoint",
            "sh",
            &pins.linux_runner_image,
            "-c",
            &owner,
        ],
        "give a cache volume to the job user",
    )
}
