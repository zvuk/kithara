use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};

use super::{linux::command::LinuxArgs, mac::command::MacArgs, provision};
use crate::{ci::process::Process, consts};

/// One CI machine, addressed by the platform it serves.
///
/// Both machines answer the same questions — which images they run, which
/// runners they register, which agents they keep alive — so they share a
/// command and differ in the platform that owns the answers.
#[derive(Debug, Args)]
pub(crate) struct HostArgs {
    #[command(subcommand)]
    platform: HostPlatform,
}

#[derive(Debug, Subcommand)]
enum HostPlatform {
    /// The macOS machine: its Apple lanes, its Linux container and its guests.
    Mac(MacArgs),
    /// A Linux machine and the runners it serves.
    Linux(LinuxArgs),
    /// Bring this machine up to what the current commit describes: its images,
    /// its runners and its services. The platform is the one it runs on.
    Provision(ProvisionArgs),
}

/// A machine provisions only itself, so the profile it reads is the local one.
#[derive(Debug, Args)]
pub(crate) struct ProvisionArgs {
    /// Machine profile of this CI host, provisioned outside the repository.
    /// A Linux machine keeps it at a known path and needs no flag.
    #[arg(long, env = "KITHARA_CI_HOST_CONFIG")]
    config: Option<PathBuf>,
    /// Reviewed build pins tracked in the repository.
    #[arg(long, env = "KITHARA_CI_PINS", default_value = consts::PINS_PATH)]
    pins: PathBuf,
}

pub(crate) fn run(args: &HostArgs) -> Result<()> {
    match &args.platform {
        HostPlatform::Mac(args) => super::mac::run(args),
        HostPlatform::Linux(args) => super::linux::run(args),
        HostPlatform::Provision(args) => {
            let process = Process::new(&std::env::current_dir()?, BTreeMap::new());
            provision::run(&process, args.config.as_deref(), &args.pins)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::Cli;

    /// A machine provisions itself with no platform in the command line: the
    /// pipeline that runs this serves both hosts from one job definition, and
    /// a platform spelled there would be a second place to keep in step.
    #[test]
    fn a_machine_provisions_itself_without_naming_its_platform() {
        Cli::try_parse_from(["xtask", "ci", "host", "provision"])
            .expect("provision takes no flags");
        Cli::try_parse_from([
            "xtask",
            "ci",
            "host",
            "provision",
            "--config",
            "/etc/kithara-ci/linux-host.toml",
        ])
        .expect("a machine may name its profile");
    }
}
