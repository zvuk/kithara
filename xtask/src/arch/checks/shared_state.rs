//! Concentration of `Arc<Mutex/RwLock>` (and friends) per file.
//!
//! Counts substring occurrences after stripping `//` comments; type aliases are
//! out of scope (configurable via `patterns`).

use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "shared_state";

pub(crate) struct SharedState;

impl Check for SharedState {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.shared_state;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let content = fs::read_to_string(&path)?;
            let mut count = 0usize;
            for line in content.lines() {
                let code = strip_line_comment(line);
                for pat in &cfg.patterns {
                    count += code.matches(pat.as_str()).count();
                }
            }
            if count == 0 {
                continue;
            }
            let key = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            if count >= cfg.deny {
                violations.push(Violation::deny(
                    ID,
                    key,
                    format!(
                        "{count} Arc<Mutex/RwLock> occurrences in one file (deny threshold {})",
                        cfg.deny
                    ),
                ));
            } else if count >= cfg.warn {
                violations.push(Violation::warn(
                    ID,
                    key,
                    format!(
                        "{count} Arc<Mutex/RwLock> occurrences in one file (warn threshold {})",
                        cfg.warn
                    ),
                ));
            }
        }
        Ok(violations)
    }
}

fn strip_line_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(code, _)| code)
}
