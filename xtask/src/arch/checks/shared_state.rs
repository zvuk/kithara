//! Concentration of `Arc<Mutex/RwLock>` (and friends) per file.
//!
//! Counts substring occurrences after stripping `//` comments and string-literal
//! content, and skipping lines inside `#[cfg(test)]` modules — only real
//! production type-position usage is meant to count, not the rule's own pattern
//! list, doc-comment examples, or test fixtures.

use std::{collections::HashSet, fs};

use anyhow::Result;
use syn::{File, Item, ItemMod, spanned::Spanned};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
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
            let test_lines = parse_file(&path)
                .map(|file| collect_cfg_test_lines(&file))
                .unwrap_or_default();
            let mut count = 0usize;
            for (idx, line) in content.lines().enumerate() {
                let line_no = idx + 1;
                if test_lines.contains(&line_no) {
                    continue;
                }
                let code = strip_string_literals(strip_line_comment(line));
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

/// Returns the set of (1-based) line numbers covered by any `#[cfg(test)]`
/// inline module in `file`. External test files (`#[path = "..."]`) are out of
/// scope — those live in `tests/` and are already excluded by ignore globs.
fn collect_cfg_test_lines(file: &File) -> HashSet<usize> {
    let mut lines = HashSet::new();
    for item in &file.items {
        collect_in_item(item, &mut lines);
    }
    lines
}

fn collect_in_item(item: &Item, out: &mut HashSet<usize>) {
    if let Item::Mod(m) = item {
        if has_cfg_test_attr(m) {
            let span = m.span();
            let start = span.start().line;
            let end = span.end().line;
            for line_no in start..=end {
                out.insert(line_no);
            }
        }
        if let Some((_, inner)) = &m.content {
            for it in inner {
                collect_in_item(it, out);
            }
        }
    }
}

fn has_cfg_test_attr(m: &ItemMod) -> bool {
    m.attrs.iter().any(|a| {
        if !a.path().is_ident("cfg") {
            return false;
        }
        let mut hit = false;
        let _ = a.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                hit = true;
            }
            Ok(())
        });
        hit
    })
}

fn strip_line_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(code, _)| code)
}

/// Replace the *contents* of `"..."` string literals on a single line with
/// blanks so that pattern matchers can ignore literal text. Preserves the
/// surrounding quotes and the string length, and handles `\"` escapes. Raw
/// strings (`r"…"` / `r#"…"#`) are also handled at single-line granularity;
/// multi-line raw strings are out of scope (rare in production code) and are
/// only stripped on the line where they begin.
fn strip_string_literals(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                while let Some(next) = chars.next() {
                    if next == '\\' {
                        out.push(' ');
                        if chars.next().is_some() {
                            out.push(' ');
                        }
                        continue;
                    }
                    if next == '"' {
                        out.push('"');
                        break;
                    }
                    out.push(' ');
                }
            }
            'r' if chars.peek() == Some(&'"') => {
                out.push('r');
                chars.next();
                out.push('"');
                for next in chars.by_ref() {
                    if next == '"' {
                        out.push('"');
                        break;
                    }
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_blanks_string_content_but_preserves_length() {
        let stripped = strip_string_literals("let x = \"Arc<Mutex<u32>>\";");
        assert!(!stripped.contains("Arc<"));
        assert!(stripped.contains("\""));
    }

    #[test]
    fn strip_handles_escaped_quote() {
        let stripped = strip_string_literals(r#"let x = "a\"b";"#);
        assert!(stripped.starts_with("let x = "));
        assert!(stripped.contains('"'));
    }

    #[test]
    fn strip_leaves_real_code_alone() {
        let code = "let m: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));";
        assert_eq!(strip_string_literals(code), code);
    }
}
