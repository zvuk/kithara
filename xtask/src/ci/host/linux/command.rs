use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use tracing::info;

use super::{
    cleanup, compose, firewall,
    profile::{LinuxHost, RunnerFlavor},
    registration, services, system, windows,
};
use crate::{
    ci::{
        config::CiPins,
        host::provision::Provision,
        image::{self, ImageCommand},
        process::Process,
    },
    consts,
};

#[derive(Debug, Args)]
pub(crate) struct LinuxArgs {
    /// Machine profile of this Linux CI host, provisioned outside the repository.
    #[arg(long, env = "KITHARA_CI_LINUX_CONFIG", default_value = consts::LINUX_CONFIG_PATH)]
    config: PathBuf,
    /// Reviewed build pins tracked in the repository.
    #[arg(long, env = "KITHARA_CI_PINS", default_value = consts::PINS_PATH)]
    pins: PathBuf,
    #[command(subcommand)]
    command: LinuxCommand,
}

#[derive(Debug, Subcommand)]
enum LinuxCommand {
    /// Install the host packages a runner machine needs beyond Docker.
    InstallTools,
    /// Create the caches and the fenced network the runners live on.
    Bootstrap,
    /// Restore the rules that keep the runners away from the rest of the host.
    Firewall,
    /// Mint one runner's just-in-time configuration. Runs before its service.
    Configure {
        /// Runner to configure, named by the profile.
        #[arg(long)]
        runner: String,
        /// File the configuration is written to, readable only by root.
        #[arg(long)]
        env_file: PathBuf,
    },
    /// Write and enable one service per runner in the profile.
    InstallServices,
    /// Generate the whole fleet as one Compose project, from the same profile,
    /// and install the memory budget its services are started under.
    Compose {
        /// Where the project is written.
        #[arg(long, default_value = consts::FILE)]
        out: PathBuf,
        /// Mint every runner's registration first. They are accepted once, so
        /// this runs immediately before `docker compose up`, not ahead of time.
        #[arg(long)]
        mint: bool,
    },
    /// Reclaim superseded project images and stale build cache.
    Cleanup {
        /// An image this machine is installed to run, named once per image.
        /// Written by `install-services`, which knows what it installed.
        #[arg(long = "keep")]
        keep: Vec<String>,
    },
    /// Install the Windows guest that serves the Windows lane, replacing any
    /// guest already there, and wait until it is a registered runner.
    InstallWindows,
    /// Register the installed Windows guest as a runner again and wait for it.
    EnrolWindows,
    /// Build the Windows guest again when its licence is about to run out and
    /// it runs no job. A daily timer runs this.
    RenewWindows,
    /// Report what the machine is currently serving.
    Health,
}

/// Bring this machine up to what the current commit describes.
///
/// The images come first because the services refuse to install without them,
/// and both steps are idempotent: a rebuild whose inputs have not changed is
/// answered out of the layer cache, and the services are rewritten from the
/// same profile every time.
pub(in crate::ci::host) fn provision(
    process: &Process,
    config: Option<&Path>,
    pins: &Path,
) -> Result<()> {
    let config = config.map_or_else(
        || PathBuf::from(consts::LINUX_CONFIG_PATH),
        Path::to_path_buf,
    );
    let host = LinuxHost::load(&config)?;
    let provision = Provision {
        process,
        config: &config,
        pins,
    };
    let pins = CiPins::load(pins)?;
    process.require_tools(&["docker"])?;
    for image in required_builds(&host) {
        image::build_pinned(process, image, &pins, Some(&host.image_cache_env_file))?;
    }
    for step in consts::LINUX_PROVISION_STEPS {
        provision.as_root("linux", step)?;
    }
    info!(platform = "linux", "host provisioned from this commit");
    Ok(())
}

/// Every image this profile's runners are built from, base before layer. An
/// Android machine still builds the plain toolchain: the emulator image is
/// layered on it.
fn required_builds(host: &LinuxHost) -> Vec<ImageCommand> {
    let mut builds = vec![ImageCommand::Toolchain];
    if host
        .runners
        .iter()
        .any(|runner| matches!(runner.flavor, RunnerFlavor::Plain))
    {
        builds.push(ImageCommand::Runner);
    }
    if host
        .runners
        .iter()
        .any(|runner| matches!(runner.flavor, RunnerFlavor::Android))
    {
        builds.push(ImageCommand::Android);
        builds.push(ImageCommand::AndroidRunner);
    }
    builds
}

