use std::{
    collections::{BTreeMap, BTreeSet},
    ops::RangeInclusive,
    path::Path,
};

use glob::Pattern;
use syn::{ImplItem, Item, Meta, spanned::Spanned};

use crate::common::{
    parse::parse_file,
    violation::Report,
    walker::{compile_globs, matches_any},
};

/// Drop violations whose path portion matches any glob. A no-op when empty.
pub(crate) fn apply_path_excludes(report: &mut Report, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let globs = compile_globs(patterns);
    report
        .violations
        .retain(|v| !matches_any(&globs, Path::new(key_path(&v.key))));
}

/// Drop violations that land on a line inside a `#[cfg(test)]`-predicated item
/// (`mod tests { ... }`, a test `fn`, a test `impl`, ...). Complements the
/// path-glob pass: inline test modules in production `src/*.rs` files are test
/// code but are not matched by path globs. Only `test`-keyed cfgs count —
/// `#[cfg(feature = ...)]` and other cfgs are left untouched. Files that fail
/// to parse are skipped (their violations are kept).
pub(crate) fn apply_cfg_test_exclusion(report: &mut Report, workspace_root: &Path) {
    let ranges = ranges_by_file(report, workspace_root, |items, out| {
        collect_cfg_test_ranges(items, out);
    });
    retain_outside_ranges(report, &ranges);
}

/// Drop violations inside an inline `mod` whose leaf name or file-relative
/// `::`-path matches a glob. Complements the path-glob and `#[cfg(test)]`
/// passes with sub-file, module-scoped exclusion (e.g. scope out a whole
/// `mod legacy {}` without listing files). A no-op when `patterns` is empty.
pub(crate) fn apply_module_excludes(
    report: &mut Report,
    patterns: &[String],
    workspace_root: &Path,
) {
    if patterns.is_empty() {
        return;
    }
    let globs = compile_globs(patterns);
    let ranges = ranges_by_file(report, workspace_root, |items, out| {
        collect_module_ranges(items, &mut Vec::new(), &globs, out);
    });
    retain_outside_ranges(report, &ranges);
}

/// Parse every file referenced by a violation once and collect its excluded
/// line ranges via `collect`. Files that fail to parse contribute no ranges
/// (their violations are kept).
fn ranges_by_file(
    report: &Report,
    workspace_root: &Path,
    collect: impl Fn(&[Item], &mut Vec<RangeInclusive<usize>>),
) -> BTreeMap<String, Vec<RangeInclusive<usize>>> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for v in &report.violations {
        files.insert(key_path(&v.key).to_string());
    }
    files
        .into_iter()
        .filter_map(|rel| {
            let file = parse_file(&workspace_root.join(&rel)).ok()?;
            let mut out = Vec::new();
            collect(&file.items, &mut out);
            (!out.is_empty()).then_some((rel, out))
        })
        .collect()
}

/// Keep a violation unless its line falls inside one of its file's ranges.
/// Violations whose key carries no line (e.g. `file.rs::Name`) are kept.
fn retain_outside_ranges(
    report: &mut Report,
    ranges: &BTreeMap<String, Vec<RangeInclusive<usize>>>,
) {
    report.violations.retain(|v| {
        let Some(line) = key_line(&v.key) else {
            return true;
        };
        ranges
            .get(key_path(&v.key))
            .is_none_or(|rs| !rs.iter().any(|r| r.contains(&line)))
    });
}

/// Extract the workspace-relative path portion from a violation key. Keys look
/// like `crates/<crate>/src/.../file.rs:line:col` or `file.rs::Name`; the path
/// always ends at the `.rs` extension, so we slice up to and including it.
pub(crate) fn key_path(key: &str) -> &str {
    key.find(".rs").map_or(key, |i| &key[..i + 3])
}

