//! Flags files with many `#[cfg(...)]` attributes on top-level items.
//!
//! High cfg density signals that the file should be split into
//! platform/feature-specific modules with a single `#[cfg]` gate on the
//! `mod` declaration instead of sprinkling gates across every item.

use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "cfg_density";

pub(crate) struct CfgDensity;

impl Check for CfgDensity {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.cfg_density;
        let exclude = compile_globs(&cfg.exclude_globs);
        let exempt_crates: Vec<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exclude, rel) {
                continue;
            }
            if is_exempt_crate(rel, &exempt_crates) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            let count = count_cfg_attributes(&content);
            let key = rel.to_string_lossy().replace('\\', "/");

            if count >= cfg.deny {
                violations.push(
                    Violation::deny(
                        ID,
                        &key,
                        format!("{count} #[cfg] attributes (deny threshold {})", cfg.deny),
                    )
                    .with_explanation(EXPLANATION),
                );
            } else if count >= cfg.warn {
                violations.push(
                    Violation::warn(
                        ID,
                        &key,
                        format!("{count} #[cfg] attributes (warn threshold {})", cfg.warn),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

const EXPLANATION: &str = "\
Summary: Too many `#[cfg(...)]` gates scattered across individual items.

Why: Repeated cfg attributes are noisy, error-prone (easy to forget one
branch), and make the file hard to read. Grouping gated code into
dedicated modules with a single `#[cfg]` on the `mod` declaration is
cleaner and more maintainable.

Bad:
    #[cfg(not(target_arch = \"wasm32\"))]
    use std::env;
    #[cfg(not(target_arch = \"wasm32\"))]
    fn native_only() { ... }
    #[cfg(target_arch = \"wasm32\")]
    fn wasm_only() { ... }

Good:
    #[cfg(not(target_arch = \"wasm32\"))]
    mod native;
    #[cfg(target_arch = \"wasm32\")]
    mod wasm;

Suppress: add the file to `[cfg_density] exclude_globs` in
`.config/arch/thresholds.toml`, or the crate to `exempt_crates`.";

fn count_cfg_attributes(source: &str) -> usize {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(")
        })
        .count()
}

fn is_exempt_crate(rel: &std::path::Path, exempt: &[&str]) -> bool {
    let mut components = rel.components();
    if components.next().and_then(|c| c.as_os_str().to_str()) != Some("crates") {
        return false;
    }
    let Some(crate_dir) = components.next().and_then(|c| c.as_os_str().to_str()) else {
        return false;
    };
    exempt.contains(&crate_dir)
}
