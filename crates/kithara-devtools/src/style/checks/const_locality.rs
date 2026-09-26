use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::Result;
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::{
    Expr, GenericArgument, ImplItem, Item, ItemConst, ItemImpl, Lit, Meta, PathArguments, Type,
    Visibility,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    fix::{FixOutcome, SourceRewriter, deletion_range, leading_trivia_start, line_start},
    parse::self_ty_name,
    scope::Scope,
    violation::Violation,
    walker::relative_to,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "const_locality";
}

pub(crate) struct ConstLocality;

impl Check for ConstLocality {
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let mut outcome = FixOutcome::default();
        for file in analyze(ctx)? {
            let Some(src) = ctx.scan.source(&file.path) else {
                continue;
            };
            let mut rw = SourceRewriter::new(&src);
            stage_moves(&src, &file.findings, &mut rw);
            for finding in &file.findings {
                match &finding.locality {
                    Locality::SingleFn { label, .. } => outcome
                        .changes
                        .push(format!("{}: `{}` into `{label}`", file.rel, finding.name)),
                    Locality::SingleImpl(target) => outcome.skipped.push(format!(
                        "{}: `{}` is shared by the methods of `impl {target}`; choose \
                         where it lives by hand",
                        file.rel, finding.name
                    )),
                }
            }
            if !rw.is_empty() {
                ctx.scan.write(&file.path, rw.finish()?)?;
                outcome.writes += 1;
            }
        }
        Ok(outcome)
    }

    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let mut violations: Vec<Violation> = analyze(ctx)?
            .iter()
            .flat_map(|file| file.findings.iter().map(Finding::violation))
            .collect();
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct ParsedFile {
    file: syn::File,
    names: HashSet<String>,
    path: PathBuf,
    crate_key: String,
    rel: String,
    in_scope: bool,
}

struct FileFindings {
    path: PathBuf,
    rel: String,
    findings: Vec<Finding>,
}

/// One const that a single owner keeps to itself.
struct Finding {
    /// Source lines inside a multi-line literal of the const, which a move
    /// must carry verbatim rather than re-indent.
    literal_lines: BTreeSet<usize>,
    locality: Locality,
    vis: Option<Range<usize>>,
    item: Range<usize>,
    key: String,
    name: String,
    /// Where the container holding the const opens: the file start, or just
    /// past the `{` of the inline module.
    container: usize,
}

impl Finding {
    fn violation(&self) -> Violation {
        let msg = match &self.locality {
            Locality::SingleImpl(target) => format!(
                "L2: const `{}` is referenced only by methods of `impl {target}`; \
                 move it into that impl block (accessed as `Self::{}`)",
                self.name, self.name
            ),
            Locality::SingleFn { label, .. } => format!(
                "L1: const `{}` is referenced only from `{label}`; move it \
                 inside that function as a local `const`",
                self.name
            ),
        };
        Violation::warn(consts::ID, self.key.clone(), msg)
    }
}

/// Parse every scoped file and every other file of the crates they belong
/// to, then report the consts of the scoped files. A const is only local
/// when no other file of its crate names it, so the crate is read whole even
/// when the scope is narrower: a narrower read would move a const out from
/// under a file it did not see.
fn analyze(ctx: &Context<'_>) -> Result<Vec<FileFindings>> {
    let scoped: HashSet<PathBuf> = ctx.scan.rs_files(ctx.scope)?.iter().cloned().collect();
    let crates: HashSet<String> = scoped
        .iter()
        .map(|path| crate_key_for(ctx.workspace_root, path))
        .collect();
    let mut files: Vec<ParsedFile> = Vec::new();
    for path in ctx.scan.rs_files(&Scope::default())?.iter() {
        let crate_key = crate_key_for(ctx.workspace_root, path);
        if !crates.contains(&crate_key) {
            continue;
        }
        let Ok(file) = ctx.scan.parse_file(path) else {
            continue;
        };
        let rel = relative_to(ctx.workspace_root, path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut names = HashSet::new();
        let mut collector = NameCollector { names: &mut names };
        collector.visit_file(&file);
        files.push(ParsedFile {
            file,
            names,
            crate_key,
            rel,
            in_scope: scoped.contains(path),
            path: path.clone(),
        });
    }

    let mut out = Vec::new();
    for (idx, pf) in files.iter().enumerate().filter(|(_, pf)| pf.in_scope) {
        let external: HashSet<&str> = files
            .iter()
            .enumerate()
            .filter(|(j, other)| *j != idx && other.crate_key == pf.crate_key)
            .flat_map(|(_, other)| other.names.iter().map(String::as_str))
            .collect();
        let findings = analyze_file(&pf.rel, &pf.file, &external);
        if !findings.is_empty() {
            out.push(FileFindings {
                findings,
                path: pf.path.clone(),
                rel: pf.rel.clone(),
            });
        }
    }
    Ok(out)
}

/// Nearest-ancestor crate directory (the dir holding `Cargo.toml`), relative
/// to the workspace root. Files in different crates that happen to share a
/// const name never collide; same-crate files do.
fn crate_key_for(workspace_root: &Path, file: &Path) -> String {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() {
            return relative_to(workspace_root, d)
                .to_string_lossy()
                .replace('\\', "/");
        }
        dir = d.parent();
    }
    String::new()
}

