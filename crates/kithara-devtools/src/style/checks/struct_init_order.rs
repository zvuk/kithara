use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, hash_map::Entry},
    mem,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use proc_macro2::{Span, TokenTree};
use syn::{
    ExprStruct, FieldValue, Fields, ItemEnum, ItemImpl, ItemStruct, Member, Token, Type,
    punctuated::Punctuated,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        fix::{BlockRange, ExpansionError, FixOutcome, SourceRewriter, expand_blocks},
        scan::Scan,
        violation::Violation,
        walker::relative_to,
    },
    style::config::StructInitOrderConfig,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "struct_init_order";
}

pub(crate) struct StructInitOrder;

impl Check for StructInitOrder {
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.struct_init_order;
        let mut outcome = FixOutcome::default();
        let files = ctx.scan.rs_files(ctx.scope)?;
        let decls = DeclIndex::from_files(ctx.scan, ctx.workspace_root, &files);
        for path in files.iter() {
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            let krate = crate_of(&rel);
            let mut skipped = HashSet::new();
            let mut wrote = false;
            for pass in 0.. {
                let Some(src) = ctx.scan.source(path) else {
                    break;
                };
                let Ok(file) = syn::parse_file(&src) else {
                    break;
                };
                let mut rw = SourceRewriter::new(&src);
                let mut pass_skipped = Vec::new();
                let mut visitor = FixVisitor {
                    cfg,
                    rel: &rel,
                    src: &src,
                    rw: &mut rw,
                    skipped: &mut pass_skipped,
                    decls: &decls,
                    krate: &krate,
                    self_ty: None,
                };
                visitor.visit_file(&file);
                skipped.extend(pass_skipped);
                if rw.is_empty() {
                    break;
                }
                if pass == cfg.max_fix_passes {
                    skipped.insert(format!("{rel}: nested reorder did not converge"));
                    break;
                }
                let new_src = rw
                    .finish()
                    .with_context(|| format!("{ID} fix failed for {rel}", ID = consts::ID))?;
                ctx.scan.write(path, new_src)?;
                wrote = true;
            }
            outcome.writes += usize::from(wrote);
            outcome.skipped.extend(skipped);
        }
        Ok(outcome)
    }

    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.struct_init_order;
        let mut violations = Vec::new();
        let files = ctx.scan.rs_files(ctx.scope)?;
        let decls = DeclIndex::from_files(ctx.scan, ctx.workspace_root, &files);
        for path in files.iter() {
            let Ok(file) = ctx.scan.parse_file(path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            let krate = crate_of(&rel);

            let mut v = InitVisitor {
                cfg,
                rel: &rel,
                out: &mut violations,
                decls: &decls,
                krate: &krate,
                self_ty: None,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct FixVisitor<'a, 'src> {
    decls: &'a DeclIndex,
    rw: &'a mut SourceRewriter<'src>,
    cfg: &'a StructInitOrderConfig,
    skipped: &'a mut Vec<String>,
    krate: &'a str,
    rel: &'a str,
    src: &'src str,
    self_ty: Option<String>,
}

impl<'ast> Visit<'ast> for FixVisitor<'_, '_> {
    /// A reordered struct literal's nested literals travel inside the moved blocks, so touching
    /// them now would stage overlapping edits; the caller's fix loop revisits them on the next
    /// pass.
    fn visit_expr_struct(&mut self, e: &'ast ExprStruct) {
        let order = expected_order(
            self.cfg,
            self.decls,
            (self.krate, self.self_ty.as_deref()),
            e,
        );
        match try_fix_expr_struct(self.src, e, order, self.rw) {
            Ok(true) => return,
            Ok(false) => {}
            Err(reason) => {
                self.skipped.push(format!(
                    "{}:{}: {reason}",
                    self.rel,
                    e.path.span().start().line
                ));
            }
        }
        visit::visit_expr_struct(self, e);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let outer = mem::replace(&mut self.self_ty, impl_self_name(item));
        visit::visit_item_impl(self, item);
        self.self_ty = outer;
    }
}

/// Reorder one `Foo { ... }` literal in place. Returns `Ok(true)` when a
/// reorder was staged, `Ok(false)` when the literal was already ordered or
/// too small to matter; returns `Err(reason)` only when the engine refused
/// (floating comment, missing trailing comma, `..base` rest, etc.) so the
/// caller can log it.
fn try_fix_expr_struct(
    src: &str,
    e: &ExprStruct,
    order: ExpectedOrder,
    rw: &mut SourceRewriter<'_>,
) -> Result<bool, String> {
    if e.fields.len() < 2 {
        return Ok(false);
    }
    if e.dot2_token.is_some() || e.rest.is_some() {
        return Err("contains `..base` rest".to_string());
    }

    if has_shorthand_use_def_conflict(&e.fields) {
        return Err("shorthand field is moved before another field reads it".to_string());
    }

    let ExpectedOrder {
        actual, expected, ..
    } = order;
    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return Ok(false);
    }

    let scope_start = e.brace_token.span.open().byte_range().end;
    let scope_end = e.brace_token.span.close().byte_range().start;

    let item_spans: Vec<Range<usize>> = e.fields.iter().map(|fv| fv.span().byte_range()).collect();

    let blocks = match expand_blocks(src, scope_start..scope_end, &item_spans) {
        Ok(b) => b,
        Err(ExpansionError::FloatingComment { line, snippet }) => {
            return Err(format!("floating comment at line {line}: `{snippet}`"));
        }
        Err(other) => return Err(format!("engine error: {other:?}")),
    };

    let texts: Vec<String> = blocks.iter().map(|b| block_with_comma(src, b)).collect();
    for (slot_idx, expected_key) in expected.iter().enumerate() {
        let source_idx = expected_key.idx;
        if source_idx == slot_idx {
            continue;
        }
        rw.replace(blocks[slot_idx].bytes.clone(), texts[source_idx].clone());
    }
    Ok(true)
}

/// The block's source with a `,` between its field and its trailing trivia,
/// whether or not the author wrote one there.
///
/// The last field of a literal is allowed to go without a comma, and a block
/// that moves out of the last slot needs one to keep the literal parsing.
/// The comma goes in ahead of any trailing comment, so the comment stays a
/// comment and a comma inside it separates nothing, and `just fmt` takes the
/// trailing one back off a literal that fits on its line.
fn block_with_comma(src: &str, block: &BlockRange) -> String {
    let tail = &src[block.item_bytes.end..block.bytes.end];
    let before_comment = tail
        .find("//")
        .or_else(|| tail.find("/*"))
        .unwrap_or(tail.len());
    if tail[..before_comment].contains(',') {
        return src[block.bytes.clone()].to_string();
    }
    format!(
        "{}{},{tail}",
        &src[block.bytes.start..block.item_bytes.start],
        &src[block.item_bytes.clone()],
    )
}

struct InitVisitor<'a> {
    decls: &'a DeclIndex,
    cfg: &'a StructInitOrderConfig,
    out: &'a mut Vec<Violation>,
    krate: &'a str,
    rel: &'a str,
    self_ty: Option<String>,
}

impl<'ast> Visit<'ast> for InitVisitor<'_> {
    fn visit_expr_struct(&mut self, e: &'ast ExprStruct) {
        let order = expected_order(
            self.cfg,
            self.decls,
            (self.krate, self.self_ty.as_deref()),
            e,
        );
        check_expr_struct(self.rel, e, order, self.out);
        visit::visit_expr_struct(self, e);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let outer = mem::replace(&mut self.self_ty, impl_self_name(item));
        visit::visit_item_impl(self, item);
        self.self_ty = outer;
    }
}

