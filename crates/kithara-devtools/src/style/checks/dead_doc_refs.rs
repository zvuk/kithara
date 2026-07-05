use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::Result;
use glob::Pattern;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{
        compile_globs, matches_any, relative_to, workspace_text_files_scoped,
        workspace_tracked_files,
    },
};

pub(crate) const ID: &str = "dead_doc_refs";

pub(crate) struct DeadDocRefs;

impl Check for DeadDocRefs {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.dead_doc_refs;
        let excludes = compile_globs(&cfg.exclude_paths);
        let allow_targets = compile_globs(&cfg.allow_targets);
        let tracked_docs = tracked_doc_targets(ctx.workspace_root)?;
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
            let refs = scan_content(&rel, &src, |target| tracked_docs.contains(target));
            violations.extend(
                refs.into_iter()
                    .filter(|reference| !target_allowed(&allow_targets, &reference.target))
                    .map(|reference| reference.into_violation(&rel)),
            );
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

fn tracked_doc_targets(workspace_root: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for path in workspace_tracked_files(workspace_root)? {
        let rel = relative_to(workspace_root, &path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_doc_path(&rel) {
            out.insert(rel);
        }
    }
    Ok(out)
}

fn path_excluded(patterns: &[Pattern], rel: &str) -> bool {
    matches_any(patterns, Path::new(rel))
}

fn target_allowed(patterns: &[Pattern], target: &str) -> bool {
    matches_any(patterns, Path::new(target))
}

#[derive(Debug, Eq, PartialEq)]
struct DeadDocRef {
    line_no: usize,
    reference: String,
    target: String,
}

impl DeadDocRef {
    fn into_violation(self, rel: &str) -> Violation {
        Violation::deny(
            ID,
            format!("{rel}:{}:{}", self.line_no, self.target),
            format!(
                "dead document reference at line {}: '{}' resolves to '{}', which is not tracked by git",
                self.line_no, self.reference, self.target
            ),
        )
    }
}

fn scan_content<F>(file_rel: &str, src: &str, is_tracked: F) -> Vec<DeadDocRef>
where
    F: Fn(&str) -> bool,
{
    let file_norm =
        normalize_workspace_path(file_rel).unwrap_or_else(|| file_rel.replace('\\', "/"));
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        let line_no = idx + 1;
        for target in markdown_link_targets(line) {
            add_candidate(
                &mut out,
                &mut seen,
                &file_norm,
                line_no,
                &target.reference,
                resolve_markdown_target(file_rel, &target.target),
                &is_tracked,
            );
        }
        for target in bare_path_targets(line) {
            add_candidate(
                &mut out,
                &mut seen,
                &file_norm,
                line_no,
                &target,
                resolve_bare_target(&target),
                &is_tracked,
            );
        }
    }
    out
}

fn add_candidate<F>(
    out: &mut Vec<DeadDocRef>,
    seen: &mut BTreeSet<(usize, String)>,
    file_rel: &str,
    line_no: usize,
    reference: &str,
    target: Option<String>,
    is_tracked: &F,
) where
    F: Fn(&str) -> bool,
{
    let Some(target) = target else {
        return;
    };
    if target == file_rel || is_tracked(&target) || !seen.insert((line_no, target.clone())) {
        return;
    }
    out.push(DeadDocRef {
        line_no,
        reference: reference.to_string(),
        target,
    });
}

#[derive(Debug, Eq, PartialEq)]
struct MarkdownTarget {
    reference: String,
    target: String,
}

fn markdown_link_targets(line: &str) -> Vec<MarkdownTarget> {
    let mut out = Vec::new();
    let mut search_start = 0;
    while let Some(marker_start) = line[search_start..].find("](") {
        let target_start = search_start + marker_start + 2;
        let Some(target_end_rel) = line[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + target_end_rel;
        let raw = &line[target_start..target_end];
        if let Some(target) = parse_markdown_target(raw) {
            out.push(MarkdownTarget {
                reference: raw.trim().to_string(),
                target,
            });
        }
        search_start = target_end + 1;
    }
    out
}

fn parse_markdown_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let target = if let Some(rest) = trimmed.strip_prefix('<') {
        let end = rest.find('>')?;
        &rest[..end]
    } else {
        trimmed.split_whitespace().next()?
    };
    let target = strip_fragment(target).trim();
    if should_skip_target(target) || !is_doc_path(target) {
        return None;
    }
    Some(target.to_string())
}

fn bare_path_targets(line: &str) -> Vec<String> {
    line.split(is_bare_delimiter)
        .filter_map(parse_bare_token)
        .collect()
}

fn is_bare_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';' | '|')
}

fn parse_bare_token(raw: &str) -> Option<String> {
    let token = raw
        .trim_start_matches(['`', '*', '_'])
        .trim_end_matches(['`', '*', '_', '.', ':', '!', '?']);
    let token = strip_fragment(token);
    if should_skip_target(token) || !is_bare_doc_path(token) {
        return None;
    }
    Some(token.to_string())
}

fn resolve_markdown_target(file_rel: &str, target: &str) -> Option<String> {
    if is_external_target(target) {
        return None;
    }
    let raw = target.strip_prefix('/').map_or_else(
        || {
            let base = Path::new(file_rel)
                .parent()
                .and_then(Path::to_str)
                .unwrap_or("");
            if base.is_empty() {
                target.to_string()
            } else {
                format!("{base}/{target}")
            }
        },
        ToString::to_string,
    );
    normalize_workspace_path(&raw)
}

