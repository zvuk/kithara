use std::{
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args;
use kithara_xtask_core::{common::project::ProjectConfig, util::check_tool};

struct Consts;
impl Consts {
    const INSTALL_HINT: &'static str = "cargo install cargo-modules";

    const PARALLELISM: usize = 4;
}

#[derive(Debug, Args)]
pub(crate) struct OrphansArgs {
    /// Limit to specific packages. Repeatable. Empty = all non-excluded
    /// workspace packages.
    #[arg(long = "package", short = 'p', value_name = "NAME")]
    pub packages: Vec<String>,
    /// Treat orphans as a hard failure (exit non-zero). Without it the
    /// run is advisory: orphans are printed but exit is success.
    #[arg(long)]
    pub deny: bool,
    /// In audit mode, skip the run when no packages are given (avoids
    /// the ~90s workspace sweep on every pre-commit `just audit`).
    /// `just orphans` and `just health` leave this off.
    #[arg(long = "audit-mode")]
    pub audit_mode: bool,
}

pub(crate) fn run(args: &OrphansArgs) -> Result<()> {
    check_tool(
        "cargo-modules",
        &["modules", "--version"],
        Consts::INSTALL_HINT,
    )?;

    let excluded = excluded_packages()?;
    let packages: Vec<String> = if args.packages.is_empty() {
        if args.audit_mode {
            println!(
                "orphans: workspace-wide run skipped in audit mode \
                 (run `just orphans` or `just health` for full sweep)"
            );
            return Ok(());
        }
        all_non_excluded(&excluded)?
    } else {
        let mut kept = Vec::new();
        let mut skipped = Vec::new();
        for pkg in &args.packages {
            if excluded.iter().any(|e| e == pkg) {
                skipped.push(pkg.clone());
            } else {
                kept.push(pkg.clone());
            }
        }
        if !skipped.is_empty() {
            println!(
                "orphans: skipping cfg-gated packages with known false-positives: {} \
                 (validate via target-specific builds)",
                skipped.join(", ")
            );
        }
        kept
    };

    if packages.is_empty() {
        println!("orphans: no packages in scope (skipped)");
        return Ok(());
    }

    println!(
        "orphans: checking {} package(s) with {}-way parallelism",
        packages.len(),
        Consts::PARALLELISM
    );

    let queue = Arc::new(Mutex::new(packages));
    let failed = Arc::new(Mutex::new(Vec::<String>::new()));
    let deny = args.deny;
    let mut handles = Vec::with_capacity(Consts::PARALLELISM);

    for _ in 0..Consts::PARALLELISM {
        let q = Arc::clone(&queue);
        let f = Arc::clone(&failed);
        handles.push(thread::spawn(move || {
            loop {
                let pkg = {
                    let mut g = q.lock().expect("orphans queue mutex poisoned");
                    g.pop()
                };
                let Some(pkg) = pkg else { break };
                let mut cmd = Command::new("cargo");
                cmd.arg("modules")
                    .arg("orphans")
                    .arg("--cfg-test")
                    .arg("--lib")
                    .arg("--package")
                    .arg(&pkg)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                if deny {
                    cmd.arg("--deny");
                }
                let ok = cmd.status().is_ok_and(|s| s.success());
                if !ok {
                    f.lock().expect("orphans failed mutex poisoned").push(pkg);
                }
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }

    let mut names: Vec<String> = {
        let guard = failed.lock().expect("orphans failed mutex poisoned");
        guard.clone()
    };
    if !names.is_empty() {
        names.sort();
        bail!(
            "orphans: {} package(s) reported orphans: {}",
            names.len(),
            names.join(", ")
        );
    }
    Ok(())
}

fn excluded_packages() -> Result<Vec<String>> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    let root = metadata.workspace_root.as_std_path();
    Ok(ProjectConfig::load(root)?.orphans.exclude_packages)
}

fn all_non_excluded(excluded: &[String]) -> Result<Vec<String>> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    let mut out: Vec<String> = metadata
        .workspace_packages()
        .into_iter()
        .map(|p| p.name.to_string())
        .filter(|n| !excluded.iter().any(|e| e == n))
        .collect();
    out.sort();
    Ok(out)
}
