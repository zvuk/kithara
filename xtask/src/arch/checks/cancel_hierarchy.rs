//! Forbid hard-coded `CancellationToken::new()` outside the marked
//! owner / bridge sites.
//!
//! The cancel hierarchy contract — see `crates/kithara-play/README.md`
//! "Cancel Hierarchy" — requires that production code derive every
//! `CancellationToken` either as a child of an upstream master (via
//! `.child_token()`) or take the master in via a config
//! field. Hard-coded `CancellationToken::new()` outside designated
//! owner / bridge sites is a hierarchy escape: those tokens never see
//! a parent shutdown pulse and the resulting "subsystem cancel" is an
//! orphan.
//!
//! Allowed sites carry an inline marker comment on the same line:
//! `// kithara:cancel:owner` (consumer-crate or `PlayerImpl::new`
//! fallback) or `// kithara:cancel:bridge` (FFI bridge that
//! intentionally outlives the player).
//!
//! Detection scans every `.rs` file in scope, parses with syn, finds
//! mod / fn / impl items annotated `#[cfg(test)]` (or any cfg
//! containing `test`) and excludes their line ranges. Lines outside
//! those ranges that contain `CancellationToken::new()` without a
//! marker comment are reported as violations. Crates whose
//! production *is* test scaffolding (test-utils, test-macros) are
//! exempt at the crate level.

use std::{collections::BTreeSet, fs};

use anyhow::Result;
use syn::{Attribute, Item, Meta, parse_file};

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "cancel_hierarchy";

/// Inline-comment markers + the call pattern the lint matches.
mod markers {
    pub(super) const OWNER: &str = "kithara:cancel:owner";
    pub(super) const BRIDGE: &str = "kithara:cancel:bridge";
    pub(super) const PATTERN: &str = "CancellationToken::new()";
}

pub(crate) struct CancelHierarchy;

impl Check for CancelHierarchy {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.cancel_hierarchy;
        let exempt: BTreeSet<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if rel_str.starts_with("tests/") || rel_str.contains("/tests/") {
                continue;
            }
            // Conventional `tests.rs` modules under `src/` are test
            // scaffolding (hosted via `#[cfg(test)] mod tests;` in the
            // parent module). Skip them by filename so callers don't
            // need to mark every fixture token.
            if rel.file_name().and_then(|f| f.to_str()) == Some("tests.rs") {
                continue;
            }
            if crate_is_exempt(rel, &exempt) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            if !content.contains(markers::PATTERN) {
                continue;
            }
            let Ok(file) = parse_file(&content) else {
                continue;
            };

            let mut test_ranges: Vec<(usize, usize)> = Vec::new();
            collect_test_ranges(&file.items, &mut test_ranges);

