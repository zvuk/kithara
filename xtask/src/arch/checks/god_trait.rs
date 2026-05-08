//! Number of methods (`fn` items) in a single trait definition.
//!
//! Per AGENTS.md: "prefer several smaller traits with clearer ownership"
//! over one fat trait. A trait with many methods makes implementations
//! large, makes mocks/test-doubles painful, and signals weak interface
//! cohesion (the trait is doing several jobs at once).

use std::collections::BTreeMap;

use anyhow::Result;
use syn::{Item, ItemTrait, TraitItem};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "god_trait";

pub(crate) struct GodTrait;

impl Check for GodTrait {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.god_trait;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut traits: BTreeMap<String, usize> = BTreeMap::new();
            collect(&file.items, &mut traits);

            for (name, count) in &traits {
                if *count >= cfg.warn {
                    let key = format!("{rel}::{name}");
                    let msg = format!(
                        "{name}: {count} methods (warn threshold {}); split into smaller \
                         single-purpose traits",
                        cfg.warn
                    );
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
}

fn collect(items: &[Item], out: &mut BTreeMap<String, usize>) {
    for item in items {
        match item {
            Item::Trait(t) => {
                out.insert(t.ident.to_string(), count_methods(t));
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn count_methods(t: &ItemTrait) -> usize {
    t.items
        .iter()
        .filter(|i| matches!(i, TraitItem::Fn(_)))
        .count()
}