#[derive(Debug, Clone)]
struct InitKey {
    name: String,
    /// 0 = shorthand, 1 = explicit, 2 = unnamed/positional (sorts last).
    bucket: usize,
    idx: usize,
}

/// The fields of one literal as written and as they should read, with the
/// rule that decided the second.
struct ExpectedOrder {
    rule: &'static str,
    actual: Vec<InitKey>,
    expected: Vec<InitKey>,
}

/// How the literal's fields should read.
///
/// A literal whose fields are all shorthand carries no evaluation order to
/// preserve, so it reads in the order its type declares — the order
/// `clippy::inconsistent_struct_constructor` demands and the one
/// [`super::struct_field_order`] rewrites declarations into. Every other
/// literal keeps the shorthand-before-explicit rule, which says nothing
/// about the declaration and so cannot disagree with it.
fn expected_order(
    cfg: &StructInitOrderConfig,
    decls: &DeclIndex,
    scope: (&str, Option<&str>),
    e: &ExprStruct,
) -> ExpectedOrder {
    let (krate, self_ty) = scope;
    let actual: Vec<InitKey> = e
        .fields
        .iter()
        .enumerate()
        .map(|(idx, fv)| InitKey {
            idx,
            ..classify(cfg, fv)
        })
        .collect();
    let mut expected = actual.clone();
    let rule = if let Some(positions) =
        decls.positions(krate, declared_name(e, self_ty).as_deref(), &e.fields)
    {
        expected.sort_by_key(|key| positions[key.idx]);
        "follow the order its type declares"
    } else {
        expected.sort_by(cmp_init_key);
        "put shorthand fields before explicit ones"
    };
    ExpectedOrder {
        rule,
        actual,
        expected,
    }
}

