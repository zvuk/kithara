//! Number of `pub`/`pub(crate)` items declared in one module file.
//!
//! High pub-surface per module signals that one file is exposing too many
//! independent abstractions. The companion shape that should drive a split
//! is "many small modules with single-purpose APIs", not "one fat module".
//!
//! Counted item kinds: struct, enum, trait, fn, type alias, const, static,
//! `macro_rules`. Inline `mod foo { ... }` blocks declared inside the file
//! emit *separate* counts (their items belong to the inner module, not the
//! outer one), but only the file-level scope produces a violation key
//! when its own count exceeds the threshold.
//!
//! Per-crate overrides (`[god_module.overrides]`) let app/test/macro crates
//! relax the default threshold without having to baseline each file.

use std::path::Path;

use anyhow::Result;
use syn::{File, Item};

use super::{Check, Context};
use crate::common::{
    parse::{is_pub_visibility, parse_file},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "god_module";

pub(crate) struct GodModule;

impl Check for GodModule {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.god_module;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path);
            let key = rel.to_string_lossy().replace('\\', "/");
            let crate_name = crate_name_from_path(rel);
            let warn = crate_name
                .as_deref()
                .and_then(|c| cfg.overrides.get(c))
                .copied()
                .unwrap_or(cfg.warn);

            let count = count_pub_items_top_level(&file);
            if count >= warn {
                violations.push(Violation::warn(
                    ID,
                    key,
                    format!("{count} pub/pub(crate) items in one module (warn threshold {warn})"),
                ));
            }
        }
        Ok(violations)
    }
}

fn crate_name_from_path(rel: &Path) -> Option<String> {
    let mut comps = rel.components();
    let first = comps.next()?.as_os_str().to_str()?;
    if first != "crates" {
        return None;
    }
    let crate_dir = comps.next()?.as_os_str().to_str()?;
    Some(crate_dir.to_string())
}

fn count_pub_items_top_level(file: &File) -> usize {
    file.items.iter().filter(|i| is_pub_item(i)).count()
}

fn is_pub_item(item: &Item) -> bool {
    match item {
        Item::Struct(s) => is_pub_visibility(&s.vis),
        Item::Enum(e) => is_pub_visibility(&e.vis),
        Item::Trait(t) => is_pub_visibility(&t.vis),
        Item::Fn(f) => is_pub_visibility(&f.vis),
        Item::Type(t) => is_pub_visibility(&t.vis),
        Item::Const(c) => is_pub_visibility(&c.vis),
        Item::Static(s) => is_pub_visibility(&s.vis),
        Item::Macro(m) => m.ident.is_some(),
        _ => false,
    }
}