/// A const is only "local to one fn" when every reference to its name is an expression-position use
/// inside a single fn/method body in the const's own module; any other reference makes the locality
/// claim unprovable, so analysis stays conservative and skips it.
fn analyze_file(rel: &str, file: &syn::File, external: &HashSet<&str>) -> Vec<Finding> {
    let mut consts: Vec<ConstSite> = Vec::new();
    collect_consts(&file.items, 0, &mut Vec::new(), &mut consts);

    let mut findings = Vec::new();
    for site in consts {
        if external.contains(site.name.as_str()) {
            continue;
        }

        let mut analyzer = RefAnalyzer::new(&site.name, &site.mod_path);
        analyzer.visit_file(file);
        if analyzer.disqualified {
            continue;
        }
        let Some(locality) = classify(&analyzer.owners) else {
            continue;
        };

        let mod_prefix = if site.mod_path.is_empty() {
            String::new()
        } else {
            format!("{}::", site.mod_path.join("::"))
        };
        findings.push(Finding {
            locality,
            key: format!("{rel}::{mod_prefix}{}", site.name),
            literal_lines: site.literal_lines,
            item: site.item,
            container: site.container,
            vis: site.vis,
            name: site.name,
        });
    }
    findings
}

struct ConstSite {
    literal_lines: BTreeSet<usize>,
    vis: Option<Range<usize>>,
    item: Range<usize>,
    name: String,
    mod_path: Vec<String>,
    container: usize,
}

fn collect_consts(
    items: &[Item],
    container: usize,
    mod_path: &mut Vec<String>,
    out: &mut Vec<ConstSite>,
) {
    for item in items {
        match item {
            Item::Const(c) if is_intra_crate(&c.vis) => out.push(ConstSite {
                container,
                literal_lines: literal_lines(c),
                item: c.span().byte_range(),
                vis: (!matches!(c.vis, Visibility::Inherited)).then(|| c.vis.span().byte_range()),
                name: c.ident.to_string(),
                mod_path: mod_path.clone(),
            }),
            Item::Mod(m) => {
                if let Some((brace, inner)) = &m.content {
                    mod_path.push(m.ident.to_string());
                    collect_consts(inner, brace.span.open().byte_range().end, mod_path, out);
                    mod_path.pop();
                }
            }
            _ => {}
        }
    }
}

