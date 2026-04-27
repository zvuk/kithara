//! Detect files that mix multiple sizable entities (god-files in disguise).
//!
//! A type is "sizable" if its impl surface (`impl X` + `impl Trait for X`)
//! contains at least `min_fns_per_type` methods. A file with several sizable
//! types signals that distinct entities should live in separate files.

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    parse::{parse_file, type_weights},
    violation::Violation,
    walker::{relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "mixed_entities";

pub(crate) struct MixedEntities;

impl Check for MixedEntities {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.mixed_entities;
        let mut violations = Vec::new();

        for path in workspace_rs_files(ctx.workspace_root)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let weights = type_weights(&file);
            let mut sizable: Vec<(String, usize, usize)> = weights
                .into_iter()
                .filter_map(|(name, w)| {
                    let by_fns = w.impl_fns >= cfg.min_fns_per_type;
                    let by_blocks = w.impl_blocks >= cfg.min_impl_blocks;
                    (by_fns || by_blocks).then_some((name, w.impl_fns, w.impl_blocks))
                })
                .collect();
            sizable.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));

            if sizable.len() < cfg.warn {
                continue;
            }
            let key = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            let summary = sizable
                .iter()
                .map(|(n, fns, blocks)| format!("{n}({fns} fns, {blocks} impls)"))
                .collect::<Vec<_>>()
                .join(", ");
            let count = sizable.len();
            let msg = format!(
                "{count} sizable types share one file (>={} fns OR >={} impl-blocks each): {summary}; \
                 split into separate modules",
                cfg.min_fns_per_type, cfg.min_impl_blocks
            );
            if count >= cfg.deny {
                violations.push(Violation::deny(ID, key, msg));
            } else {
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        Ok(violations)
    }
}
