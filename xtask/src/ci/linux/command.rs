use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use super::{
    cleanup, compose, firewall,
    profile::{LINUX_CONFIG_PATH, LinuxHost},
    registration, services, system, windows,
};
use crate::ci::{
    config::{CiPins, PINS_PATH},
    process::Process,
};

#[derive(Debug, Args)]
pub(crate) struct LinuxArgs {
    /// Machine profile of this Linux CI host, provisioned outside the repository.
    #[arg(long, env = "KITHARA_CI_LINUX_CONFIG", default_value = LINUX_CONFIG_PATH)]
    config: PathBuf,
    /// Reviewed build pins tracked in the repository.
    #[arg(long, env = "KITHARA_CI_PINS", default_value = PINS_PATH)]
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
    /// Generate the whole fleet as one Compose project, from the same profile.
    Compose {
        /// Where the project is written.
        #[arg(long, default_value = compose::FILE)]
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
    /// Install the Windows guest that serves the Windows lane.
    InstallWindows,
    /// Register the installed Windows guest as a runner and wait for it.
    EnrolWindows,
    /// Report what the machine is currently serving.
    Health,
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
        LinuxCommand::Cleanup { keep } => cleanup::run(&process, keep),
        LinuxCommand::InstallServices => {
            let pins = CiPins::load(&args.pins)?;
            let executable = std::env::current_exe().context("locating this executable")?;
            let executable = executable
                .to_str()
                .context("this executable's path is not UTF-8")?;
            services::install(&process, &host, &pins, executable)
        }
        LinuxCommand::Compose { out, mint } => {
            let pins = CiPins::load(&args.pins)?;
            if *mint {
                for runner in &host.runners {
                    registration::configure(&host, runner, Path::new(&services::env_file(runner)))?;
                }
            }
            compose::write(&host, &pins, out)
        }
        LinuxCommand::InstallWindows => {
            let pins = CiPins::load(&args.pins)?;
            windows::install(&process, &host, &pins, &root)
        }
        LinuxCommand::EnrolWindows => windows::enrol(&process, &host),
        LinuxCommand::Health => services::health(&process, &host),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::Cli;

    fn parse(command: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec![
            "xtask",
            "ci",
            "linux",
            "--config",
            "/etc/kithara-ci/linux-host.toml",
        ];
        argv.extend_from_slice(command);
        Cli::try_parse_from(argv)
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
            ["health"].as_slice(),
        ] {
            assert!(parse(command).is_ok(), "{command:?} must parse");
        }
        assert!(parse(&["configure"]).is_err());
        assert!(parse(&["rm"]).is_err());
    }
}
