//! No crate defines `pub enum Error` more than once.
//!
//! Severity is `warn` to match legacy behavior; PR 3 promotes it to `deny`
//! once the ratchet baseline lands.

use std::{collections::HashMap, fs};

use anyhow::Result;
use regex::Regex;

use super::{Check, Context};
use crate::common::{violation::Violation, walker::walk_rs_files};

pub(crate) const ID: &str = "duplicate_error_enums";

pub(crate) struct DuplicateErrorEnums;

impl Check for DuplicateErrorEnums {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let crates_dir = ctx.workspace_root.join("crates");
        let rs_files = walk_rs_files(&crates_dir)?;
        let pattern = Regex::new(r"\bpub\s+enum\s+Error\b")?;
        let mut counts: HashMap<String, usize> = HashMap::new();

        for file_path in &rs_files {
            let content = fs::read_to_string(file_path)?;
            let line_count = content
                .lines()
                .filter(|line| pattern.is_match(line))
                .count();
            if line_count == 0 {
                continue;
            }
            let rel = file_path.strip_prefix(&crates_dir).unwrap_or(file_path);
            if let Some(crate_name) = rel.components().next() {
                let name = crate_name.as_os_str().to_string_lossy().to_string();
                *counts.entry(name).or_insert(0) += line_count;
            }
        }

        let mut violations = Vec::new();
        for (crate_name, count) in counts {
            if count > 1 {
                violations.push(Violation::warn(
                    ID,
                    crate_name.clone(),
                    format!("crate '{crate_name}' defines 'pub enum Error' {count} times"),
                ));
            }
        }
        Ok(violations)
    }
}
