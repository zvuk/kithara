use std::{fs, io::Write as _, path::PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use kithara_devtools::Ctx;
use serde_json::json;

use super::{
    catalog::Catalog,
    frontend::{self, Deployment},
    legacy, prepare_view,
    request::{PlanRequest, PlanningPolicy},
    require_policy_checkout,
    resolve::resolve,
    wire,
};
use crate::{ci::lane::select::Fleet, config::KitharaExt};

#[derive(Debug, Args)]
pub(crate) struct PlanArgs {
    #[arg(long, conflicts_with_all = ["request", "policy", "legacy", "project"])]
    catalog: bool,
    #[arg(long, conflicts_with_all = ["request", "policy", "catalog", "project"])]
    legacy: bool,
    /// Produce the canonical view for this operation from the protected policy.
    #[arg(long, requires = "consumer_source_pin", conflicts_with = "project")]
    view_operation: Option<PathBuf>,
    /// Project a fleet's admissions from canonical view bytes.
    #[arg(long, requires_all = ["view_sha256", "consumer_source_pin", "fleet"], conflicts_with_all = ["request", "policy"])]
    project: Option<PathBuf>,
    #[arg(long, requires = "project")]
    view_sha256: Option<String>,
    #[arg(long, requires = "project")]
    fleet: Option<Fleet>,
    #[arg(long, requires = "project")]
    deployment: Option<PathBuf>,
    #[arg(long)]
    consumer_source_pin: Option<String>,
    #[arg(long, required_unless_present_any = ["catalog", "legacy", "project"])]
    request: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["catalog", "legacy", "project"])]
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
    if let Some(path) = &args.project {
        let pins = wire::ReadPins {
            schema_digest: wire::SCHEMA_DIGEST,
            consumer_source_pin: args.consumer_source_pin.as_deref().context("source pin")?,
        };
        let view = wire::decode(
            &fs::read(path)?,
            args.view_sha256.as_deref().context("view digest")?,
            &pins,
        )?;
        let deployment: Option<Deployment> = match &args.deployment {
            Some(path) => Some(serde_json::from_slice(&fs::read(path)?)?),
            None => None,
        };
        let jobs = frontend::project(
            &view,
            &catalog,
            &ext.ci,
            deployment.as_ref(),
            args.fleet.context("fleet")?,
        )?;
        println!("{}", serde_json::to_string(&jobs)?);
        return Ok(());
    }
    if args.catalog {
        let output = json!({"catalog_digest":catalog.fingerprint()?,"catalog":catalog});
        println!("{}", serde_json::to_string(&output)?);
        return Ok(());
    }
    let request = args.request.as_ref().context("plan requires --request")?;
    let policy = args.policy.as_ref().context("plan requires --policy")?;
    let request: PlanRequest = serde_json::from_slice(&fs::read(request)?)?;
    let policy: PlanningPolicy = serde_json::from_slice(&fs::read(policy)?)?;
    if let Some(operation) = &args.view_operation {
        require_policy_checkout(&ctx.root, &policy.revision)?;
        let operation: wire::Operation = serde_json::from_slice(&fs::read(operation)?)?;
        let pin = args.consumer_source_pin.as_deref().context("source pin")?;
        let view = prepare_view(ctx, request, &policy, &operation, pin)?;
        std::io::stdout().write_all(&wire::canonical_json(&view)?)?;
        return Ok(());
    }
    let plan = resolve(&catalog, &ext.ci, request, &policy)?;
    println!("{}", serde_json::to_string(&plan)?);
    Ok(())
}
