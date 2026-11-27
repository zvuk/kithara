use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use kithara_devtools::Ctx;
use serde_json::json;

use super::{
    catalog::Catalog,
    legacy,
    request::{PlanRequest, PlanningPolicy},
    resolve::resolve,
};
use crate::config::KitharaExt;

#[derive(Debug, Args)]
pub(crate) struct PlanArgs {
    #[arg(long, conflicts_with_all = ["request", "policy", "legacy"])]
    catalog: bool,
    #[arg(long, conflicts_with_all = ["request", "policy", "catalog"])]
    legacy: bool,
    #[arg(long, required_unless_present_any = ["catalog", "legacy"])]
    request: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["catalog", "legacy"])]
    policy: Option<PathBuf>,
}

pub(crate) fn run(args: &PlanArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    if args.legacy {
        println!(
            "{}",
            serde_json::to_string(&legacy::fingerprints(&ext.ci)?)?
        );
        return Ok(());
    }
    let catalog = Catalog::load(&ctx.root, &ext.ci)?;
    let output = if args.catalog {
        json!({"catalog_digest":catalog.fingerprint()?,"catalog":catalog})
    } else {
        let request = args.request.as_ref().context("plan requires --request")?;
        let policy = args.policy.as_ref().context("plan requires --policy")?;
        let request: PlanRequest = serde_json::from_slice(&fs::read(request)?)?;
        let policy: PlanningPolicy = serde_json::from_slice(&fs::read(policy)?)?;
        serde_json::to_value(resolve(&catalog, &ext.ci, request, &policy)?)?
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