/// Extract the 1-based source line a violation key points at. The line is the
/// first `:`-separated field after the `.rs` path portion, e.g. `561` in
/// `crates/x/src/foo.rs:561:for_body`. Returns `None` for keys that carry no
/// line (e.g. `file.rs::Name`).
pub(crate) fn key_line(key: &str) -> Option<usize> {
    let path = key_path(key);
    let rest = key.get(path.len()..)?.strip_prefix(':')?;
    let field = rest.split(':').next()?;
    field.parse::<usize>().ok()
}

/// Count lines in `src` that do not fall inside a `#[cfg(test)]` item range.
/// File-keyed checks (e.g. `file_size`) carry no line in their violation key,
/// so the line-based [`apply_cfg_test_exclusion`] pass cannot reach them — they
/// fold the same test-code exclusion in here instead, measuring only the
/// production surface. Returns the raw line count when `src` fails to parse.
pub(crate) fn non_test_line_count(src: &str) -> usize {
    let total = src.lines().count();
    let Ok(file) = syn::parse_file(src) else {
        return total;
    };
    let mut ranges = Vec::new();
    collect_cfg_test_ranges(&file.items, &mut ranges);
    let excluded: BTreeSet<usize> = ranges.into_iter().flatten().collect();
    total.saturating_sub(excluded.len())
}

/// Walk items recursively, recording the line range of every item whose
/// attributes carry a `test`-predicated cfg. Recurses into module bodies so
/// nested `#[cfg(test)] mod`/`fn` are caught regardless of depth.
pub(crate) fn collect_cfg_test_ranges(items: &[Item], out: &mut Vec<RangeInclusive<usize>>) {
    for item in items {
        if attrs_have_cfg_test(item_attrs(item)) {
            out.push(item.span().start().line..=item.span().end().line);
        }
        if let Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            collect_cfg_test_ranges(inner, out);
        }
        if let Item::Impl(im) = item {
            for it in &im.items {
                if let ImplItem::Fn(f) = it
                    && attrs_have_cfg_test(&f.attrs)
                {
                    out.push(f.span().start().line..=f.span().end().line);
                }
            }
        }
    }
}

/// Record the line range of every inline `mod` whose leaf name or `::`-path
/// (joined from the file root) matches one of the compiled globs. Recurses so
/// nested modules match at any depth.
fn collect_module_ranges(
    items: &[Item],
    path: &mut Vec<String>,
    globs: &[Pattern],
    out: &mut Vec<RangeInclusive<usize>>,
) {
    for item in items {
        let Item::Mod(m) = item else {
            continue;
        };
        let leaf = m.ident.to_string();
        path.push(leaf.clone());
        let full = path.join("::");
        if matches_any(globs, Path::new(&leaf)) || matches_any(globs, Path::new(&full)) {
            out.push(m.span().start().line..=m.span().end().line);
        }
        if let Some((_, inner)) = &m.content {
            collect_module_ranges(inner, path, globs, out);
        }
        path.pop();
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
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

/// True when any `#[cfg(...)]` attribute is keyed on `test` — either directly
/// (`#[cfg(test)]`) or as a bare predicate inside `any(...)` / `all(...)`
/// (`#[cfg(any(test, feature = "x"))]`). `#[cfg(not(test))]`, `feature = "test"`,
/// and non-cfg attributes are intentionally not matched.
pub(crate) fn attrs_have_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| match &a.meta {
        Meta::List(list) if list.path.is_ident("cfg") => meta_tokens_have_test(&list.tokens),
        _ => false,
    })
}

fn meta_tokens_have_test(tokens: &proc_macro2::TokenStream) -> bool {
    syn::parse2::<Meta>(tokens.clone()).is_ok_and(|m| cfg_predicate_has_test(&m))
}

