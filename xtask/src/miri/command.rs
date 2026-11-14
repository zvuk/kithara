use std::{collections::BTreeSet, process::Command};

use anyhow::{Context, Result, ensure};
use clap::Args;
use kithara_devtools::{Ctx, verdict::ChildFailure};
use tracing::info;

use super::selection;
use crate::config::KitharaExt;

#[derive(Debug, Args)]
pub(crate) struct MiriArgs {
    /// Check package ownership of every Miri-marked test without running Miri.
    #[arg(long)]
    check: bool,
    /// Arguments forwarded unchanged to Miri's nextest runner.
    #[arg(last = true)]
    args: Vec<String>,
}

fn packages(ctx: &Ctx) -> Result<Vec<String>> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let lane = ext
        .ci
        .lanes
        .get("deep-miri")
        .context("missing deep-miri lane")?;
    let packages = &lane.test_packages;
    ensure!(
        !packages.is_empty(),
        "deep-miri.test_packages must not be empty"
    );
    ensure!(
        packages.iter().collect::<BTreeSet<_>>().len() == packages.len(),
        "deep-miri.test_packages contains duplicates"
    );
    for package in packages {
        ensure!(
            ctx.metadata()?
                .workspace_packages()
                .iter()
                .any(|p| p.name.as_str() == package.as_str()),
            "deep-miri.test_packages names unknown package `{package}`"
        );
    }
    Ok(packages.clone())
}

pub(crate) fn check(ctx: &Ctx) -> Result<()> {
    selection::check(ctx, &packages(ctx)?)
}

pub(crate) fn run(args: &MiriArgs, ctx: &Ctx) -> Result<()> {
    if args.check {
        ensure!(
            args.args.is_empty(),
            "--check does not accept runner arguments"
        );
        return check(ctx);
    }
    let mut command = Command::new(ctx.config.tools.program("cargo"));
    command
        .current_dir(&ctx.root)
        .env("MIRIFLAGS", "-Zmiri-disable-isolation")
        .args([
            "+nightly", "miri", "nextest", "run", "--profile", "miri", "--lib",
            "--no-default-features", "--features", "kithara-platform/tokio-rt-multi-thread",
            "--features",
            "kithara-queue/symphonia,kithara-queue/stretch-glide,kithara-queue/client-reqwest,kithara-queue/tls-rustls",
        ]);
    for package in packages(ctx)? {
        command.args(["-p", &package]);
    }
    command.args(&args.args);
    info!(?command, "running the declared Miri selection");
    let status = command.status().context("run Miri nextest")?;
    if !status.success() {
        return Err(ChildFailure::inherited(
            "Miri nextest".to_owned(),
            status.code(),
        ));
    }
    Ok(())
}