/// Source lines a multi-line literal of `item` continues onto. Their text is
/// part of the value, so re-indenting them would change it.
fn literal_lines(item: &ItemConst) -> BTreeSet<usize> {
    fn walk(tokens: TokenStream, out: &mut BTreeSet<usize>) {
        for tt in tokens {
            match tt {
                TokenTree::Literal(lit) => {
                    let span = lit.span();
                    out.extend(span.start().line + 1..=span.end().line);
                }
                TokenTree::Group(g) => walk(g.stream(), out),
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(item.to_token_stream(), &mut out);
    out
}

fn is_intra_crate(vis: &Visibility) -> bool {
    match vis {
        Visibility::Inherited => true,
        Visibility::Restricted(r) => r.path.is_ident("crate") || r.path.is_ident("super"),
        Visibility::Public(_) => false,
    }
}

/// Moves the const, with the comments attached above it, to the top of the
/// body that opens at `body`, dropping its visibility and re-indenting it to
/// that body.
/// Move every const a single fn owns into that fn. Consts bound for one body
/// are inserted together, so they stay one block.
fn stage_moves(src: &str, findings: &[Finding], rw: &mut SourceRewriter<'_>) {
    let mut moved: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for finding in findings {
        if let Locality::SingleFn { body, .. } = finding.locality {
            let text = stage_removal(src, finding, body, rw);
            moved.entry(body).or_default().push(text);
        }
    }
    for (body, consts) in moved {
        rw.replace(body..body, format!("\n{}\n", consts.join("\n")));
    }
}

/// Stage the removal of `finding` and return its text re-indented for the
/// body opening at `body`.
fn stage_removal(src: &str, finding: &Finding, body: usize, rw: &mut SourceRewriter<'_>) -> String {
    let start = line_start(
        src,
        leading_trivia_start(src, finding.item.start, finding.container),
    );
    let rest = &src[finding.item.end..];
    let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
    let end = if line.trim_start().starts_with("//") {
        finding.item.end + line.len()
    } else {
        finding.item.end
    };
    rw.replace(deletion_range(src, start..end), "");

    let mut text = String::from(&src[start..end]);
    if let Some(vis) = &finding.vis {
        let after = &src[vis.end..end];
        let gap = after.len() - after.trim_start().len();
        text.replace_range(vis.start - start..vis.end + gap - start, "");
    }
    let from = &src[start..start + indent_len(&src[start..])];
    let body_line = line_start(src, body);
    let to = format!(
        "{}    ",
        &src[body_line..body_line + indent_len(&src[body_line..])]
    );
    let first_line = src[..start].matches('\n').count() + 1;
    text.split('\n')
        .enumerate()
        .map(|(offset, line)| {
            if finding.literal_lines.contains(&(first_line + offset)) || line.is_empty() {
                return line.to_owned();
            }
            line.strip_prefix(from)
                .map_or_else(|| line.to_owned(), |rest| format!("{to}{rest}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn indent_len(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

#[derive(Debug, Clone)]
enum Owner {
    TopFn {
        name: String,
        body: usize,
    },
    ImplMethod {
        impl_id: usize,
        target: String,
        method: String,
        body: usize,
    },
}

impl Owner {
    /// Byte offset just past the `{` that opens the owning body; it names the
    /// body uniquely.
    const fn body(&self) -> usize {
        match self {
            Self::TopFn { body, .. } | Self::ImplMethod { body, .. } => *body,
        }
    }
}

#[derive(Debug)]
enum Locality {
    SingleFn { label: String, body: usize },
    SingleImpl(String),
}

/// `None` when the const is unused or spread over several owners.
fn classify(owners: &[Owner]) -> Option<Locality> {
    if let [owner] = owners {
        return Some(Locality::SingleFn {
            label: format_owner(owner),
            body: owner.body(),
        });
    }
    let mut impl_ids: BTreeMap<usize, &str> = BTreeMap::new();
    for o in owners {
        match o {
            Owner::TopFn { .. } => return None,
            Owner::ImplMethod {
                impl_id, target, ..
            } => {
                impl_ids.insert(*impl_id, target.as_str());
            }
        }
    }
    match impl_ids.into_values().collect::<Vec<_>>().as_slice() {
        [target] => Some(Locality::SingleImpl((*target).to_owned())),
        _ => None,
    }
}

fn format_owner(o: &Owner) -> String {
    match o {
        Owner::TopFn { name, .. } => format!("fn {name}"),
        Owner::ImplMethod { target, method, .. } => format!("impl {target} :: {method}"),
    }
}

/// Walks an entire file and records, for one const name, every reference
/// site. References found in an expression position inside a single fn or
/// impl-method body are recorded as `owners`. Any reference in a
/// disqualifying position — fn signature, type / const-generic / array
/// length, macro or attribute token stream, doc comment, or a non-fn item
/// — sets `disqualified`, so the caller refuses to flag the const.
struct RefAnalyzer<'a> {
    /// Module path where the const is declared. An expression-position
    /// reference from a *different* module is a cross-module use, not a
    /// fn-local one.
    const_mod_path: &'a [String],
    name: &'a str,
    /// `Some` while visiting a fn/method block body; carries that body's owner.
    current_owner: Option<Owner>,
    /// Module path currently being visited.
    current_mod_path: Vec<String>,
    owners: Vec<Owner>,
    disqualified: bool,
    impl_counter: usize,
    /// `> 0` while inside a type / const-generic / array-length context.
    type_depth: usize,
}

impl<'a> RefAnalyzer<'a> {
    const fn new(name: &'a str, const_mod_path: &'a [String]) -> Self {
        Self {
            name,
            const_mod_path,
            owners: Vec::new(),
            disqualified: false,
            impl_counter: 0,
            current_owner: None,
            current_mod_path: Vec::new(),
            type_depth: 0,
        }
    }

    fn record_owner(&mut self) {
        if self.current_mod_path != self.const_mod_path {
            self.disqualified = true;
            return;
        }
        match &self.current_owner {
            Some(owner) => {
                if !self.owners.iter().any(|o| o.body() == owner.body()) {
                    self.owners.push(owner.clone());
                }
            }
            None => self.disqualified = true,
        }
    }

    fn tokens_mention_name(&self, tokens: &TokenStream) -> bool {
        token_stream_mentions(tokens, self.name)
    }
}

impl<'ast> Visit<'ast> for RefAnalyzer<'_> {
    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if attr_mentions_name(a, self.name) {
            self.disqualified = true;
        }
        visit::visit_attribute(self, a);
    }

    fn visit_expr_repeat(&mut self, e: &'ast syn::ExprRepeat) {
        self.visit_expr(&e.expr);
        self.type_depth += 1;
        self.visit_expr(&e.len);
        self.type_depth -= 1;
    }

    fn visit_generic_argument(&mut self, arg: &'ast GenericArgument) {
        if let GenericArgument::Const(_) = arg {
            self.type_depth += 1;
            visit::visit_generic_argument(self, arg);
            self.type_depth -= 1;
        } else {
            visit::visit_generic_argument(self, arg);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if self.disqualified {
            return;
        }
        for attr in &f.attrs {
            self.visit_attribute(attr);
        }
        self.visit_signature(&f.sig);
        let saved = self.current_owner.take();
        self.current_owner = Some(Owner::TopFn {
            name: f.sig.ident.to_string(),
            body: f.block.brace_token.span.open().byte_range().end,
        });
        self.visit_block(&f.block);
        self.current_owner = saved;
    }

    fn visit_item_impl(&mut self, im: &'ast ItemImpl) {
        if self.disqualified {
            return;
        }
        let impl_id = self.impl_counter;
        self.impl_counter += 1;
        let target = self_ty_name(&im.self_ty).unwrap_or_else(|| format!("<impl#{impl_id}>"));
        self.type_depth += 1;
        self.visit_type(&im.self_ty);
        self.type_depth -= 1;
        for it in &im.items {
            match it {
                ImplItem::Fn(method) => {
                    for attr in &method.attrs {
                        self.visit_attribute(attr);
                    }
                    self.visit_signature(&method.sig);
                    let saved = self.current_owner.take();
                    self.current_owner = Some(Owner::ImplMethod {
                        impl_id,
                        target: target.clone(),
                        method: method.sig.ident.to_string(),
                        body: method.block.brace_token.span.open().byte_range().end,
                    });
                    self.visit_block(&method.block);
                    self.current_owner = saved;
                }
                other => visit::visit_impl_item(self, other),
            }
        }
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if self.disqualified {
            return;
        }
        for attr in &m.attrs {
            self.visit_attribute(attr);
        }
        if let Some((_, inner)) = &m.content {
            self.current_mod_path.push(m.ident.to_string());
            for it in inner {
                self.visit_item(it);
            }
            self.current_mod_path.pop();
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.tokens_mention_name(&m.tokens) {
            self.disqualified = true;
        }
        visit::visit_macro(self, m);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if self.disqualified {
            return;
        }
        let is_match = p.segments.last().is_some_and(|s| s.ident == self.name);
        if is_match {
            if self.type_depth > 0 {
                self.disqualified = true;
            } else {
                self.record_owner();
            }
        }
        for seg in &p.segments {
            if let PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    self.visit_generic_argument(arg);
                }
            }
        }
    }

    fn visit_type(&mut self, t: &'ast Type) {
        self.type_depth += 1;
        visit::visit_type(self, t);
        self.type_depth -= 1;
    }
}

fn token_stream_mentions(tokens: &TokenStream, name: &str) -> bool {
    tokens.clone().into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => id == name,
        TokenTree::Group(g) => token_stream_mentions(&g.stream(), name),
        TokenTree::Literal(lit) => format_captures(&lit.to_string()).any(|c| c == name),
        TokenTree::Punct(_) => false,
    })
}

/// The identifiers a format string captures inline — `{NAME}`, `{NAME:>4}` —
/// which name a binding from inside a string literal, where no path is seen.
/// An escaped `{{` opens no capture.
fn format_captures(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        loop {
            let open = rest.find('{')?;
            let after = &rest[open + 1..];
            if let Some(escaped) = after.strip_prefix('{') {
                rest = escaped;
                continue;
            }
            let len = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            rest = &after[len..];
            if len > 0 && (rest.starts_with('}') || rest.starts_with(':')) {
                return Some(&after[..len]);
            }
        }
    })
}

fn attr_mentions_name(a: &syn::Attribute, name: &str) -> bool {
    match &a.meta {
        Meta::Path(_) => false,
        Meta::List(list) => token_stream_mentions(&list.tokens, name),
        Meta::NameValue(nv) => match &nv.value {
            Expr::Lit(lit) => match &lit.lit {
                Lit::Str(s) => s.value().contains(name),
                _ => false,
            },
            other => token_stream_mentions(&other.to_token_stream(), name),
        },
    }
}

fn collect_token_idents(tokens: &TokenStream, out: &mut HashSet<String>) {
    for tt in tokens.clone() {
        match tt {
            TokenTree::Ident(id) => {
                out.insert(id.to_string());
            }
            TokenTree::Group(g) => collect_token_idents(&g.stream(), out),
            TokenTree::Literal(lit) => {
                out.extend(format_captures(&lit.to_string()).map(str::to_owned));
            }
            TokenTree::Punct(_) => {}
        }
    }
}

/// Collects every identifier name referenced anywhere in a file — path
/// segments, macro token streams, and attribute tokens. Used to detect
/// cross-file uses of a const name within the same crate.
struct NameCollector<'a> {
    names: &'a mut HashSet<String>,
}

