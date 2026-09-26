use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    exclude::cfg_test_lines,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "cfg_density";

    pub(super) const EXPLANATION: &str = "\
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

Resolve: move gated production items into dedicated platform or feature
modules and gate each module once. Test-only item ranges are excluded
automatically.";
}

pub(crate) struct CfgDensity;

impl Check for CfgDensity {
    fn id(&self) -> &'static str {
        consts::ID
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
                        consts::ID,
                        &key,
                        format!("{count} #[cfg] attributes (deny threshold {})", cfg.deny),
                    )
                    .with_explanation(consts::EXPLANATION),
                );
            } else if count >= cfg.warn {
                violations.push(
                    Violation::warn(
                        consts::ID,
                        &key,
                        format!("{count} #[cfg] attributes (warn threshold {})", cfg.warn),
                    )
                    .with_explanation(consts::EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

fn count_cfg_attributes(source: &str) -> usize {
    let test_lines = cfg_test_lines(source);

    source
        .lines()
        .enumerate()
        .filter(|(index, line)| {
            let trimmed = line.trim_start();
            let is_cfg = trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(");
            let line_number = index + 1;
            is_cfg
                && test_lines
                    .as_ref()
                    .is_none_or(|excluded| !excluded.contains(&line_number))
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

#[cfg(test)]
mod tests {
    use super::count_cfg_attributes;

    #[test]
    fn cfg_density_ignores_test_only_item_ranges() {
        let source = r#"
#[cfg(target_arch = "wasm32")]
fn production() {}

#[cfg(test)]
mod tests {
    #[cfg(feature = "fixture-a")]
    fn fixture_a() {}

    #[cfg_attr(feature = "fixture-b", ignore)]
    fn fixture_b() {}
}
"#;

        assert_eq!(count_cfg_attributes(source), 1);
    }

    #[test]
    fn cfg_density_keeps_non_test_predicates() {
        let source = r#"
#[cfg(not(test))]
fn non_test_build() {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native() {}

#[cfg_attr(feature = "trace", derive(Debug))]
struct Trace;
"#;

        assert_eq!(count_cfg_attributes(source), 3);
    }

    #[test]
    fn cfg_density_falls_back_to_raw_count_for_invalid_rust() {
        let source = "#[cfg(unix)]\nfn broken( {\n#[cfg_attr(test, ignore)]\n";

        assert_eq!(count_cfg_attributes(source), 2);
    }
}
