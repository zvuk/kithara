//! Path depth under `crates/<crate>/src/` must not exceed `max_depth`.

use std::path::{Component, Path};

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "max_nesting";

pub(crate) struct MaxNesting;

impl Check for MaxNesting {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.max_nesting;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            let Some(crate_name) = crate_name_from(rel) else {
                continue;
            };
            if cfg.exempt_crates.iter().any(|c| c == crate_name) {
                continue;
            }
            let Some(depth) = depth_under_src(rel) else {
                continue;
            };
            if depth > cfg.max_depth {
                let key = rel.to_string_lossy().replace('\\', "/");
                violations.push(Violation::deny(
                    ID,
                    key,
                    format!(
                        "module nesting depth {depth} under src/ exceeds max {}",
                        cfg.max_depth
                    ),
                ));
            }
        }
        Ok(violations)
    }
}

fn crate_name_from(rel: &Path) -> Option<&str> {
    let mut comps = rel.components();
    if let Some(Component::Normal(n)) = comps.next()
        && n.to_str() == Some("crates")
        && let Some(Component::Normal(c)) = comps.next()
    {
        return c.to_str();
    }
    None
}

/// Depth of file relative to `crates/<crate>/src/`. `src/foo/bar/baz.rs` → 2.
fn depth_under_src(rel: &Path) -> Option<usize> {
    let mut comps = rel.components();
    let crates = comps.next()?;
    let _crate = comps.next()?;
    let src = comps.next()?;
    if matches!(crates, Component::Normal(n) if n == "crates")
        && matches!(src, Component::Normal(n) if n == "src")
    {
        let remaining: Vec<_> = comps.collect();
        // remaining = [...dirs, file.rs]; depth = dirs.len() = remaining.len() - 1
        return Some(remaining.len().saturating_sub(1));
    }
    None
}