            for (idx, line) in content.lines().enumerate() {
                let code = strip_line_comment(line);
                if !code.contains(markers::PATTERN) {
                    continue;
                }
                if line.contains(markers::OWNER) || line.contains(markers::BRIDGE) {
                    continue;
                }
                let line_num = idx + 1;
                if test_ranges
                    .iter()
                    .any(|&(s, e)| line_num >= s && line_num <= e)
                {
                    continue;
                }
                violations.push(
                    Violation::deny(
                        ID,
                        format!("{rel_str}:{line_num}"),
                        "orphan `CancellationToken::new()` in production code".to_string(),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

const EXPLANATION: &str = "\
Summary: Hard-coded `CancellationToken::new()` outside the marked
owner / bridge sites breaks the unified cancel hierarchy.

Why: Every cancel token in production should be either (a) the
single master owned by the consumer crate (`Queue` / `App` / FFI
player / `PlayerImpl` fallback) or (b) a child of one such master
derived via `.child_token()`. Orphans synthesised at subsystem level
never see a parent shutdown pulse — dropping the player leaves the
orphan-rooted task running.

Fix: derive a child from the cancel passed in via your config
struct (e.g. `config.cancel.clone().unwrap_or_default().child_token()`),
or take a `CancellationToken` parameter from your caller. If the
construction is genuinely an owner / bridge site, add the inline
marker comment on the same line:
  - `// kithara:cancel:owner`  (consumer-crate top, PlayerImpl fallback)
  - `// kithara:cancel:bridge` (FFI bridge that outlives the player)

See `crates/kithara-play/README.md` \"Cancel Hierarchy\" for the
full contract.";

fn collect_test_ranges(items: &[Item], out: &mut Vec<(usize, usize)>) {
    for item in items {
        let attrs = item_attrs(item);
        let cfg_test = attrs_indicate_test(attrs);

        if cfg_test && let Some(range) = item_line_range(item) {
            out.push(range);
        }
        if let Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            collect_test_ranges(inner, out);
        }
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn attrs_indicate_test(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        // `#[cfg(test)]`, `#[cfg(any(test, feature = "..."))]`, …
        if attr.path().is_ident("cfg") {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta_contains_test(&meta) {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }
            // Fallback: tokenise the attribute and look for `test` ident.
            let s = quote_str(&attr.meta);
            if attr_token_contains_test(&s) {
                return true;
            }
        } else if attr.path().is_ident("test") || matches_path(&attr.meta, &["kithara", "test"]) {
            return true;
        }
    }
    false
}

fn meta_contains_test(meta: &syn::meta::ParseNestedMeta<'_>) -> bool {
    if meta.path.is_ident("test") {
        return true;
    }
    let mut nested_hit = false;
    let _ = meta.parse_nested_meta(|inner| {
        if meta_contains_test(&inner) {
            nested_hit = true;
        }
        Ok(())
    });
    nested_hit
}

fn quote_str(meta: &Meta) -> String {
    quote::ToTokens::to_token_stream(meta).to_string()
}

fn attr_token_contains_test(s: &str) -> bool {
    // Heuristic: split on any non-ident char and look for the `test`
    // word. Avoids matching `tests` substrings inside identifiers.
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|tok| tok == "test")
}

fn matches_path(meta: &Meta, segments: &[&str]) -> bool {
    let path = meta.path();
    if path.segments.len() != segments.len() {
        return false;
    }
    path.segments
        .iter()
        .zip(segments.iter())
        .all(|(seg, expected)| seg.ident == *expected)
}

fn item_line_range(item: &Item) -> Option<(usize, usize)> {
    let start_span = match item {
        Item::Mod(i) => i.mod_token.span,
        Item::Fn(i) => i.sig.fn_token.span,
        Item::Impl(i) => i.impl_token.span,
        Item::Const(i) => i.const_token.span,
        Item::Static(i) => i.static_token.span,
        _ => return None,
    };
    let start_line = start_span.start().line;
    let end_line = item_end_line(item).unwrap_or(start_line);
    Some((start_line, end_line))
}

fn item_end_line(item: &Item) -> Option<usize> {
    let line = match item {
        Item::Mod(i) => i.content.as_ref().map_or_else(
            || i.mod_token.span.end().line,
            |(b, _)| b.span.close().end().line,
        ),
        Item::Fn(i) => i.block.brace_token.span.close().end().line,
        Item::Impl(i) => i.brace_token.span.close().end().line,
        Item::Const(i) => i.semi_token.span.end().line,
        Item::Static(i) => i.semi_token.span.end().line,
        _ => return None,
    };
    Some(line)
}

/// Strip the trailing `//` line comment so the pattern match only
/// inspects code. Keeps the part before the first `//` that is not
/// inside a string literal (best-effort: counts unescaped double-quotes).
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut prev_backslash = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !in_string && b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            return &line[..i];
        }
        if b == b'"' && !prev_backslash {
            in_string = !in_string;
        }
        prev_backslash = b == b'\\' && !prev_backslash;
        i += 1;
    }
    line
}

fn crate_is_exempt(rel: &std::path::Path, exempt: &BTreeSet<&str>) -> bool {
    let mut comps = rel.components();
    if comps.next().and_then(|c| c.as_os_str().to_str()) != Some("crates") {
        return true; // outside crates/ — not a lib (e.g. xtask, apps)
    }
    let Some(crate_name) = comps
        .next()
        .and_then(|c| c.as_os_str().to_str().map(String::from))
    else {
        return true;
    };
    exempt.contains(crate_name.as_str())
}
