//! Each canonical type has exactly one definition, in its declared owner crate.
//!
//! Migrated from legacy `arch.rs::check_canonical_types`, with the addition of
//! owner-crate enforcement (configured via `canonical-types.toml`).

use std::fs;

use anyhow::{Context as _, Result};
use regex::Regex;

use super::{Check, Context};
use crate::common::{violation::Violation, walker::walk_rs_files};

pub(crate) const ID: &str = "canonical_types";

pub(crate) struct CanonicalTypes;

impl Check for CanonicalTypes {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let entries = &ctx.config.canonical_types.entries;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let crates_dir = ctx.workspace_root.join("crates");
        let rs_files = walk_rs_files(&crates_dir)?;
        let mut violations = Vec::new();

        for entry in entries {
            let pattern = Regex::new(&format!(
                r"\bpub\s+{kind}\s+{name}\b",
                kind = regex::escape(&entry.kind),
                name = regex::escape(&entry.name),
            ))
            .with_context(|| format!("compile regex for {} {}", entry.kind, entry.name))?;

            let mut hits: Vec<String> = Vec::new();
            for file_path in &rs_files {
                let content = fs::read_to_string(file_path)?;
                for (line_no, line) in content.lines().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.contains("pub use") {
                        continue;
                    }
                    if pattern.is_match(line) {
                        let display_path = file_path
                            .strip_prefix(ctx.workspace_root)
                            .unwrap_or(file_path)
                            .display()
                            .to_string();
                        hits.push(format!("{display_path}:{line}", line = line_no + 1));
                    }
                }
            }

            let key = format!("{} {}", entry.kind, entry.name);

            match hits.len() {
                0 => {
                    violations.push(Violation::deny(
                        ID,
                        key,
                        format!(
                            "canonical {} '{}' is not defined anywhere (expected in '{}')",
                            entry.kind, entry.name, entry.owner,
                        ),
                    ));
                }
                1 => {
                    let location = &hits[0];
                    let owner_marker = format!("crates/{}/", entry.owner);
                    if !location.contains(&owner_marker) {
                        violations.push(Violation::deny(
                            ID,
                            key,
                            format!(
                                "canonical {} '{}' is defined in '{}', but expected in '{}'",
                                entry.kind, entry.name, location, entry.owner,
                            ),
                        ));
                    }
                }
                _ => {
                    let joined = hits.join(", ");
                    violations.push(Violation::deny(
                        ID,
                        key,
                        format!(
                            "canonical {} '{}' defined {} times: {}",
                            entry.kind,
                            entry.name,
                            hits.len(),
                            joined,
                        ),
                    ));
                }
            }
        }

        Ok(violations)
    }
}
