use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use anyhow::Result;

use super::{Check, Context};
use crate::{
    common::{
        scan::Scan,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_tracked_files},
    },
    style::config::DocStalenessConfig,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "doc_staleness";
}

pub(crate) struct DocStaleness;

impl Check for DocStaleness {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.doc_staleness;
        let known = source_identifiers(ctx.scan, ctx.workspace_root)?;
        let mut violations = Vec::new();
        for path in ctx.scan.text_files(ctx.scope)?.iter() {
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            let Some(src) = ctx.scan.source(path) else {
                continue;
            };
            violations.extend(scan_content(cfg, &rel, &src, &|ident| {
                known.contains(ident)
            }));
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

/// Every identifier-shaped token the workspace sources contain.
fn source_identifiers(scan: &Scan, workspace_root: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for path in workspace_tracked_files(workspace_root)? {
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let Some(src) = scan.source(&path) else {
            continue;
        };
        for token in src.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')) {
            if !token.is_empty() {
                out.insert(token.to_string());
            }
        }
    }
    Ok(out)
}

fn scan_content(
    cfg: &DocStalenessConfig,
    rel: &str,
    src: &str,
    known: &dyn Fn(&str) -> bool,
) -> Vec<Violation> {
    let path = Path::new(rel);
    if !matches_any(&compile_globs(&cfg.include_globs), path)
        || matches_any(&compile_globs(&cfg.exclude_paths), path)
    {
        return Vec::new();
    }
    let allowed: BTreeSet<&str> = cfg.allow_terms.iter().map(String::as_str).collect();
    let mut reported = BTreeSet::new();
    let mut violations = Vec::new();
    for span in code_spans(src) {
        let Some(ident) = identifier(&span) else {
            continue;
        };
        if allowed.contains(ident.as_str()) || known(&ident) || !reported.insert(ident.clone()) {
            continue;
        }
        violations.push(Violation::deny(
            consts::ID,
            format!("{rel}::{ident}"),
            format!("{rel} documents `{ident}`, which no longer exists in the sources"),
        ));
    }
    violations
}

/// Inline code spans, ignoring fenced blocks, which hold examples rather than
/// claims about the current sources.
fn code_spans(src: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut fenced = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else {
                break;
            };
            spans.push(after[..close].to_string());
            rest = &after[close + 1..];
        }
    }
    spans
}

/// The Rust identifier a span names, or `None` when the span is prose, a
/// command, a path, or any other shape the sources cannot be asked about.
fn identifier(span: &str) -> Option<String> {
    let text = span.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let call = text.ends_with("()");
    let bare = text.strip_suffix("()").unwrap_or(text);
    let qualified = bare.contains("::");
    let last = bare.rsplit("::").next()?;
    if last.is_empty()
        || !last
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    let type_like = last.starts_with(|ch: char| ch.is_ascii_uppercase());
    if !(call || qualified || type_like) || last.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(last.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::violation::Severity;

    fn config() -> DocStalenessConfig {
        DocStalenessConfig {
            allow_terms: Vec::new(),
            exclude_paths: Vec::new(),
            include_globs: vec!["**/README.md".to_string()],
        }
    }

    #[test]
    fn denies_a_documented_identifier_absent_from_the_sources() {
        let src = "The `MissingType` owns the queue.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|_| false);

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].severity, Severity::Deny);
    }

    #[test]
    fn stays_silent_when_the_identifier_still_exists() {
        let src = "The `LivingType` owns the queue.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|_| true);

        assert!(violations.is_empty());
    }

    #[test]
    fn ignores_fenced_blocks() {
        let src = "```rust\nlet x = `MissingType`;\n```\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|_| false);

        assert!(violations.is_empty());
    }

    #[test]
    fn ignores_prose_words_in_backticks() {
        let src = "Set it to `warn` and move on.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|_| false);

        assert!(violations.is_empty());
    }

    #[test]
    fn reads_a_qualified_path_by_its_last_segment() {
        let src = "See `kithara_stream::MediaInfo` for details.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|ident| {
            ident == "MediaInfo"
        });

        assert!(violations.is_empty());
    }

    #[test]
    fn reads_a_call_span_without_its_parentheses() {
        let src = "Call `wait_range()` before reading.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|ident| {
            ident == "wait_range"
        });

        assert!(violations.is_empty());
    }

    #[test]
    fn honours_allowed_terms() {
        let cfg = DocStalenessConfig {
            allow_terms: vec!["MissingType".to_string()],
            exclude_paths: Vec::new(),
            include_globs: vec!["**/README.md".to_string()],
        };
        let src = "The `MissingType` owns the queue.\n";

        let violations = scan_content(&cfg, "crates/demo/README.md", src, &|_| false);

        assert!(violations.is_empty());
    }

    #[test]
    fn reports_a_repeated_identifier_once() {
        let src = "The `MissingType` owns it, and `MissingType` keeps it.\n";

        let violations = scan_content(&config(), "crates/demo/README.md", src, &|_| false);

        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn skips_documents_outside_the_included_globs() {
        let src = "The `MissingType` owns the queue.\n";

        let violations = scan_content(&config(), "crates/demo/ARCHITECTURE.md", src, &|_| false);

        assert!(violations.is_empty());
    }
}
