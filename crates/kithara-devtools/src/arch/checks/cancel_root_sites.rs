use std::{collections::BTreeSet, fs};

use anyhow::Result;
use syn::{Attribute, Item, Meta, parse_file};

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "cancel_root_sites";

pub(crate) struct CancelRootSites;

impl Check for CancelRootSites {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.cancel_root_sites;
        let exempt: BTreeSet<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let allowed: BTreeSet<&str> = cfg.allowed_files.iter().map(String::as_str).collect();
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if is_test_or_bench_path(&rel_str) {
                continue;
            }
            if rel.file_name().and_then(|f| f.to_str()) == Some("tests.rs") {
                continue;
            }
            if crate_is_exempt(rel, &exempt) || allowed.contains(rel_str.as_str()) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            for (line_num, pattern) in scan_source(&content) {
                violations.push(
                    Violation::deny(
                        ID,
                        format!("{rel_str}:{line_num}"),
                        format!("orphan `{pattern}()` cancel-root minting in production code"),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

/// Pure core: the (1-based line, matched pattern) of every root-minting call in
/// `content` that sits in production code (outside `#[cfg(test)]` ranges and
/// line comments). Path-level allowlist / exempt-crate / test-path skips are
/// applied by [`CancelRootSites::run`] before this is called.
fn scan_source(content: &str) -> Vec<(usize, &'static str)> {
    // Fresh-root minting calls denied outside the allowlist: the owning-master
    // `CancelToken::root` and the never-cancelled sentinel `CancelToken::never`.
    // Both root a new cancel tree; `.child()` (the sanctioned derivation) is
    const PATTERNS: &[&str] = &["CancelToken::root", "CancelToken::never"];
    if !PATTERNS.iter().any(|p| content.contains(p)) {
        return Vec::new();
    }
    let Ok(file) = parse_file(content) else {
        return Vec::new();
    };

    let mut test_ranges: Vec<(usize, usize)> = Vec::new();
    collect_test_ranges(&file.items, &mut test_ranges);

    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let code = strip_line_comment(line);
        let Some(pattern) = PATTERNS.iter().find(|p| code.contains(**p)) else {
            continue;
        };
        let line_num = idx + 1;
        if test_ranges
            .iter()
            .any(|&(s, e)| line_num >= s && line_num <= e)
        {
            continue;
        }
        hits.push((line_num, *pattern));
    }
    hits
}

fn is_test_or_bench_path(rel_str: &str) -> bool {
    rel_str.starts_with("tests/") || rel_str.contains("/tests/") || rel_str.contains("/benches/")
}

const EXPLANATION: &str = "\
Summary: minting a fresh cancel root — `CancelToken::root()` (owning master) or
`CancelToken::never()` (never-cancelled sentinel) — outside the allowlisted
sites roots a new cancel tree that no parent shutdown pulse reaches.

Why: every production cancel token should be either (a) a master root owned at a
consumer-crate top (`App` / FFI player) or vended by `CancelScope`, or (b) a
child derived via `.child()` from such a master. A root minted at subsystem
level is an orphan: dropping the player leaves its orphan-rooted tasks running,
and a child of an orphan never observes the real master cancel on the
produce-core.

Fix: derive a child from the cancel handed in via your config
(`CancelScope::new(config.cancel).token().child()`), or take a `CancelToken`
parameter from your caller. Genuine owner / bridge / sentinel sites are
sanctioned per-file in the `cancel_root_sites` allowlist (see
`.config/arch/thresholds.toml`); add a file there only with a clear owner
reason.

See `crates/kithara-play/README.md` \"Cancel Hierarchy\" and `AGENTS.md`.";

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

/// Strip the trailing `//` line comment so the pattern match only inspects code.
/// Keeps the part before the first `//` that is not inside a string literal
/// (best-effort: counts unescaped double-quotes).
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{crate_is_exempt, is_test_or_bench_path, scan_source};

    #[test]
    fn flags_root_and_never_in_production() {
        let src = "fn build() {\n    let a = CancelToken::root();\n    let b = CancelToken::never();\n}\n";
        let hits = scan_source(src);
        assert_eq!(
            hits,
            vec![(2, "CancelToken::root"), (3, "CancelToken::never")]
        );
    }

    #[test]
    fn ignores_child_and_other_methods() {
        let src = "fn run(parent: CancelToken) {\n    let _ = parent.child();\n    let _ = parent.is_cancelled();\n}\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn skips_cfg_test_modules() {
        let src = "fn prod() {\n    let _ = parent.child();\n}\n\n#[cfg(test)]\nmod tests {\n    fn t() {\n        let _ = CancelToken::root();\n    }\n}\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn ignores_matches_in_comments() {
        let src =
            "fn f() {\n    // CancelToken::root() in a comment is not a mint\n    let _ = 1;\n}\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn allowed_and_exempt_resolution() {
        let exempt = ["kithara-test-utils"].into_iter().collect();
        assert!(crate_is_exempt(
            Path::new("crates/kithara-test-utils/src/lib.rs"),
            &exempt
        ));
        assert!(!crate_is_exempt(
            Path::new("crates/kithara-audio/src/audio.rs"),
            &exempt
        ));
        assert!(is_test_or_bench_path(
            "crates/kithara-platform/benches/cancel.rs"
        ));
        assert!(is_test_or_bench_path("crates/kithara-hls/src/tests/foo.rs"));
        assert!(!is_test_or_bench_path("crates/kithara-app/src/main.rs"));
    }
}