/// The field order every named-field declaration in one file gives, keyed by
/// the name a literal writes in front of the brace.
///
/// A name two declarations share resolves to nothing unless they agree: the
/// literal names one of them and the index cannot say which. A literal whose
/// fields are not the fields of the declaration found under its name is
/// reading some other type, and orders by the fallback rule.
#[derive(Default)]
struct DeclIndex {
    by_name: HashMap<(String, String), Option<Vec<String>>>,
}

impl DeclIndex {
    fn absorb(&mut self, krate: &str, file: &syn::File) {
        DeclCollector { krate, index: self }.visit_file(file);
    }

    /// The declarations of a whole scope. A literal names its type without
    /// saying where the type lives, and the file it lives in is rarely the
    /// file that writes the literal.
    fn from_files(scan: &Scan, root: &Path, paths: &[PathBuf]) -> Self {
        let mut index = Self::default();
        for path in paths {
            if let Ok(file) = scan.parse_file(path) {
                let rel = relative_to(root, path).to_string_lossy().replace('\\', "/");
                index.absorb(&crate_of(&rel), &file);
            }
        }
        index
    }

    fn insert(&mut self, krate: &str, name: String, fields: &Fields) {
        let Fields::Named(named) = fields else {
            return;
        };
        let order: Vec<String> = named
            .named
            .iter()
            .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
            .collect();
        match self.by_name.entry((krate.to_string(), name)) {
            Entry::Occupied(mut slot) => {
                if slot.get().as_ref() != Some(&order) {
                    slot.insert(None);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(Some(order));
            }
        }
    }

    /// Where each field of the literal sits in the declaration, or `None`
    /// when the declaration cannot decide the order: the literal spells an
    /// initializer out (whose evaluation order is the author's), or names a
    /// type this file does not declare, or names one it declares twice.
    fn positions(
        &self,
        krate: &str,
        name: Option<&str>,
        fields: &Punctuated<FieldValue, Token![,]>,
    ) -> Option<Vec<usize>> {
        if fields.iter().any(|fv| fv.colon_token.is_some()) {
            return None;
        }
        let order = self
            .by_name
            .get(&(krate.to_string(), name?.to_string()))?
            .as_ref()?;
        if fields.len() != order.len() {
            return None;
        }
        fields
            .iter()
            .map(|fv| match &fv.member {
                Member::Named(id) => {
                    let name = id.to_string();
                    order.iter().position(|field| *field == name)
                }
                Member::Unnamed(_) => None,
            })
            .collect()
    }
}

struct DeclCollector<'a> {
    index: &'a mut DeclIndex,
    krate: &'a str,
}

