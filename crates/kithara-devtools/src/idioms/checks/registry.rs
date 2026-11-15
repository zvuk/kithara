use std::path::Path;

use anyhow::Result;
use cargo_metadata::Metadata;

use super::{
    super::config::IdiomsConfig, accumulator_loops, arc_mutex_collection, await_under_guard,
    box_concrete_type, branch_chains, const_group_enum_shape, derivable_built_default,
    derivable_clone, derivable_control, derivable_control_painter, derivable_debug,
    derivable_default, derivable_delegation, derivable_deref, derivable_display,
    derivable_enum_str, derivable_error, derivable_event, derivable_from, derivable_getter,
    derivable_into_probe_arg, derivable_mirror, derivable_patch, derivable_phase, derivable_ranged,
    derivable_serialize, derivable_skin_walk, derivable_variants, fat_loop_body,
    function_branch_density, guard_cascade, loop_allocation, loop_flag_accumulator,
    manual_question_mark, multi_accumulator_loop, nested_if_let_pyramid, no_passthrough_builder,
    parallel_loops, pointwise_loop, retry_fallback, thin_wrapper_economy,
};
use crate::common::{fix::FixOutcome, scan::Scan, scope::Scope, violation::Violation};

pub(crate) struct Context<'a> {
    pub(crate) config: &'a IdiomsConfig,
    pub(crate) metadata: &'a Metadata,
    pub(crate) workspace_root: &'a Path,
    pub(crate) scan: &'a Scan,
    pub(crate) scope: &'a Scope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckPolicy {
    Default,
    WorkspaceSources,
}

impl CheckPolicy {
    pub(crate) const fn keeps_source_findings(self) -> bool {
        matches!(self, Self::WorkspaceSources)
    }

    pub(crate) fn scope(self, scope: &Scope) -> Scope {
        match self {
            Self::Default => scope.clone(),
            Self::WorkspaceSources => scope.clone().with_workspace_sources(),
        }
    }
}

pub(crate) trait Check: Sync {
    /// Whether this check's findings about a file depend on that file alone.
    ///
    /// Most checks read one file to judge it, so the driver runs a check over
    /// one file at a time and keeps each verdict beside the digest of the
    /// bytes it judged: an unchanged file is not read, parsed or analysed
    /// again. The check itself runs as it always does, over a scan that holds
    /// the one file.
    ///
    /// A check that correlates files answers `false`: whatever it reads
    /// beyond the file - another file, the crate graph, a tool - is in no key,
    /// so a change there would leave a kept verdict standing. The tests below
    /// run every check that keeps this answer over a directory and again over
    /// each of its files alone, and the two must agree.
    fn caches_by_file(&self) -> bool {
        true
    }
    fn fix(&self, _ctx: &Context<'_>) -> Result<FixOutcome> {
        Ok(FixOutcome::default())
    }
    fn id(&self) -> &'static str;
    fn policy(&self) -> CheckPolicy {
        if self.id().starts_with("derivable_") {
            CheckPolicy::WorkspaceSources
        } else {
            CheckPolicy::Default
        }
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(branch_chains::BranchChains),
        Box::new(guard_cascade::GuardCascade),
        Box::new(derivable_delegation::DerivableDelegation),
        Box::new(derivable_clone::DerivableClone),
        Box::new(derivable_control::DerivableControl),
        Box::new(derivable_control_painter::DerivableControlPainter),
        Box::new(derivable_built_default::DerivableBuiltDefault),
        Box::new(derivable_debug::DerivableDebug),
        Box::new(derivable_default::DerivableDefault),
        Box::new(derivable_from::DerivableFrom),
        Box::new(derivable_ranged::DerivableRanged),
        Box::new(derivable_serialize::DerivableSerialize),
        Box::new(derivable_skin_walk::DerivableSkinWalk),
        Box::new(derivable_patch::DerivablePatch),
        Box::new(derivable_phase::DerivablePhase),
        Box::new(derivable_deref::DerivableDeref),
        Box::new(derivable_display::DerivableDisplay),
        Box::new(derivable_error::DerivableError),
        Box::new(derivable_event::DerivableEvent),
        Box::new(derivable_getter::DerivableGetter),
        Box::new(derivable_into_probe_arg::DerivableIntoProbeArg),
        Box::new(derivable_mirror::DerivableMirror),
        Box::new(derivable_enum_str::DerivableEnumStr),
        Box::new(derivable_variants::DerivableVariants),
        Box::new(accumulator_loops::AccumulatorLoops),
        Box::new(multi_accumulator_loop::MultiAccumulatorLoop),
        Box::new(parallel_loops::ParallelLoops),
        Box::new(pointwise_loop::PointwiseLoop),
        Box::new(manual_question_mark::ManualQuestionMark),
        Box::new(loop_allocation::LoopAllocation),
        Box::new(box_concrete_type::BoxConcreteType),
        Box::new(arc_mutex_collection::ArcMutexCollection),
        Box::new(await_under_guard::AwaitUnderGuard),
        Box::new(function_branch_density::FunctionBranchDensity),
        Box::new(retry_fallback::RetryFallback),
        Box::new(fat_loop_body::FatLoopBody),
        Box::new(loop_flag_accumulator::LoopFlagAccumulator),
        Box::new(const_group_enum_shape::ConstGroupEnumShape),
        Box::new(nested_if_let_pyramid::NestedIfLetPyramid),
        Box::new(no_passthrough_builder::NoPassthroughBuilder),
        Box::new(thin_wrapper_economy::ThinWrapperEconomy),
    ]
}