fn resolve_bare_target(target: &str) -> Option<String> {
    if is_external_target(target) {
        return None;
    }
    normalize_workspace_path(target.strip_prefix('/').unwrap_or(target))
}

fn is_bare_doc_path(target: &str) -> bool {
    let root_relative = target.strip_prefix("./").unwrap_or(target);
    let root_relative = root_relative.strip_prefix('/').unwrap_or(root_relative);
    if root_relative.starts_with("docs/") || root_relative.starts_with("crates/") {
        return has_doc_extension(root_relative);
    }
    root_relative.ends_with(".mdc")
}

fn is_doc_path(target: &str) -> bool {
    has_doc_extension(target)
}

fn has_doc_extension(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdc")
}

fn strip_fragment(target: &str) -> &str {
    target
        .split_once('#')
        .map_or(target, |(path, _fragment)| path)
}

fn should_skip_target(target: &str) -> bool {
    target.is_empty()
        || is_external_target(target)
        || target.contains("YYYY")
        || target
            .chars()
            .any(|ch| matches!(ch, '*' | '<' | '>' | '$' | '{' | '}'))
}

fn is_external_target(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:")
}

fn normalize_workspace_path(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let path = PathBuf::from(path);
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|part| part != "..") {
                    parts.pop();
                } else {
                    parts.push("..".to_string());
                }
            }
            Component::Normal(part) => {
                let part = part.to_str()?;
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::common::violation::Severity;

    fn tracked(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|path| (*path).to_string()).collect()
    }

    fn scan_with_tracked(file_rel: &str, src: &str, tracked: &HashSet<String>) -> Vec<DeadDocRef> {
        scan_content(file_rel, src, |target| tracked.contains(target))
    }

    #[test]
    fn markdown_links_resolve_relative_with_title_and_fragment() {
        let tracked = tracked(&["docs/guides/reference.md"]);
        let src = r#"[ok](../reference.md#details "Reference")"#;

        let violations = scan_with_tracked("docs/guides/topic/current.md", src, &tracked);

        assert!(violations.is_empty());
    }

    #[test]
    fn markdown_links_resolve_leading_slash_from_workspace_root() {
        let tracked = tracked(&["docs/README.md"]);
        let src = "[ok](/docs/README.md)";

        let violations = scan_with_tracked("crates/kithara/README.md", src, &tracked);

        assert!(violations.is_empty());
    }

    #[test]
    fn bare_doc_paths_resolve_from_workspace_root() {
        let tracked = tracked(&["docs/guides/test-harness.md"]);
        let src = "See docs/guides/test-harness.md.";

        let violations = scan_with_tracked("crates/kithara-devtools/README.md", src, &tracked);

        assert!(violations.is_empty());
    }

    #[test]
    fn reports_missing_markdown_and_bare_targets() {
        let tracked = tracked(&["docs/README.md"]);
        let src = "See [missing](../missing.md) and docs/missing-too.md.";

        let violations = scan_with_tracked("docs/guides/current.md", src, &tracked);

        assert_eq!(
            violations,
            vec![
                DeadDocRef {
                    line_no: 1,
                    reference: "../missing.md".to_string(),
                    target: "docs/missing.md".to_string(),
                },
                DeadDocRef {
                    line_no: 1,
                    reference: "docs/missing-too.md".to_string(),
                    target: "docs/missing-too.md".to_string(),
                },
            ]
        );
    }

    #[test]
    fn skips_urls_placeholders_and_self_references() {
        let tracked = HashSet::new();
        let src = concat!(
            "[web](https://example.test/missing.md)\n",
            "[mail](mailto:docs/missing.md)\n",
            "docs/plans/YYYY-MM-DD-<slug>.md\n",
            "[self](current.md#section)\n",
        );

        let violations = scan_with_tracked("docs/guides/current.md", src, &tracked);

        assert!(violations.is_empty());
    }

    #[test]
    fn treats_existing_but_untracked_paths_as_dead() {
        let tracked = tracked(&["docs/README.md"]);
        let src = "Local-only doc: docs/local-only.md";

        let violations = scan_with_tracked("README.md", src, &tracked);

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].target, "docs/local-only.md");
    }

    #[test]
    fn configured_allowed_targets_are_not_violations() {
        let allow_targets = compile_globs(&["docs/local-only.md".to_string()]);
        let refs = scan_content("README.md", "docs/local-only.md", |_target| false);
        let violations: Vec<Violation> = refs
            .into_iter()
            .filter(|reference| !target_allowed(&allow_targets, &reference.target))
            .map(|reference| reference.into_violation("README.md"))
            .collect();

        assert!(violations.is_empty());
    }

    #[test]
    fn violations_are_deny_severity() {
        let refs = scan_content("README.md", "docs/missing.md", |_target| false);
        let violation = refs
            .into_iter()
            .next()
            .map(|reference| reference.into_violation("README.md"))
            .unwrap();

        assert_eq!(violation.severity, Severity::Deny);
        assert_eq!(violation.key, "README.md:1:docs/missing.md");
    }
}
