//! God-impl detection: high `fn`-to-`type` ratio inside one file.

use anyhow::Result;

use super::{Check, Context};
use crate::arch::{
    parse::{count_items, parse_file},
    violation::Violation,
    walker::{relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "file_density";

pub(crate) struct FileDensity;

impl Check for FileDensity {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.file_density;
        let mut violations = Vec::new();

        for path in workspace_rs_files(ctx.workspace_root)? {
            // skip files that fail to parse — file_size will catch giant blobs anyway
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let stats = count_items(&file);
            if stats.fns < cfg.min_fns_to_evaluate {
                continue;
            }
            let denom = u32::try_from(stats.types.max(1)).unwrap_or(u32::MAX);
            let numer = u32::try_from(stats.fns).unwrap_or(u32::MAX);
            let ratio = f64::from(numer) / f64::from(denom);
            let key = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            let msg = format!(
                "{} fns / {} types (ratio {:.1})",
                stats.fns, stats.types, ratio
            );
            if ratio >= cfg.deny_fns_per_type {
                violations.push(Violation::deny(ID, key, msg));
            } else if ratio >= cfg.warn_fns_per_type {
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        Ok(violations)
    }
}
