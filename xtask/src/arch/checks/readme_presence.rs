//! Each workspace member has a sufficiently long `README.md`.

use std::fs;

use anyhow::Result;

use super::{Check, Context};
use crate::common::{scope::packages_in_scope, violation::Violation};

pub(crate) const ID: &str = "readme_presence";

pub(crate) struct ReadmePresence;

impl Check for ReadmePresence {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.readme_presence;
        let mut violations = Vec::new();

        for pkg in packages_in_scope(ctx.metadata, ctx.scope) {
            if cfg.exempt.iter().any(|name| name == pkg.name.as_str()) {
                continue;
            }
            let manifest_dir = pkg
                .manifest_path
                .parent()
                .map(|p| p.as_std_path().to_path_buf());
            let Some(manifest_dir) = manifest_dir else {
                continue;
            };
            let readme = manifest_dir.join("README.md");
            let key = pkg.name.to_string();
            match fs::metadata(&readme) {
                Ok(meta) if meta.len() >= cfg.min_bytes => {}
                Ok(meta) => violations.push(Violation::deny(
                    ID,
                    key,
                    format!(
                        "stub README.md ({} bytes < min {})",
                        meta.len(),
                        cfg.min_bytes
                    ),
                )),
                Err(_) => violations.push(Violation::deny(
                    ID,
                    key,
                    format!("README.md missing at {}", readme.display()),
                )),
            }
        }
        Ok(violations)
    }
}