impl<'ast> Visit<'ast> for NameCollector<'_> {
    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if let Meta::List(list) = &a.meta {
            collect_token_idents(&list.tokens, self.names);
        }
        visit::visit_attribute(self, a);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        collect_token_idents(&m.tokens, self.names);
        visit::visit_macro(self, m);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        for seg in &p.segments {
            self.names.insert(seg.ident.to_string());
        }
        visit::visit_path(self, p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Vec<String> {
        run_with_external(src, &HashSet::new())
    }

    fn fix(src: &str) -> String {
        let file = syn::parse_file(src).expect("valid Rust source");
        let mut rw = SourceRewriter::new(src);
        stage_moves(
            src,
            &analyze_file("fixture.rs", &file, &HashSet::new()),
            &mut rw,
        );
        rw.finish().expect("non-overlapping edits")
    }

    #[test]
    fn a_fix_moves_the_const_and_its_docs_into_its_only_fn() {
        let src = "\
use std::fmt;

/// Bytes per frame.
pub(crate) const FRAME: usize = 4;

fn size(n: usize) -> usize {
    n * FRAME
}
";
        assert_eq!(
            fix(src),
            "\
use std::fmt;

fn size(n: usize) -> usize {
    /// Bytes per frame.
    const FRAME: usize = 4;

    n * FRAME
}
"
        );
    }

    #[test]
    fn a_fix_carries_attached_comments_to_the_method_indentation() {
        let src = "\
mod tests {
    // Chunk count the fixture covers.
    const CHUNKS: usize = 3; // three

    struct S;

    impl S {
        fn count(&self) -> usize {
            CHUNKS
        }
    }
}
";
        assert_eq!(
            fix(src),
            "\
mod tests {
    struct S;

    impl S {
        fn count(&self) -> usize {
            // Chunk count the fixture covers.
            const CHUNKS: usize = 3; // three

            CHUNKS
        }
    }
}
"
        );
    }

    #[test]
    fn a_fix_keeps_the_text_of_a_multi_line_literal() {
        let src = "\
const HEADER: &str = \"first
  second\";

fn header() -> &'static str {
    HEADER
}
";
        assert_eq!(
            fix(src),
            "\
fn header() -> &'static str {
    const HEADER: &str = \"first
  second\";

    HEADER
}
"
        );
    }

    #[test]
    fn a_fix_moves_the_consts_of_one_fn_as_one_block() {
        let src = "\
const WIDTH: usize = 4;
const HEIGHT: usize = 3;

fn area() -> usize {
    WIDTH * HEIGHT
}
";
        assert_eq!(
            fix(src),
            "\
fn area() -> usize {
    const WIDTH: usize = 4;
    const HEIGHT: usize = 3;

    WIDTH * HEIGHT
}
"
        );
    }

    fn run_with_external(src: &str, external: &HashSet<&str>) -> Vec<String> {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        analyze_file("fixture.rs", &file, external)
            .into_iter()
            .map(|finding| finding.key)
            .collect()
    }

    #[test]
    fn genuine_single_fn_is_flagged() {
        let src = "\
const ONLY_HERE: u64 = 7;
fn one() {
    let _ = ONLY_HERE + 1;
}
";
        assert_eq!(
            run(src),
            vec!["fixture.rs::ONLY_HERE"],
            "a const used in exactly one fn body (expression position) must fire"
        );
    }

    #[test]
    fn genuine_single_impl_is_flagged() {
        let src = "\
const ONLY_IMPL: u64 = 7;
struct S;
impl S {
    fn a(&self) -> u64 { ONLY_IMPL }
    fn b(&self) -> u64 { ONLY_IMPL + 1 }
}
";
        assert_eq!(
            run(src),
            vec!["fixture.rs::ONLY_IMPL"],
            "a const used only by methods of one impl must fire"
        );
    }

    #[test]
    fn used_in_two_fns_is_not_flagged() {
        let src = "\
const SHARED: u64 = 7;
fn a() { let _ = SHARED; }
fn b() { let _ = SHARED; }
";
        assert!(run(src).is_empty(), "two-fn use is genuinely spread");
    }

    #[test]
    fn test_module_ref_is_not_flagged() {
        // `subscription.rs::TICK_INTERVAL_*` shape: used by one prod fn AND
        // a `#[cfg(test)]` module (both in a macro attr and a test fn body).
        let src = "\
pub(crate) const TICK: u64 = 100;
pub(crate) const fn cfg(playing: bool) -> u64 {
    if playing { TICK } else { 0 }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() { assert!(TICK > 0); }
}
";
        assert!(
            run(src).is_empty(),
            "a const also referenced from a test module must not be flagged"
        );
    }

    #[test]
    fn macro_arg_ref_is_not_flagged() {
        // `abr.rs::ABR_MODE_AUTO_THRESHOLD` shape: one fn body reference is
        // inside a `debug_assert!` macro token stream.
        let src = "\
const THRESHOLD: usize = 100;
fn check(v: usize) {
    debug_assert!(v < THRESHOLD, \"too large\");
}
";
        assert!(
            run(src).is_empty(),
            "a const referenced only inside a macro token stream is unprovable — must not flag"
        );
    }

    #[test]
    fn format_capture_ref_is_not_flagged() {
        // `envelope.rs::MAX_ENVELOPE_DIRECTORY_ENTRIES` shape: the second fn
        // names the const only as an inline capture inside a format string.
        let src = "\
const LIMIT: usize = 100;
fn check(n: usize) -> bool { n >= LIMIT }
fn explain() -> String { format!(\"limit of {LIMIT} entries, {{LIMIT}} escaped\") }
";
        assert!(
            run(src).is_empty(),
            "a format-string capture is a use the fixer must not move away from"
        );
    }

    #[test]
    fn format_capture_in_another_file_is_not_flagged() {
        let src = "\
const LIMIT: usize = 100;
fn check(n: usize) -> bool { n >= LIMIT }
";
        let file: syn::File = syn::parse_str("fn explain() -> String { format!(\"{LIMIT:>4}\") }")
            .expect("valid Rust source");
        let mut names = HashSet::new();
        NameCollector { names: &mut names }.visit_file(&file);
        let external: HashSet<&str> = names.iter().map(String::as_str).collect();

        assert!(run_with_external(src, &external).is_empty());
    }

    #[test]
    fn attr_macro_arg_ref_is_not_flagged() {
        // `dl/tests.rs::SLOW_DEADLINE_SECS` shape: referenced in a
        // `#[kithara::test(timeout(NAME))]` attribute and a fn body.
        let src = "\
const DEADLINE: u64 = 5;
#[some::test(timeout(DEADLINE))]
fn t() {
    let _ = DEADLINE;
}
";
        assert!(
            run(src).is_empty(),
            "a const referenced in an attribute token stream must not be flagged"
        );
    }

    #[test]
    fn cross_module_ref_is_not_flagged() {
        // Referenced by a sibling module in the same file.
        let src = "\
pub(crate) const GAIN: f32 = 1.0;
fn clamp(x: f32) -> f32 { x.min(GAIN) }
mod other {
    use super::GAIN;
    pub fn also(x: f32) -> f32 { x.max(GAIN) }
}
";
        assert!(
            run(src).is_empty(),
            "a const referenced from another module must not be flagged"
        );
    }

    #[test]
    fn cross_file_ref_is_not_flagged() {
        // `shared_eq.rs::EQ_*` shape: single in-file fn use, but the const is
        // also referenced from another file of the same crate (passed via the
        // crate-wide `external` set).
        let src = "\
pub(crate) const GAIN: f32 = 1.0;
fn clamp(x: f32) -> f32 { x.min(GAIN) }
";
        // Without external refs it *would* be a genuine single-fn local.
        assert_eq!(run(src), vec!["fixture.rs::GAIN"]);
        // With a cross-file reference it must not be flagged.
        let external: HashSet<&str> = ["GAIN"].into_iter().collect();
        assert!(
            run_with_external(src, &external).is_empty(),
            "a const referenced from another file of the same crate must not be flagged"
        );
    }

    #[test]
    fn fixture_module_const_used_by_parent_is_not_flagged() {
        // `parsing.rs::tests::fixtures::*` shape: const lives in a child
        // module, referenced by a fn in the parent module — cross-module.
        let src = "\
mod tests {
    mod fixtures {
        pub(super) const DATA: &[u8] = b\"x\";
    }
    fn t() {
        let _ = fixtures::DATA;
    }
}
";
        assert!(
            run(src).is_empty(),
            "a fixture const used only by a parent-module fn must not be flagged"
        );
    }

    #[test]
    fn type_position_ref_is_not_flagged() {
        // `registry.rs::SLOT_COUNT` shape: array-length in a fn signature
        // and a struct field type, plus one expression-position loop use.
        let src = "\
const SLOT_COUNT: usize = 4;
struct Reg {
    slots: [u8; SLOT_COUNT],
}
fn enqueue(slots: &mut [u8; SLOT_COUNT]) {
    for i in 0..SLOT_COUNT {
        slots[i] = 0;
    }
}
";
        assert!(
            run(src).is_empty(),
            "a const used in a type / array-length position must not be flagged"
        );
    }

    #[test]
    fn array_length_in_body_is_not_flagged() {
        // `decrypt.rs::AES_BLOCK_SIZE` shape: all refs in one fn, but one is
        // an array-length / type position inside the body.
        let src = "\
const BLOCK: usize = 16;
fn process(input: &[u8]) {
    let mut iv = [0u8; BLOCK];
    iv[0] = input.len() as u8;
}
";
        assert!(
            run(src).is_empty(),
            "an array-length reference (even inside the single fn) must not be flagged"
        );
    }

    #[test]
    fn signature_only_ref_is_not_flagged() {
        // Reference appears only in a fn signature (const-generic default-ish
        // / array param) — never in any body.
        let src = "\
const N: usize = 4;
fn takes(_x: [u8; N]) {}
";
        assert!(
            run(src).is_empty(),
            "a reference only in a fn signature must not be flagged"
        );
    }

    #[test]
    fn doc_link_ref_is_not_flagged() {
        // `ffi/player.rs::SALT_LEN` shape: intra-doc link plus a test ref.
        let src = "\
const SALT_LEN: usize = 16;
/// Produces a salt of [`SALT_LEN`] bytes.
fn salt() -> usize { SALT_LEN }
";
        assert!(
            run(src).is_empty(),
            "an intra-doc-link reference must not be flagged"
        );
    }

    #[test]
    fn pub_const_is_not_considered() {
        let src = "\
pub const EXPORTED: u64 = 7;
fn one() { let _ = EXPORTED; }
";
        assert!(
            run(src).is_empty(),
            "fully public consts are part of the API surface, not locality candidates"
        );
    }
}
