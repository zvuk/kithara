//! Registry of idiom checks (constructions that hint at a better Rust pattern).
//!
//! `idioms` is the third static-analysis namespace alongside `arch` (topology)
//! and `style` (intra-file organisation). It flags constructions that compile
//! and pass clippy but are worth reconsidering for performance, readability,
//! or expressivity.

use std::path::Path;

use anyhow::Result;
use cargo_metadata::Metadata;

use super::config::IdiomsConfig;
use crate::common::violation::Violation;

pub(crate) mod branch_chains;

#[expect(dead_code, reason = "fields consumed by upcoming idiom checks")]
pub(crate) struct Context<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) metadata: &'a Metadata,
    pub(crate) config: &'a IdiomsConfig,
}

pub(crate) trait Check {
    fn id(&self) -> &'static str;
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![Box::new(branch_chains::BranchChains)]
}
