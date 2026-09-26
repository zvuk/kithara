use anyhow::Result;
use tracing::info;

use super::profile::LinuxHost;
use crate::{ci::process::Process, consts};

/// Keep the runners' subnet away from the host and from its neighbours.
///
/// Rules are restored on every start rather than saved once: `ufw` and
/// `iptables-persistent` conflict, and a machine that already runs one of them
/// should not be made to choose. Each rule is checked before it is added, so
/// repeated starts do not stack duplicates.
pub(super) fn apply(process: &Process, host: &LinuxHost) -> Result<()> {
    for destination in consts::PRIVATE_BLOCKS {
        ensure(
            process,
            &[
                "DOCKER-USER",
                "-s",
                &host.subnet,
                "-d",
                destination,
                "-j",
                "DROP",
            ],
        )?;
    }
    // Traffic aimed at the host itself arrives on INPUT rather than FORWARD, so
    // the rules above never see it.
    ensure(process, &["INPUT", "-s", &host.subnet, "-j", "DROP"])?;
    info!(subnet = host.subnet, "runner subnet fenced");
    Ok(())
}

fn ensure(process: &Process, rule: &[&str]) -> Result<()> {
    let mut check = process.command("iptables");
    check.arg("-C").args(rule);
    if check.output()?.status.success() {
        return Ok(());
    }
    let mut insert = process.command("iptables");
    insert.arg("-I").arg(rule[0]).arg("1").args(&rule[1..]);
    process.run_command(&mut insert, "fence the runner subnet")
}