pub(crate) fn run(args: &LinuxArgs) -> Result<()> {
    let root = std::env::current_dir()?;
    let process = Process::new(&root, BTreeMap::new());
    if matches!(args.command, LinuxCommand::InstallTools) {
        return system::install_tools(&process);
    }

    let host = LinuxHost::load(&args.config)?;
    match &args.command {
        LinuxCommand::InstallTools => unreachable!("handled before the profile is read"),
        LinuxCommand::Bootstrap => system::bootstrap(&process, &host),
        LinuxCommand::Firewall => firewall::apply(&process, &host),
        LinuxCommand::Configure { runner, env_file } => {
            registration::configure(&host, host.runner(runner)?, env_file)
        }
        LinuxCommand::Cleanup { keep } => cleanup::run(&process, &host, keep),
        LinuxCommand::InstallServices => {
            let pins = CiPins::load(&args.pins)?;
            let executable = std::env::current_exe().context("locating this executable")?;
            let executable = executable
                .to_str()
                .context("this executable's path is not UTF-8")?;
            services::install(&process, &host, &pins, executable, &root)
        }
        LinuxCommand::Compose { out, mint } => {
            let pins = CiPins::load(&args.pins)?;
            if *mint {
                for runner in &host.runners {
                    registration::configure(&host, runner, Path::new(&services::env_file(runner)))?;
                }
            }
            services::install_slice()?;
            process.run("systemctl", &["daemon-reload"], "reload systemd")?;
            compose::write(&host, &pins, out)
        }
        LinuxCommand::InstallWindows => {
            let pins = CiPins::load(&args.pins)?;
            windows::install(&process, &host, &pins, &root)
        }
        LinuxCommand::EnrolWindows => windows::enrol(&process, &host),
        LinuxCommand::RenewWindows => {
            let pins = CiPins::load(&args.pins)?;
            windows::renew(&process, &host, &pins, &root)
        }
        LinuxCommand::Health => {
            services::health(&process, &host)?;
            windows::health(&process, &host)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::Cli;

    fn parse(command: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec![
            "xtask",
            "ci",
            "host",
            "linux",
            "--config",
            "/etc/kithara-ci/linux-host.toml",
        ];
        argv.extend_from_slice(command);
        Cli::try_parse_from(argv)
    }

    /// The emulator image is layered on the plain toolchain, so a machine
    /// serving the Android lane builds that first. Missing it is a machine
    /// that builds the emulator against whatever toolchain it happens to
    /// still hold.
    #[test]
    fn an_android_machine_builds_the_toolchain_its_emulator_stands_on() {
        let host = super::super::profile::tests::host_fixture();
        let builds = required_builds(&host);
        assert!(
            matches!(builds.first(), Some(ImageCommand::Toolchain)),
            "{builds:?}"
        );
        assert!(
            builds
                .iter()
                .any(|image| matches!(image, ImageCommand::AndroidRunner)),
            "{builds:?}"
        );
    }

    /// A runner that starts on a mount nobody handed over cannot write its
    /// cache: the start creates the directory as root, and the job is not
    /// root. Provisioning is what brings a machine to this commit, so it
    /// prepares every mount this commit's runners use before it installs
    /// the services that start them.
    #[test]
    fn provisioning_hands_every_cache_mount_over_before_runners_start() {
        let steps = consts::LINUX_PROVISION_STEPS;
        for step in steps {
            assert!(parse(&[step]).is_ok(), "{step} must be a machine command");
        }
        let position = |step: &str| steps.iter().position(|named| *named == step);
        assert!(
            matches!(
                (position("bootstrap"), position("install-services")),
                (Some(prepare), Some(start)) if prepare < start
            ),
            "{steps:?}"
        );
    }

    #[test]
    fn machine_commands_are_typed() {
        for command in [
            ["install-tools"].as_slice(),
            ["bootstrap"].as_slice(),
            ["firewall"].as_slice(),
            [
                "configure",
                "--runner",
                "kithara-ci",
                "--env-file",
                "/run/kithara-ci/kithara-ci.env",
            ]
            .as_slice(),
            ["install-services"].as_slice(),
            ["install-windows"].as_slice(),
            ["enrol-windows"].as_slice(),
            ["renew-windows"].as_slice(),
            ["health"].as_slice(),
        ] {
            assert!(parse(command).is_ok(), "{command:?} must parse");
        }
        assert!(parse(&["configure"]).is_err());
        assert!(parse(&["rm"]).is_err());
    }
}
