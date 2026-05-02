//! No `.rs` files at workspace root.

use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::violation::Violation;

pub(crate) const ID: &str = "stray_rs_files";

pub(crate) struct StrayRsFiles;

impl Check for StrayRsFiles {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let mut violations = Vec::new();
        for entry in fs::read_dir(ctx.workspace_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let display = path.display().to_string();
                violations.push(Violation::deny(
                    ID,
                    display.clone(),
                    format!("stray .rs file at workspace root: {display}"),
                ));
            }
        }
        Ok(violations)
    }
}
