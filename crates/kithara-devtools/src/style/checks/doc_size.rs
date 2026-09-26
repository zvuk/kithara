use std::path::Path;

use anyhow::Result;

use super::{Check, Context};
use crate::{
    common::{
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to},
    },
    style::config::DocSizeConfig,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "doc_size";
}

pub(crate) struct DocSize;

impl Check for DocSize {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.doc_size;
        let mut violations = Vec::new();
        for path in ctx.scan.text_files(ctx.scope)?.iter() {
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            let Some(src) = ctx.scan.source(path) else {
                continue;
            };
            violations.extend(scan_content(cfg, &rel, &src));
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

fn scan_content(cfg: &DocSizeConfig, rel: &str, src: &str) -> Vec<Violation> {
    let excludes = compile_globs(&cfg.exclude_paths);
    if matches_any(&excludes, Path::new(rel)) {
        return Vec::new();
    }
    let bytes = src.len();
    for limit in &cfg.limits {
        let globs = compile_globs(&limit.globs);
        if !matches_any(&globs, Path::new(rel)) {
            continue;
        }
        if bytes > limit.deny {
            return vec![Violation::deny(
                consts::ID,
                rel.to_string(),
                format!(
                    "{rel} is {bytes} bytes, above the {} byte limit",
                    limit.deny
                ),
            )];
        }
        if bytes > limit.warn {
            return vec![Violation::warn(
                consts::ID,
                rel.to_string(),
                format!(
                    "{rel} is {bytes} bytes, above the {} byte limit",
                    limit.warn
                ),
            )];
        }
        return Vec::new();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{common::violation::Severity, style::config::DocSizeLimit};

    fn readme_limit() -> DocSizeConfig {
        DocSizeConfig {
            exclude_paths: Vec::new(),
            limits: vec![DocSizeLimit {
                deny: 3000,
                globs: vec!["**/README.md".to_string()],
                warn: 1500,
            }],
        }
    }

    fn two_class_limits() -> DocSizeConfig {
        DocSizeConfig {
            exclude_paths: Vec::new(),
            limits: vec![
                DocSizeLimit {
                    deny: 3000,
                    globs: vec!["**/ARCHITECTURE.md".to_string()],
                    warn: 1500,
                },
                DocSizeLimit {
                    deny: 1500,
                    globs: vec!["**/README.md".to_string()],
                    warn: 750,
                },
            ],
        }
    }

    #[test]
    fn denies_when_a_document_exceeds_its_deny_limit() {
        let src = "line\n".repeat(601);

        let violations = scan_content(&readme_limit(), "crates/demo/README.md", &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].severity, Severity::Deny);
    }

    #[test]
    fn stays_silent_at_the_warn_limit() {
        let src = "line\n".repeat(300);

        let violations = scan_content(&readme_limit(), "crates/demo/README.md", &src);

        assert!(violations.is_empty());
    }

    #[test]
    fn applies_the_limit_of_the_matching_document_class() {
        let src = "line\n".repeat(200);

        let violations = scan_content(&two_class_limits(), "crates/demo/README.md", &src);

        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("750"));
    }

    #[test]
    fn skips_excluded_documents() {
        let cfg = DocSizeConfig {
            exclude_paths: vec!["crates/demo/**".to_string()],
            limits: readme_limit().limits,
        };
        let src = "line\n".repeat(301);

        let violations = scan_content(&cfg, "crates/demo/README.md", &src);

        assert!(violations.is_empty());
    }

    #[test]
    fn warns_when_a_document_exceeds_its_warn_limit() {
        let src = "line\n".repeat(301);

        let violations = scan_content(&readme_limit(), "crates/demo/README.md", &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].severity, Severity::Warn);
    }
}