fn cfg_predicate_has_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::List(list) if list.path.is_ident("any") || list.path.is_ident("all") => list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .is_ok_and(|nested| nested.iter().any(cfg_predicate_has_test)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::violation::Violation;

    fn exclude_globs() -> Vec<String> {
        [
            "**/tests/**",
            "**/tests.rs",
            "**/*_test.rs",
            "**/test_*.rs",
            "crates/kithara-test-utils/**",
            "crates/kithara-test-macros/**",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    #[test]
    fn path_excludes_drop_test_code_keep_production() {
        let mut report = Report::default();
        report.extend([
            Violation::warn("loop_allocation", "crates/x/tests.rs:10:4", "test"),
            Violation::warn(
                "fat_loop_body",
                "crates/x/tests/helper.rs:5:loop_body",
                "test",
            ),
            Violation::warn(
                "box_concrete_type",
                "crates/kithara-test-utils/src/a.rs:1:1",
                "test",
            ),
            Violation::warn("loop_allocation", "crates/x/src/foo.rs:7:8", "prod"),
        ]);

        apply_path_excludes(&mut report, &exclude_globs());

        let keys: Vec<&str> = report.violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, ["crates/x/src/foo.rs:7:8"]);
    }

    #[test]
    fn path_excludes_empty_is_noop() {
        let mut report = Report::default();
        report.extend([Violation::warn(
            "loop_allocation",
            "crates/x/tests.rs:10:4",
            "test",
        )]);
        apply_path_excludes(&mut report, &[]);
        assert_eq!(report.violations.len(), 1);
    }

    #[test]
    fn key_path_strips_line_col_and_name_suffix() {
        assert_eq!(key_path("crates/x/src/foo.rs:7:8"), "crates/x/src/foo.rs");
        assert_eq!(
            key_path("crates/x/src/foo.rs:173:48::outcome"),
            "crates/x/src/foo.rs"
        );
        assert_eq!(key_path("kithara-foo"), "kithara-foo");
    }

    #[test]
    fn key_line_reads_first_field_after_path() {
        assert_eq!(key_line("crates/x/src/foo.rs:561:for_body"), Some(561));
        assert_eq!(key_line("crates/x/src/foo.rs:7:8"), Some(7));
        assert_eq!(key_line("crates/x/src/foo.rs:173:48::outcome"), Some(173));
        assert_eq!(key_line("crates/x/src/foo.rs::Name"), None);
        assert_eq!(key_line("kithara-foo"), None);
    }

    fn cfg_ranges(src: &str) -> Vec<(usize, usize)> {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        collect_cfg_test_ranges(&file.items, &mut out);
        out.iter().map(|r| (*r.start(), *r.end())).collect()
    }

    #[test]
    fn cfg_test_mod_range_covers_inner_lines() {
        let src = "fn prod() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn unit() {}\n\
                   }\n";
        // The `#[cfg(test)] mod tests` spans its attribute line through close.
        assert_eq!(cfg_ranges(src), [(2, 5)]);
    }

    #[test]
    fn cfg_any_test_is_matched_but_feature_is_not() {
        let any_test = "#[cfg(any(test, feature = \"x\"))]\nmod m { fn f() {} }\n";
        assert_eq!(cfg_ranges(any_test).len(), 1);
        let feature_only = "#[cfg(feature = \"test\")]\nmod m { fn f() {} }\n";
        assert!(cfg_ranges(feature_only).is_empty());
    }

    #[test]
    fn apply_cfg_test_exclusion_filters_inline_test_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = "crates/x/src/foo.rs";
        let src = "fn prod() {\n\
                   \x20\x20\x20\x20for _ in 0..1 {}\n\
                   }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn unit() {\n\
                       \x20\x20\x20\x20for _ in 0..1 {}\n\
                       }\n\
                   }\n";
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, src).expect("write fixture");

        let mut report = Report::default();
        report.extend([
            Violation::warn("fat_loop_body", format!("{rel}:2:for_body"), "prod"),
            Violation::warn("fat_loop_body", format!("{rel}:7:for_body"), "test"),
        ]);

        apply_cfg_test_exclusion(&mut report, tmp.path());

        let keys: Vec<&str> = report.violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, [format!("{rel}:2:for_body")]);
    }

    #[test]
    fn apply_cfg_test_exclusion_keeps_unparsable_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = "crates/x/src/broken.rs";
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "this is not valid rust ::: {{{").expect("write fixture");

        let mut report = Report::default();
        report.extend([Violation::warn(
            "fat_loop_body",
            format!("{rel}:3:for_body"),
            "kept",
        )]);

        apply_cfg_test_exclusion(&mut report, tmp.path());
        assert_eq!(report.violations.len(), 1);
    }

    #[test]
    fn non_test_line_count_subtracts_cfg_test_modules() {
        let src = "fn a() {}\n\
                   fn b() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn t1() {}\n\
                       fn t2() {}\n\
                   }\n";
        // 7 lines total, the `#[cfg(test)] mod` spans lines 3..=7 (5 lines).
        assert_eq!(non_test_line_count(src), 2);
    }

    #[test]
    fn non_test_line_count_matches_cfg_all_test_feature() {
        let src = "fn a() {}\n\
                   #[cfg(all(test, feature = \"x\"))]\n\
                   mod tests {\n\
                       fn t() {}\n\
                   }\n";
        // `all(test, feature)` is a test cfg: lines 2..=5 drop, leaving 1.
        assert_eq!(non_test_line_count(src), 1);
    }

    #[test]
    fn non_test_line_count_keeps_unparsable_and_test_free() {
        assert_eq!(non_test_line_count("not ::: valid {{{"), 1);
        assert_eq!(non_test_line_count("fn a() {}\nfn b() {}\n"), 2);
    }

    fn module_ranges(src: &str, patterns: &[&str]) -> Vec<(usize, usize)> {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let globs = compile_globs(
            &patterns
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
        );
        let mut out = Vec::new();
        collect_module_ranges(&file.items, &mut Vec::new(), &globs, &mut out);
        out.iter().map(|r| (*r.start(), *r.end())).collect()
    }

    #[test]
    fn collect_module_ranges_matches_leaf_and_nested_path() {
        let src = "mod keep {\n\
                   \x20\x20\x20\x20fn a() {}\n\
                   }\n\
                   mod outer {\n\
                       mod legacy {\n\
                       \x20\x20\x20\x20fn b() {}\n\
                       }\n\
                   }\n";
        assert_eq!(module_ranges(src, &["legacy"]), [(5, 7)]);
        assert_eq!(module_ranges(src, &["outer::legacy"]), [(5, 7)]);
        assert!(module_ranges(src, &["missing"]).is_empty());
    }

    #[test]
    fn apply_module_excludes_filters_named_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = "crates/x/src/foo.rs";
        let src = "fn prod() {\n\
                   \x20\x20\x20\x20for _ in 0..1 {}\n\
                   }\n\
                   mod legacy {\n\
                       fn old() {\n\
                       \x20\x20\x20\x20for _ in 0..1 {}\n\
                       }\n\
                   }\n";
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, src).expect("write fixture");

        let mut report = Report::default();
        report.extend([
            Violation::warn("fat_loop_body", format!("{rel}:2:for_body"), "prod"),
            Violation::warn("fat_loop_body", format!("{rel}:6:for_body"), "legacy"),
        ]);

        apply_module_excludes(&mut report, &["legacy".to_string()], tmp.path());

        let keys: Vec<&str> = report.violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, [format!("{rel}:2:for_body")]);
    }

    #[test]
    fn apply_module_excludes_empty_is_noop() {
        let mut report = Report::default();
        report.extend([Violation::warn(
            "fat_loop_body",
            "crates/x/src/foo.rs:6:for_body",
            "kept",
        )]);
        apply_module_excludes(&mut report, &[], Path::new("/nonexistent"));
        assert_eq!(report.violations.len(), 1);
    }
}