/// The crate a workspace-relative path belongs to. A literal resolves against
/// the declarations of its own crate: a name two crates both spell names two
/// types, and no literal means both.
fn crate_of(rel: &str) -> String {
    let mut parts = rel.split('/');
    match (parts.next(), parts.next()) {
        (Some("crates"), Some(name)) => format!("crates/{name}"),
        (Some(root), _) => root.to_string(),
        (None, _) => rel.to_string(),
    }
}

impl<'ast> Visit<'ast> for DeclCollector<'_> {
    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        for variant in &item.variants {
            self.index
                .insert(self.krate, variant.ident.to_string(), &variant.fields);
        }
        visit::visit_item_enum(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.index
            .insert(self.krate, item.ident.to_string(), &item.fields);
        visit::visit_item_struct(self, item);
    }
}

/// The name under which [`DeclIndex`] holds the literal's declaration, when
/// the literal names one this file can hold: a bare `Foo { ... }`, or
/// `Self { ... }` inside an `impl`. A qualified path names a declaration that
/// may live in another file, where this index cannot see it.
fn declared_name(e: &ExprStruct, self_ty: Option<&str>) -> Option<String> {
    if e.path.leading_colon.is_some() || e.path.segments.len() != 1 {
        return None;
    }
    let ident = e.path.segments.first()?.ident.to_string();
    if ident == "Self" {
        return self_ty.map(ToString::to_string);
    }
    Some(ident)
}

fn impl_self_name(item: &ItemImpl) -> Option<String> {
    match item.self_ty.as_ref() {
        Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn cmp_init_key(a: &InitKey, b: &InitKey) -> Ordering {
    a.bucket.cmp(&b.bucket).then_with(|| a.idx.cmp(&b.idx))
}

fn classify(cfg: &StructInitOrderConfig, fv: &FieldValue) -> InitKey {
    let is_shorthand = fv.colon_token.is_none();
    let bucket = match &fv.member {
        Member::Unnamed(_) => 2,
        Member::Named(_) if is_shorthand && cfg.shorthand_first => 0,
        Member::Named(_) => 1,
    };
    let name = match &fv.member {
        Member::Named(id) => id.to_string(),
        Member::Unnamed(i) => i.index.to_string(),
    };
    InitKey {
        bucket,
        name,
        idx: 0,
    }
}

/// Mirrors the autofix safety model: a `..base` rest, or a shorthand field read by an earlier
/// explicit initializer, makes reordering to declaration order a use-after-move, so detection
/// refuses these the same way the fix does.
fn check_expr_struct(rel: &str, e: &ExprStruct, order: ExpectedOrder, out: &mut Vec<Violation>) {
    if e.fields.len() < 2 {
        return;
    }
    if e.dot2_token.is_some() || e.rest.is_some() {
        return;
    }
    if has_shorthand_use_def_conflict(&e.fields) {
        return;
    }
    let ExpectedOrder {
        actual,
        expected,
        rule,
    } = order;

    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return;
    }

    let type_name = e
        .path
        .segments
        .last()
        .map_or_else(|| "<anon>".to_string(), |s| s.ident.to_string());
    let line = type_span_start_line(e);
    let key = format!("{rel}:{line}::{type_name}");
    let actual_summary = actual
        .iter()
        .map(|k| k.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let expected_summary = expected
        .iter()
        .map(|k| k.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let msg = format!(
        "init `{type_name} {{ ... }}` should {rule}: \
         expected [{expected_summary}], found [{actual_summary}]"
    );
    out.push(Violation::warn(consts::ID, key, msg));
}

/// True iff some shorthand field name is referenced by another field's
/// explicit expression. In that case, moving the shorthand first would
/// move/borrow the value before the explicit expression runs, which is
/// a compile error or behaviour change. The check is conservative: it
/// only inspects identifier references, not field-access paths or
/// method-call receivers (those start with the same identifier).
fn has_shorthand_use_def_conflict(fields: &Punctuated<FieldValue, Token![,]>) -> bool {
    let shorthand_names: HashSet<String> = fields
        .iter()
        .filter(|fv| fv.colon_token.is_none())
        .filter_map(|fv| match &fv.member {
            Member::Named(id) => Some(id.to_string()),
            Member::Unnamed(_) => None,
        })
        .collect();
    if shorthand_names.is_empty() {
        return false;
    }
    for fv in fields.iter().filter(|fv| fv.colon_token.is_some()) {
        let mut v = IdentScanner {
            names: &shorthand_names,
            found: false,
        };
        v.visit_expr(&fv.expr);
        if v.found {
            return true;
        }
    }
    false
}

struct IdentScanner<'a> {
    names: &'a HashSet<String>,
    found: bool,
}

impl<'ast> Visit<'ast> for IdentScanner<'_> {
    /// `Visit` does not parse macro bodies, so a `vec![x.clone()]` read of a shorthand field is
    /// invisible to `visit_path`; the raw token stream is scanned instead.
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if !self.found && tokens_contain_ident(m.tokens.clone(), self.names) {
            self.found = true;
        }
        visit::visit_macro(self, m);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if let Some(first) = p.segments.first()
            && p.leading_colon.is_none()
            && self.names.contains(&first.ident.to_string())
        {
            self.found = true;
            return;
        }
        visit::visit_path(self, p);
    }
}

