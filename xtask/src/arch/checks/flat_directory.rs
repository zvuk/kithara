//! Subsystem-extraction hint: many sibling `.rs` files in one directory.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use super::{Check, Context};
use crate::arch::{
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "flat_directory";

pub(crate) struct FlatDirectory;

impl Check for FlatDirectory {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.flat_directory;
        let ignore = compile_globs(&cfg.ignore_globs);

        // group rs files by parent dir, excluding mod.rs
        let mut by_dir: BTreeMap<PathBuf, usize> = BTreeMap::new();
        for path in workspace_rs_files(ctx.workspace_root)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&ignore, rel) {
                continue;
            }
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name == "mod.rs" {
                continue;
            }
            if let Some(parent) = path.parent() {
                let parent_rel = relative_to(ctx.workspace_root, parent).to_path_buf();
                *by_dir.entry(parent_rel).or_insert(0) += 1;
            }
        }

        let mut violations = Vec::new();
        for (dir, count) in by_dir {
            let key = dir.to_string_lossy().replace('\\', "/");
            if count >= cfg.deny {
                violations.push(Violation::deny(
                    ID,
                    key,
                    format!(
                        "{count} sibling .rs files in one directory (deny threshold {}); \
                         consider extracting subsystem into a submodule",
                        cfg.deny
                    ),
                ));
            } else if count >= cfg.warn {
                violations.push(Violation::warn(
                    ID,
                    key,
                    format!(
                        "{count} sibling .rs files in one directory (warn threshold {}); \
                         consider extracting subsystem into a submodule",
                        cfg.warn
                    ),
                ));
            }
        }
        Ok(violations)
    }
}
