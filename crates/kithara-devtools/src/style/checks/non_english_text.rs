use std::{fs, path::Path};

use anyhow::Result;
use glob::Pattern;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_text_files_scoped},
};

pub(crate) const ID: &str = "non_english_text";

pub(crate) struct NonEnglishText;

impl Check for NonEnglishText {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.non_english_text;
        let excludes = compile_globs(&cfg.exclude_paths);
        let mut violations = Vec::new();
        for path in workspace_text_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            if path_excluded(&excludes, &rel) {
                continue;
            }
            let Ok(src) = fs::read_to_string(&path) else {
                continue;
            };
            violations.extend(scan_lines(&rel, &src));
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

fn path_excluded(patterns: &[Pattern], rel: &str) -> bool {
    matches_any(patterns, Path::new(rel))
}

#[cfg(test)]
fn scan_content(
    cfg: &crate::style::config::NonEnglishTextConfig,
    rel: &str,
    src: &str,
) -> Vec<Violation> {
    let excludes = compile_globs(&cfg.exclude_paths);
    if path_excluded(&excludes, rel) {
        return Vec::new();
    }
    scan_lines(rel, src)
}

fn scan_lines(rel: &str, src: &str) -> Vec<Violation> {
    src.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if !line.chars().any(is_cyrillic) {
                return None;
            }
            let line_no = idx + 1;
            Some(Violation::deny(
                ID,
                format!("{rel}:{line_no}"),
                format!(
                    "Cyrillic text is not allowed in tracked text files at line {line_no}: {}",
                    excerpt(line)
                ),
            ))
        })
        .collect()
}

fn is_cyrillic(ch: char) -> bool {
    ('\u{0400}'..='\u{052F}').contains(&ch)
}

fn excerpt(line: &str) -> String {
    const LIMIT: usize = 120;
    let trimmed = line.trim();
    let mut out = String::new();
    for (idx, ch) in trimmed.chars().enumerate() {
        if idx == LIMIT {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{common::violation::Severity, style::config::NonEnglishTextConfig};

    fn cyrillic_sample() -> &'static str {
        "\u{043F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}"
    }

    #[test]
    fn detects_cyrillic_and_reports_line() {
        let src = format!("alpha\n// {}\nomega\n", cyrillic_sample());
        let violations = scan_content(
            &NonEnglishTextConfig::default(),
            "crates/demo/src/lib.rs",
            &src,
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].severity, Severity::Deny);
        assert_eq!(violations[0].key, "crates/demo/src/lib.rs:2");
        assert!(violations[0].message.contains("line 2"));
        assert!(violations[0].message.contains(cyrillic_sample()));
    }

    #[test]
    fn reports_each_matching_line_once() {
        let src = format!("{}\nlatin\n{}\n", cyrillic_sample(), "\u{052F}");
        let violations = scan_content(&NonEnglishTextConfig::default(), "docs/reference.md", &src);

        let keys: Vec<&str> = violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, ["docs/reference.md:1", "docs/reference.md:3"]);
    }

    #[test]
    fn excludes_configured_paths() {
        let cfg = NonEnglishTextConfig {
            exclude_paths: vec!["docs/plans/**".to_string()],
        };
        let src = format!("{}\n", cyrillic_sample());
        let violations = scan_content(&cfg, "docs/plans/local.md", &src);

        assert!(violations.is_empty());
    }
}