fn tokens_contain_ident(tokens: proc_macro2::TokenStream, names: &HashSet<String>) -> bool {
    tokens.into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => names.contains(&id.to_string()),
        TokenTree::Group(g) => tokens_contain_ident(g.stream(), names),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn type_span_start_line(e: &ExprStruct) -> usize {
    let span = e
        .path
        .segments
        .first()
        .map_or_else(Span::call_site, Spanned::span);
    span.start().line
}

#[cfg(test)]
mod fix_tests {
    use std::collections::BTreeMap;

    use syn::visit::Visit;

    use super::*;

    fn default_cfg() -> StructInitOrderConfig {
        StructInitOrderConfig {
            shorthand_first: true,
            ..StructInitOrderConfig::default()
        }
    }

    /// Apply the visitor-driven fix to a snippet wrapped in a fn body so
    /// `syn::parse_file` accepts it. Returns the rewritten snippet
    /// (still wrapped) and the list of skip reasons collected.
    fn run_fix(src: &str) -> (String, Vec<String>) {
        let cfg = default_cfg();
        let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse failed: {e}\n---\n{src}"));
        let mut decls = DeclIndex::default();
        decls.absorb("fixture", &file);
        let mut rw = SourceRewriter::new(src);
        let mut skipped = Vec::new();
        let mut visitor = FixVisitor {
            src,
            cfg: &cfg,
            rel: "fixture.rs",
            rw: &mut rw,
            skipped: &mut skipped,
            decls: &decls,
            krate: "fixture",
            self_ty: None,
        };
        visitor.visit_file(&file);
        let out = if rw.is_empty() {
            src.to_string()
        } else {
            rw.finish().expect("rewriter finish")
        };
        (out, skipped)
    }

    /// Count line-comment occurrences as a multiset (I1 invariant check).
    fn comment_multiset(src: &str) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") {
                *counts.entry(t.trim_end().to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    #[test]
    fn a_field_left_without_a_comma_gets_one_when_it_moves() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo { a, b };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Foo { b, a, }"),
            "the last field's missing comma must not block the reorder, got: {out}"
        );
    }

    #[test]
    fn a_comma_inside_a_trailing_comment_separates_nothing() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo {
        a,
        b // one, two
    };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("b, // one, two"),
            "the separator goes in ahead of the comment, got: {out}"
        );
        assert_eq!(
            comment_multiset(&out),
            comment_multiset(src),
            "I1: comments are preserved"
        );
    }

    #[test]
    fn an_all_shorthand_literal_follows_its_declaration() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo { a, b, };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Foo { b, a, }"),
            "literal must read as the struct declares, got: {out}"
        );
    }

    #[test]
    fn a_self_literal_follows_the_declaration_of_its_impl() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
