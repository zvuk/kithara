use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, thread, time::Duration};

use anyhow::Result;
use clap::{Args, Subcommand};
use tracing::error;

use super::{
    runners::RunnerManager,
    services::ServiceInstaller,
    storage::HostStorage,
    system::SystemSetup,
    toolchain::ToolchainInstaller,
    windows::{Boot, WindowsHost},
};
use crate::ci::{
    config::{CiConfig, PINS_PATH},
    process::Process,
};

#[derive(Debug, Args)]
pub(crate) struct HostArgs {
    /// Machine profile of this CI host, provisioned outside the repository.
    #[arg(long, env = "KITHARA_CI_HOST_CONFIG")]
    config: PathBuf,
    /// Reviewed build pins tracked in the repository.
    #[arg(long, env = "KITHARA_CI_PINS", default_value = PINS_PATH)]
    pins: PathBuf,
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// Create and validate the bounded APFS volume and dedicated CI account.
    Bootstrap,
    /// Configure Xcode and install the root-owned CI service binary.
    Finish,
    /// Install host packages through the existing Homebrew installation.
    InstallHostTools,
    /// Install pinned Rust and Android tools for the CI account.
    InstallUserTools,
    /// Install the current Rust executable and launchd service definitions.
    InstallServices,
    /// Validate credentials and activate the repository bridge daemon.
    ActivateBridge,
    /// Configure `GitLab` runners from local token files.
    ConfigureRunners,
    /// Load or reload the CI user's launchd services.
    Activate,
    /// Prepare a throwaway macOS guest before `GitLab` Runner starts.
    GuestPrepare,
    /// Build and pin the Linux runner image.
    BuildLinuxImage {
        /// Dockerfile used to build the pinned Linux image.
        dockerfile: PathBuf,
    },
    /// Verify the pinned Linux runner image and its required tools.
    SmokeLinux,
    /// Boot the Android emulator and verify it reaches a ready state.
    SmokeAndroid,
    /// Serve `GitLab` jobs from throwaway macOS VMs.
    RunMacosRunner,
    /// Start the Windows guest that serves the Windows lane.
    RunWindowsGuest {
        /// Boot the Microsoft media with the answer file instead of the
        /// installed disk.
        #[arg(long)]
        install: bool,
    },
    /// Reject a job before it can fill or damage the CI volume.
    Preflight,
    /// Remove expired job state and bounded cache entries.
    Cleanup,
    /// Emit host storage and runner health as JSON.
    Health,
}

pub(crate) fn run(args: &HostArgs) -> Result<()> {
    let config = CiConfig::load(&args.config, &args.pins)?;
    config.validate_macos_layout()?;
    let root = std::env::current_dir()?;
    let mut vars = BTreeMap::new();
    vars.insert(
        OsString::from("TART_HOME"),
        config.host.tart_home()?.as_os_str().to_os_string(),
    );
    let process = Process::new(&root, vars);
    match &args.command {
        HostCommand::Bootstrap => SystemSetup::new(&config, &process).bootstrap(),
        HostCommand::Finish => {
            SystemSetup::new(&config, &process).finish()?;
            ServiceInstaller::new(&config, &process).install(&args.config, &args.pins)
        }
        HostCommand::InstallHostTools => {
            ToolchainInstaller::new(&config, &process).install_host_tools()
        }
        HostCommand::InstallUserTools => {
            ToolchainInstaller::new(&config, &process).install_user_tools()
        }
        HostCommand::InstallServices => {
            ServiceInstaller::new(&config, &process).install(&args.config, &args.pins)
        }
        HostCommand::ActivateBridge => ServiceInstaller::new(&config, &process).activate_bridge(),
        HostCommand::ConfigureRunners => RunnerManager::new(&config, &process).configure(),
        HostCommand::Activate => RunnerManager::new(&config, &process).activate(),
        HostCommand::GuestPrepare => RunnerManager::new(&config, &process).prepare_guest(),
        HostCommand::BuildLinuxImage { dockerfile } => {
            RunnerManager::new(&config, &process).build_linux_image(dockerfile)
        }
        HostCommand::SmokeLinux => RunnerManager::new(&config, &process).smoke_linux(),
        HostCommand::SmokeAndroid => RunnerManager::new(&config, &process).smoke_android(),
        HostCommand::RunMacosRunner => RunnerManager::new(&config, &process).run_macos_runner(),
        HostCommand::RunWindowsGuest { install } => {
            WindowsHost::new(&config, &process)?.start(if *install {
                Boot::Install
            } else {
                Boot::Installed
            })
        }
        HostCommand::Preflight => HostStorage::new(&config, &process)?.preflight(),
        HostCommand::Cleanup => {
            let deadline = config.host.cleanup_deadline();
            end_the_process_after(deadline, move || {
                error!(
                    deadline_seconds = deadline.as_secs(),
                    "cleanup pass outlived its deadline; ending it so the next one can start"
                );
                std::process::exit(1);
            });
            HostStorage::new(&config, &process)?.cleanup()
        }
        HostCommand::Health => HostStorage::new(&config, &process)?.health(),
    }
}

/// Ends the process once the pass outlives `limit`, from a thread the pass
/// cannot block.
///
/// The cleanup agent on this host hung for over a day inside `opendir` on a
/// volume that had stopped answering, having done no work. `launchd` starts no
/// second instance while the first is alive, so `StartInterval` stopped meaning
/// anything and cleanup was simply gone with nothing saying so. A check between
/// steps could not have ended it: the thread never reached the next step.
fn end_the_process_after(limit: Duration, expire: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        thread::sleep(limit);
        expire();
    });
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use clap::Parser;

    use super::*;
    use crate::Cli;

    /// The pass this replaces a between-steps check for: the cleanup agent on
    /// this host hung for over a day inside `opendir` on a volume that had
    /// stopped answering. A thread that never reaches its next step cannot be
    /// ended by a check placed between steps, so the deadline has to fire from
    /// somewhere the pass does not touch.
    #[test]
    fn the_deadline_fires_while_the_pass_is_parked() {
        let (expired, waiting) = mpsc::channel();

        end_the_process_after(Duration::from_millis(10), move || drop(expired.send(())));

        waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("the deadline never fired");
    }

    fn parse(command: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec![
            "xtask",
            "ci",
            "host",
            "--config",
            "/etc/kithara-ci/mac-host.toml",
        ];
        argv.extend_from_slice(command);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn host_maintenance_commands_are_typed() {
        for command in [
            ["bootstrap"].as_slice(),
            ["finish"].as_slice(),
            ["install-host-tools"].as_slice(),
            ["install-user-tools"].as_slice(),
            ["install-services"].as_slice(),
            ["activate-bridge"].as_slice(),
            ["configure-runners"].as_slice(),
            ["activate"].as_slice(),
            ["guest-prepare"].as_slice(),
            ["build-linux-image", "docker/ci.Dockerfile"].as_slice(),
            ["smoke-linux"].as_slice(),
            ["smoke-android"].as_slice(),
            ["run-macos-runner"].as_slice(),
            ["preflight"].as_slice(),
            ["cleanup"].as_slice(),
            ["health"].as_slice(),
        ] {
            assert!(parse(command).is_ok(), "{command:?} must parse");
        }
        assert!(parse(&["rm"]).is_err());
    }
}
