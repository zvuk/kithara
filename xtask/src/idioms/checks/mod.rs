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
use crate::common::{scope::Scope, violation::Violation};

pub(crate) mod accumulator_loops;
pub(crate) mod box_concrete_type;
pub(crate) mod branch_chains;
pub(crate) mod guard_cascade;
pub(crate) mod loop_allocation;
pub(crate) mod manual_question_mark;
pub(crate) mod multi_accumulator_loop;
pub(crate) mod parallel_loops;

#[expect(dead_code, reason = "fields consumed by upcoming idiom checks")]
pub(crate) struct Context<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) metadata: &'a Metadata,
    pub(crate) config: &'a IdiomsConfig,
    pub(crate) scope: &'a Scope,
}

pub(crate) trait Check {
    fn id(&self) -> &'static str;
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(branch_chains::BranchChains),
        Box::new(guard_cascade::GuardCascade),
        Box::new(accumulator_loops::AccumulatorLoops),
        Box::new(multi_accumulator_loop::MultiAccumulatorLoop),
        Box::new(parallel_loops::ParallelLoops),
        Box::new(manual_question_mark::ManualQuestionMark),
        Box::new(loop_allocation::LoopAllocation),
        Box::new(box_concrete_type::BoxConcreteType),
    ]
}
