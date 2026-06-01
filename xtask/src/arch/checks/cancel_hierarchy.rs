use std::{collections::BTreeSet, fs};

use anyhow::Result;
use syn::{Attribute, Item, Meta, parse_file};

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "cancel_hierarchy";

/// Inline-comment markers + the call patterns the lint matches.
mod markers {
    pub(super) const OWNER: &str = "kithara:cancel:owner";
    pub(super) const BRIDGE: &str = "kithara:cancel:bridge";
    /// Master-minting calls that must sit at a marked owner / bridge site:
    /// the workspace `CancellationToken`'s fresh-root constructor
    /// (`CancellationToken::default()`). It roots a new cancel hierarchy, so an
    /// unmarked one is an orphan that never sees a parent shutdown pulse.
    /// (`.unwrap_or_default()` is not matched — it is the sanctioned standalone
    /// fallback on `Option<CancellationToken>`.)
    pub(super) const PATTERNS: &[&str] = &["CancellationToken::default()"];
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
            if rel.file_name().and_then(|f| f.to_str()) == Some("tests.rs") {
                continue;
            }
            if crate_is_exempt(rel, &exempt) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            if !markers::PATTERNS.iter().any(|p| content.contains(p)) {
                continue;
            }
            let Ok(file) = parse_file(&content) else {
                continue;
            };

            let mut test_ranges: Vec<(usize, usize)> = Vec::new();
            collect_test_ranges(&file.items, &mut test_ranges);

            for (idx, line) in content.lines().enumerate() {
                let code = strip_line_comment(line);
                let Some(pattern) = markers::PATTERNS.iter().find(|p| code.contains(**p)) else {
                    continue;
                };
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
                        format!("orphan `{pattern}` master-cancel construction in production code"),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

const EXPLANATION: &str = "\
Summary: Hard-coded `CancellationToken::default()` outside the marked
owner / bridge sites breaks the unified cancel hierarchy. It mints a fresh
master and roots a new tree.

Why: Every cancel token in production should be either (a) the
single master owned by the consumer crate (`Queue` / `App` / FFI
player / `PlayerImpl` fallback) or (b) a child of one such master
derived via `.child_token()`. Orphans synthesised at subsystem level
never see a parent shutdown pulse — dropping the player leaves the
orphan-rooted task running. A fresh master is doubly dangerous: its
lock-free flag chain roots a new tree, so a child of an orphan master
never observes the real master cancel on the produce-core.

Fix: derive a child from the cancel passed in via your config struct
(e.g. `config.cancel.clone().unwrap_or_default().child_token()`), or
take a `CancellationToken` parameter from your caller. If
the construction is genuinely an owner / bridge site, add the inline
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
        return true;
    }
    let Some(crate_name) = comps
        .next()
        .and_then(|c| c.as_os_str().to_str().map(String::from))
    else {
        return true;
    };
    exempt.contains(crate_name.as_str())
}
