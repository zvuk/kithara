//! LOC threshold per `.rs` file.

use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "file_size";

pub(crate) struct FileSize;

impl Check for FileSize {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.file_size;
        let exclude = compile_globs(&cfg.exclude_globs);
        let mut violations = Vec::new();

        for path in workspace_rs_files(ctx.workspace_root)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exclude, rel) {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            let lines = content.lines().count();
            let key = rel.to_string_lossy().replace('\\', "/");
            if lines >= cfg.deny {
                violations.push(Violation::deny(
                    ID,
                    key,
                    format!("{lines} lines (deny threshold {})", cfg.deny),
                ));
            } else if lines >= cfg.warn {
                violations.push(Violation::warn(
                    ID,
                    key,
                    format!("{lines} lines (warn threshold {})", cfg.warn),
                ));
            }
        }
        Ok(violations)
    }
}