#[cfg(test)]
mod per_file_declaration_tests {
    use anyhow::Result;
    use cargo_metadata::{Metadata, MetadataCommand};
    use rayon::prelude::*;

    use super::{Check, Context, registry};
    use crate::{
        common::{scan::Scan, scope::Scope, violation::Violation},
        idioms::config::IdiomsConfig,
    };

    /// The keys `check` finds over the whole scope, and the keys it finds
    /// over each file of the scope alone, each sorted.
    fn both_ways(check: &dyn Check, ctx: &Context<'_>) -> (Vec<String>, Vec<String>) {
        let keys = |violations: Vec<Violation>| {
            violations
                .into_iter()
                .map(|violation| violation.key)
                .collect::<Vec<_>>()
        };
        let mut together = keys(check.run(ctx).expect("a whole-scope run"));
        let mut apart = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope).expect("walk the scope").iter() {
            let view = ctx.scan.for_file(path);
            apart.extend(keys(
                check
                    .run(&Context {
                        scan: &view,
                        ..*ctx
                    })
                    .expect("a single-file run"),
            ));
        }
        together.sort();
        apart.sort();
        (together, apart)
    }

    /// A check that reports the first file of its scope only when another
    /// file is beside it, so its verdict depends on what else was scanned.
    struct Neighbourly;

    impl Check for Neighbourly {
        fn id(&self) -> &'static str {
            "neighbourly"
        }

        fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
            let files = ctx.scan.rs_files(ctx.scope)?;
            Ok(files
                .get(1)
                .map(|_| {
                    Violation::warn(self.id(), files[0].display().to_string(), "has a neighbour")
                })
                .into_iter()
                .collect())
        }
    }

    /// The workspace, its idioms configuration, and a scope over the checks'
    /// own sources, which are dense with the shapes the checks look for.
    fn fixture() -> (Metadata, IdiomsConfig, Scope) {
        let metadata = MetadataCommand::new().exec().expect("workspace metadata");
        let config = IdiomsConfig::load(
            &metadata
                .workspace_root
                .as_std_path()
                .join(".config")
                .join("idioms"),
        )
        .expect("idioms config");
        let checks = crate::testing::package()
            .join("src")
            .join("idioms")
            .join("checks");
        (metadata, config, Scope::new(Vec::new(), vec![checks]))
    }

    /// The declaration says a check's verdict about a file depends on that
    /// file alone. Here the claim is exercised rather than trusted: a check
    /// that correlates files loses the correlation file by file and differs.
    #[test]
    fn a_declared_per_file_check_finds_the_same_in_each_file_alone() {
        let (metadata, config, scope) = fixture();
        let workspace_root = metadata.workspace_root.as_std_path();
        let scan = Scan::new(workspace_root);
        let ctx = Context {
            config: &config,
            metadata: &metadata,
            workspace_root,
            scan: &scan,
            scope: &scope,
        };

        let judged: Vec<(&'static str, usize, bool)> = registry()
            .par_iter()
            .filter(|check| check.caches_by_file())
            .map(|check| {
                let effective = check.policy().scope(&scope);
                let (together, apart) = both_ways(
                    check.as_ref(),
                    &Context {
                        scope: &effective,
                        ..ctx
                    },
                );
                (check.id(), together.len(), together == apart)
            })
            .collect();

        assert!(
            judged.iter().map(|(_, found, _)| found).sum::<usize>() > 0,
            "the scope produced no findings, so agreeing about it proves nothing"
        );
        let disagreeing: Vec<&str> = judged
            .iter()
            .filter(|(_, _, agreed)| !agreed)
            .map(|(id, _, _)| *id)
            .collect();
        assert!(
            disagreeing.is_empty(),
            "these checks declare per-file verdicts but correlate files: {disagreeing:?}"
        );
    }

    /// The comparison above must be able to fail.
    #[test]
    fn a_check_that_reads_its_neighbours_is_caught() {
        let (metadata, config, scope) = fixture();
        let workspace_root = metadata.workspace_root.as_std_path();
        let scan = Scan::new(workspace_root);
        let ctx = Context {
            config: &config,
            metadata: &metadata,
            workspace_root,
            scan: &scan,
            scope: &scope,
        };

        let (together, apart) = both_ways(&Neighbourly, &ctx);

        assert_eq!(together.len(), 1);
        assert!(apart.is_empty(), "alone, no file has a neighbour");
    }
}
