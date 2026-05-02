//! Inline lint suppression for xtask static-analysis namespaces.
//!
//! Mirrors the `// ast-grep-ignore: <rule-id>` convention used by ast-grep
//! YAML rules. A directive comment on the line immediately preceding
//! a violation site silences the rule for that single line.
//!
//! Syntax:
//!
//! ```text
//! // optional reason (free-form, single line)
//! // xtask-lint-ignore: <check_id>[, <check_id>, ...]
//! offending_code();
//! ```
//!
//! Without an id list (`// xtask-lint-ignore` on its own) the directive
//! suppresses every check for the next non-comment, non-blank line. The
//! optional reason comment above is recommended but not enforced; a
//! check that ignores something silently is the surface of future
//! incidents.
//!
//! The parser walks raw source text rather than the syn AST because syn
//! discards comments. This is independent of the lint mechanics — every
//! check uses the same `Suppressions` struct constructed once per file.

use std::collections::{HashMap, HashSet};

const DIRECTIVE_PREFIX: &str = "xtask-lint-ignore";

/// Per-file map of suppressions extracted from comment directives.
#[derive(Debug, Default)]
pub(crate) struct Suppressions {
    /// Line number (1-based) → set of check ids suppressed for that
    /// line. An empty set means "suppress everything for this line".
    by_line: HashMap<usize, Suppress>,
}

#[derive(Debug, Default)]
struct Suppress {
    /// `true` when at least one directive on the preceding lines used
    /// the bare `// xtask-lint-ignore` form, suppressing every check.
    suppress_all: bool,
    ids: HashSet<String>,
}

impl Suppressions {
    /// Parse a Rust source file and collect every `xtask-lint-ignore`
    /// directive. Cheap (single linear scan).
    pub(crate) fn parse(source: &str) -> Self {
        let mut by_line: HashMap<usize, Suppress> = HashMap::new();
        let mut pending: Option<Suppress> = None;
        for (idx, line) in source.lines().enumerate() {
            let lineno = idx + 1;
            let trimmed = line.trim_start();
            if let Some(directive) = parse_directive(trimmed) {
                let entry = pending.get_or_insert_with(Suppress::default);
                match directive {
                    Directive::All => entry.suppress_all = true,
                    Directive::Ids(ids) => entry.ids.extend(ids),
                }
                continue;
            }
            // Plain comments and blank lines belong to the same banner
            // — the suppression carries through to the first real
            // statement.
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            if let Some(next) = pending.take() {
                by_line.insert(lineno, next);
            }
        }
        Self { by_line }
    }

    /// Whether `check_id` is suppressed at `line` (1-based).
    pub(crate) fn is_suppressed(&self, line: usize, check_id: &str) -> bool {
        let Some(s) = self.by_line.get(&line) else {
            return false;
        };
        s.suppress_all || s.ids.iter().any(|id| id == check_id)
    }
}

enum Directive {
    All,
    Ids(Vec<String>),
}

fn parse_directive(line: &str) -> Option<Directive> {
    // Accept both `// xtask-lint-ignore` and `//xtask-lint-ignore`,
    // optionally followed by `: <ids>`.
    let after_slash = line.strip_prefix("//")?;
    let after_slash = after_slash.trim_start();
    let rest = after_slash.strip_prefix(DIRECTIVE_PREFIX)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Some(Directive::All);
    }
    let rest = rest.strip_prefix(':')?;
    let ids: Vec<String> = rest
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    if ids.is_empty() {
        return Some(Directive::All);
    }
    Some(Directive::Ids(ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_directive_suppresses_nothing() {
        let s = Suppressions::parse("let x = 1;\nlet y = 2;\n");
        assert!(!s.is_suppressed(1, "any"));
        assert!(!s.is_suppressed(2, "any"));
    }

    #[test]
    fn id_directive_suppresses_target_line_only() {
        let src = "// xtask-lint-ignore: foo\nlet x = 1;\nlet y = 2;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(2, "foo"));
        assert!(!s.is_suppressed(2, "bar"));
        assert!(!s.is_suppressed(3, "foo"));
    }

    #[test]
    fn bare_directive_suppresses_all_checks_for_target_line() {
        let src = "// xtask-lint-ignore\nlet x = 1;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(2, "foo"));
        assert!(s.is_suppressed(2, "bar"));
    }

    #[test]
    fn multiple_ids_in_one_directive() {
        let src = "// xtask-lint-ignore: foo, bar\nlet x = 1;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(2, "foo"));
        assert!(s.is_suppressed(2, "bar"));
        assert!(!s.is_suppressed(2, "baz"));
    }

    #[test]
    fn reason_comment_above_directive_carries_through() {
        let src = "// reason: legitimate fallback\n// xtask-lint-ignore: foo\nlet x = 1;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(3, "foo"));
    }

    #[test]
    fn blank_line_between_directive_and_target_still_suppresses() {
        let src = "// xtask-lint-ignore: foo\n\nlet x = 1;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(3, "foo"));
    }

    #[test]
    fn directive_only_applies_to_next_real_line() {
        let src = "// xtask-lint-ignore: foo\nlet x = 1;\nlet y = 2;\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(2, "foo"));
        assert!(!s.is_suppressed(3, "foo"));
    }

    #[test]
    fn indented_directive_works() {
        let src = "fn f() {\n    // xtask-lint-ignore: foo\n    do_thing();\n}\n";
        let s = Suppressions::parse(src);
        assert!(s.is_suppressed(3, "foo"));
    }

    #[test]
    fn unrelated_comment_does_not_suppress() {
        let src = "// some commentary here\nlet x = 1;\n";
        let s = Suppressions::parse(src);
        assert!(!s.is_suppressed(2, "foo"));
    }
}