impl Foo {
    fn new(a: u8, b: u8) -> Self {
        Self { a, b, }
    }
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Self { b, a, }"),
            "`Self` must resolve to the type of the impl, got: {out}"
        );
    }

    #[test]
    fn spelled_out_initializers_keep_their_evaluation_order() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let _ = Foo { a: first(), b: second(), };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(
            out, src,
            "an initializer may have effects the order carries"
        );
    }

    #[test]
    fn a_name_two_declarations_disagree_on_orders_nothing() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
mod other {
    struct Foo {
        a: u8,
        b: u8,
    }
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo { a, b, };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(out, src, "the literal names one of two declarations");
    }

    #[test]
    fn shorthand_after_explicit_is_swapped() {
        let src = "fn main() { let _ = Foo { x: 1, y, }; }";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Foo { y, x: 1, }"),
            "expected shorthand first, got: {out}"
        );
    }

    #[test]
    fn nested_literal_inside_moved_field_defers_then_converges() {
        let src = "fn main() { let _ = Foo { x: Bar { b: 2, a, }, y, }; }";
        let (pass1, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            pass1.contains("Foo { y, x: Bar { b: 2, a, }, }"),
            "outer reordered, inner deferred: {pass1}"
        );
        let (pass2, skipped) = run_fix(&pass1);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            pass2.contains("Foo { y, x: Bar { a, b: 2, }, }"),
            "inner reordered on the next pass: {pass2}"
        );
    }

    #[test]
    fn shorthand_read_inside_macro_blocks_reorder() {
        let src = "fn main() { let _ = Foo { nodes: vec![sig.clone()], sig, }; }";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "macro read of `sig` must block the reorder");
        assert_eq!(skipped.len(), 1, "skipped: {skipped:?}");
        assert!(
            skipped[0].contains("shorthand field is moved"),
            "{skipped:?}"
        );
    }

    #[test]
    fn already_ordered_is_no_op() {
        let src = "fn main() { let _ = Foo { y, x: 1, }; }";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn single_field_is_no_op() {
        let src = "fn main() { let _ = Foo { x: 1, }; }";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn rest_base_is_skipped() {
        let src = "fn main() { let base = Foo::default(); let _ = Foo { x: 1, y, ..base }; }";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not modify");
        assert!(
            skipped.iter().any(|s| s.contains("..base")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn comments_move_with_their_field() {
        let src = "\
fn main() {
    let _ = Foo {
        // doc for x
        x: 1,
        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        let y_pos = out.find("y,").expect("y,");
        let x_pos = out.find("x: 1,").expect("x: 1,");
        let comment_pos = out.find("// doc for x").expect("comment");
        assert!(
            y_pos < comment_pos && comment_pos < x_pos,
            "comment did not move with x:\n{out}"
        );
    }

    #[test]
    fn floating_comment_is_skipped() {
        let src = "\
fn main() {
    let _ = Foo {
        x: 1,

        // floating comment
        // another

        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not modify when comments are floating");
        assert!(
            skipped.iter().any(|s| s.contains("floating")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn comment_attached_to_next_field_is_carried() {
        let src = "\
fn main() {
    let _ = Foo {
        x: 1,

        // glued to y
        // also glued
        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        let comment_pos = out.find("// glued to y").expect("comment");
        let x_pos = out.find("x: 1,").expect("x: 1,");
        assert!(comment_pos < x_pos, "comment did not travel with y:\n{out}");
    }

    #[test]
    fn missing_trailing_comma_still_reorders() {
        let src = "fn main() { let _ = Foo { x: 1, y }; }";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Foo { y, x: 1, }"),
            "the shorthand rule reaches a literal the author left uncommaed: {out}"
        );
    }

    #[test]
    fn idempotent_run() {
        let src = "fn main() { let _ = Foo { x: 1, y, z: 2, }; }";
        let (after_first, skipped1) = run_fix(src);
        assert!(skipped1.is_empty());
        let (after_second, skipped2) = run_fix(&after_first);
        assert_eq!(after_first, after_second, "I2: idempotency violated");
        assert!(skipped2.is_empty());
    }

    #[test]
    fn shorthand_use_def_conflict_is_skipped() {
        let src = "\
fn make(pcm: Pcm) -> Self {
    Self {
        spec: pcm.spec(),
        pcm,
    }
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not reorder when a use-def conflict exists");
        assert!(
            skipped.iter().any(|s| s.contains("moved before")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn unrelated_shorthand_still_swapped() {
        let src = "\
fn make(value: u32, label: String) -> Self {
    Self {
        label: label.clone(),
        value,
    }
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let value_pos = out.find("value,").expect("value");
        let label_pos = out.find("label: label.clone()").expect("label");
        assert!(value_pos < label_pos, "shorthand first:\n{out}");
    }
}

#[cfg(test)]
mod detect_tests {
    use syn::visit::Visit;

    use super::*;

    fn default_cfg() -> StructInitOrderConfig {
        StructInitOrderConfig {
            shorthand_first: true,
            ..StructInitOrderConfig::default()
        }
    }

    /// Run the detection visitor over a snippet and return the keys of the
    /// violations it reports.
    fn detect(src: &str) -> Vec<String> {
        let cfg = default_cfg();
        let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse failed: {e}\n---\n{src}"));
        let mut decls = DeclIndex::default();
        decls.absorb("fixture", &file);
        let mut out = Vec::new();
        let mut v = InitVisitor {
            cfg: &cfg,
            rel: "fixture.rs",
            out: &mut out,
            decls: &decls,
            krate: "fixture",
            self_ty: None,
        };
        v.visit_file(&file);
        out.into_iter().map(|viol| viol.key).collect()
    }

    #[test]
    fn an_all_shorthand_literal_out_of_declaration_order_is_flagged() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo { a, b, };
}
";
        assert_eq!(
            detect(src).len(),
            1,
            "the order clippy reads must be the order the ratchet reads"
        );
    }

    #[test]
    fn an_all_shorthand_literal_in_declaration_order_is_accepted() {
        let src = "\
struct Foo {
    b: u8,
    a: u8,
}
fn main() {
    let (a, b) = (1, 2);
    let _ = Foo { b, a, };
}
";
        assert!(detect(src).is_empty(), "already declaration-ordered");
    }

    #[test]
    fn out_of_order_explicit_is_flagged() {
        let src = "fn main() { let _ = Foo { x: 1, y, }; }";
        assert_eq!(
            detect(src).len(),
            1,
            "genuinely reorderable literal must fire"
        );
    }

    #[test]
    fn shorthand_use_def_conflict_is_not_flagged() {
        let src = "\
fn make(pcm: Pcm) -> Self {
    Self {
        spec: pcm.spec(),
        pcm,
    }
}
";
        assert!(
            detect(src).is_empty(),
            "reordering would move `pcm` before `spec` reads it — must not flag"
        );
    }

    #[test]
    fn rest_base_is_not_flagged() {
        let src = "fn main() { let base = Foo::default(); let _ = Foo { x: 1, y, ..base }; }";
        assert!(
            detect(src).is_empty(),
            "`..base` literals are skipped by the fix and must not be flagged"
        );
    }

    #[test]
    fn ui_state_use_def_conflict_is_not_flagged() {
        let src = "\
fn build(ui_state: UiState, controller: Controller) -> Self {
    Self {
        controller,
        previous_volume: ui_state.volume.max(0.01),
        ui_state,
    }
}
";
        assert!(
            detect(src).is_empty(),
            "shorthand `ui_state` is read by an earlier explicit field — must not flag"
        );
    }

    #[test]
    fn unrelated_shorthand_is_still_flagged() {
        let src = "\
fn make(value: u32, label: String) -> Self {
    Self {
        label: label.clone(),
        value,
    }
}
";
        assert_eq!(
            detect(src).len(),
            1,
            "no use-def conflict: `value` is not read by `label`'s init — must still flag"
        );
    }
}
