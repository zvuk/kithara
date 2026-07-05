use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use anyhow::Result;
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::{
    GenericArgument, Item, ItemImpl, PathArguments, Type, Visibility,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::{parse_file, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "const_locality";

pub(crate) struct ConstLocality;

impl Check for ConstLocality {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        // Pass 1: parse every scoped file and record, per file, the crate it
        // belongs to and every identifier name it references. The crate key
        // lets us ask "is this const referenced from another file of the same
        // crate?" — a cross-file use that single-file scanning cannot see.
        let mut files: Vec<ParsedFile> = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            let crate_key = crate_key_for(ctx.workspace_root, &path);
            let mut names = HashSet::new();
            let mut collector = NameCollector { names: &mut names };
            collector.visit_file(&file);
            files.push(ParsedFile {
                rel,
                crate_key,
                file,
                names,
            });
        }

        // Pass 2: per file, flag module-level consts whose every reference is
        // an expression-position use inside a single same-module fn/impl.
        let mut violations = Vec::new();
        for (idx, pf) in files.iter().enumerate() {
            // Names referenced anywhere else in the same crate.
            let external: HashSet<&str> = files
                .iter()
                .enumerate()
                .filter(|(j, other)| *j != idx && other.crate_key == pf.crate_key)
                .flat_map(|(_, other)| other.names.iter().map(String::as_str))
                .collect();
            analyze_file(&pf.rel, &pf.file, &external, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct ParsedFile {
    rel: String,
    crate_key: String,
    file: syn::File,
    names: HashSet<String>,
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

fn analyze_file(rel: &str, file: &syn::File, external: &HashSet<&str>, out: &mut Vec<Violation>) {
    let mut consts: Vec<ConstSite> = Vec::new();
    collect_consts(&file.items, &mut Vec::new(), &mut consts);

    for site in consts {
        // Referenced from another file of the same crate → shared, not local.
        if external.contains(site.name.as_str()) {
            continue;
        }

        // Whole-file reference scan: a const is only "local to one fn" when
        // *every* reference to its name is an expression-position use inside
        // a single fn/method body *in the const's own module*. References in
        // another fn, a different (e.g. `#[cfg(test)]`) module, a
        // macro/attribute token stream, a doc comment, or a type /
        // const-generic / array-length position make the locality claim
        // unprovable, so we stay conservative and skip.
        let mut analyzer = RefAnalyzer::new(&site.name, &site.mod_path);
        analyzer.visit_file(file);
        if analyzer.disqualified {
            continue;
        }

        let mod_prefix = if site.mod_path.is_empty() {
            String::new()
        } else {
            format!("{}::", site.mod_path.join("::"))
        };

        match classify(&analyzer.owners) {
            Locality::SingleFn(label) => {
                let key = format!("{rel}::{mod_prefix}{}", site.name);
                let msg = format!(
                    "L1: const `{}` is referenced only from `{label}`; move it \
                     inside that function as a local `const`",
                    site.name
                );
                out.push(Violation::warn(ID, key, msg));
            }
            Locality::SingleImpl(target) => {
                let key = format!("{rel}::{mod_prefix}{}", site.name);
                let msg = format!(
                    "L2: const `{}` is referenced only by methods of `impl {target}`; \
                     move it into that impl block (accessed as `Self::{}`)",
                    site.name, site.name
                );
                out.push(Violation::warn(ID, key, msg));
            }
            Locality::Spread | Locality::Unused => {}
        }
    }
}

struct ConstSite {
    name: String,
    mod_path: Vec<String>,
}

fn collect_consts(items: &[Item], mod_path: &mut Vec<String>, out: &mut Vec<ConstSite>) {
    for item in items {
        match item {
            Item::Const(c) if is_intra_crate(&c.vis) => out.push(ConstSite {
                name: c.ident.to_string(),
                mod_path: mod_path.clone(),
            }),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    mod_path.push(m.ident.to_string());
                    collect_consts(inner, mod_path, out);
                    mod_path.pop();
                }
            }
            _ => {}
        }
    }
}

fn is_intra_crate(vis: &Visibility) -> bool {
    match vis {
        Visibility::Inherited => true,
        Visibility::Restricted(r) => r.path.is_ident("crate") || r.path.is_ident("super"),
        Visibility::Public(_) => false,
    }
}

#[derive(Debug, Clone)]
enum Owner {
    TopFn(String),
    ImplMethod {
        impl_id: usize,
        target: String,
        method: String,
    },
}

#[derive(Debug)]
enum Locality {
    Unused,
    SingleFn(String),
    SingleImpl(String),
    Spread,
}

fn classify(owners: &[Owner]) -> Locality {
    if owners.is_empty() {
        return Locality::Unused;
    }
    if owners.len() == 1 {
        return Locality::SingleFn(format_owner(&owners[0]));
    }
    let mut impl_ids: BTreeMap<usize, &str> = BTreeMap::new();
    let mut top_fn_seen = false;
    for o in owners {
        match o {
            Owner::TopFn(_) => top_fn_seen = true,
            Owner::ImplMethod {
                impl_id, target, ..
            } => {
                impl_ids.insert(*impl_id, target.as_str());
            }
        }
    }
    if !top_fn_seen && impl_ids.len() == 1 {
        let target = impl_ids.values().next().unwrap_or(&"_").to_string();
        return Locality::SingleImpl(target);
    }
    Locality::Spread
}

fn format_owner(o: &Owner) -> String {
    match o {
        Owner::TopFn(name) => format!("fn {name}"),
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
    name: &'a str,
    /// Module path where the const is declared. An expression-position
    /// reference from a *different* module is a cross-module use, not a
    /// fn-local one.
    const_mod_path: &'a [String],
    owners: Vec<Owner>,
    disqualified: bool,
    impl_counter: usize,
    /// `Some` while visiting a fn/method block body; carries that body's owner.
    current_owner: Option<Owner>,
    /// Module path currently being visited.
    current_mod_path: Vec<String>,
    /// `> 0` while inside a type / const-generic / array-length context.
    type_depth: usize,
}

impl<'a> RefAnalyzer<'a> {
    fn new(name: &'a str, const_mod_path: &'a [String]) -> Self {
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
        // A reference from a different module than the const lives in is a
        // cross-module use; the const cannot be made fn-body-local there.
        if self.current_mod_path != self.const_mod_path {
            self.disqualified = true;
            return;
        }
        match &self.current_owner {
            Some(owner) => {
                if !self.owners.iter().any(|o| same_owner(o, owner)) {
                    self.owners.push(owner.clone());
                }
            }
            // Reference reached in expression position but outside any
            // fn/method body (e.g. a top-level const/static initializer).
            None => self.disqualified = true,
        }
    }

    fn tokens_mention_name(&self, tokens: &TokenStream) -> bool {
        token_stream_mentions(tokens, self.name)
    }
}

fn same_owner(a: &Owner, b: &Owner) -> bool {
    match (a, b) {
        (Owner::TopFn(x), Owner::TopFn(y)) => x == y,
        (
            Owner::ImplMethod {
                impl_id: ia,
                method: ma,
                ..
            },
            Owner::ImplMethod {
                impl_id: ib,
                method: mb,
                ..
            },
        ) => ia == ib && ma == mb,
        _ => false,
    }
}

impl<'ast> Visit<'ast> for RefAnalyzer<'_> {
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

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if self.disqualified {
            return;
        }
        for attr in &f.attrs {
            self.visit_attribute(attr);
        }
        // Signature (generics, args, return type) is *not* the fn body.
        // A reference there cannot be turned into a body-local const.
        self.visit_signature(&f.sig);
        let saved = self.current_owner.take();
        self.current_owner = Some(Owner::TopFn(f.sig.ident.to_string()));
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
        // The impl header (self type + generics + trait path) is not a body.
        self.type_depth += 1;
        self.visit_type(&im.self_ty);
        self.type_depth -= 1;
        for it in &im.items {
            match it {
                syn::ImplItem::Fn(method) => {
                    for attr in &method.attrs {
                        self.visit_attribute(attr);
                    }
                    self.visit_signature(&method.sig);
                    let saved = self.current_owner.take();
                    self.current_owner = Some(Owner::ImplMethod {
                        impl_id,
                        target: target.clone(),
                        method: method.sig.ident.to_string(),
                    });
                    self.visit_block(&method.block);
                    self.current_owner = saved;
                }
                other => visit::visit_impl_item(self, other),
            }
        }
    }

    fn visit_expr_repeat(&mut self, e: &'ast syn::ExprRepeat) {
        // `[value; LEN]` — the length is a const expression position, so a
        // reference there is not a movable body-local const.
        self.visit_expr(&e.expr);
        self.type_depth += 1;
        self.visit_expr(&e.len);
        self.type_depth -= 1;
    }

    fn visit_type(&mut self, t: &'ast Type) {
        self.type_depth += 1;
        visit::visit_type(self, t);
        self.type_depth -= 1;
    }

    fn visit_generic_argument(&mut self, arg: &'ast GenericArgument) {
        // `GenericArgument::Const` is a const-generic value position.
        if let GenericArgument::Const(_) = arg {
            self.type_depth += 1;
            visit::visit_generic_argument(self, arg);
            self.type_depth -= 1;
        } else {
            visit::visit_generic_argument(self, arg);
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        // syn does not parse macro token streams; a reference inside one
        // cannot be proven to be expression-position-local, so we bail.
        if self.tokens_mention_name(&m.tokens) {
            self.disqualified = true;
        }
        visit::visit_macro(self, m);
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        // Covers attribute-macro args (`#[case(..)]`,
        // `#[kithara::test(timeout(..))]`) and doc comments
        // (`#[doc = "...[`NAME`]..."]`, including intra-doc links).
        if attr_mentions_name(a, self.name) {
            self.disqualified = true;
        }
        visit::visit_attribute(self, a);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if self.disqualified {
            return;
        }
        let is_match = p.segments.last().is_some_and(|s| s.ident == self.name);
        if is_match {
            if self.type_depth > 0 {
                // Type / const-generic / array-length position.
                self.disqualified = true;
            } else {
                self.record_owner();
            }
            // Still descend so nested generic args (e.g. turbofish) are seen.
        }
        // Path segment arguments can contain types / const-generics.
        for seg in &p.segments {
            if let PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    self.visit_generic_argument(arg);
                }
            }
        }
    }
}

fn token_stream_mentions(tokens: &TokenStream, name: &str) -> bool {
    tokens.clone().into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => id == name,
        TokenTree::Group(g) => token_stream_mentions(&g.stream(), name),
        _ => false,
    })
}

fn attr_mentions_name(a: &syn::Attribute, name: &str) -> bool {
    match &a.meta {
        syn::Meta::Path(_) => false,
        syn::Meta::List(list) => token_stream_mentions(&list.tokens, name),
        // Doc comments and other name-value attrs: scan the value (catches
        // intra-doc links such as `[`NAME`]` in `#[doc = "..."]`).
        syn::Meta::NameValue(nv) => match &nv.value {
            syn::Expr::Lit(lit) => match &lit.lit {
                syn::Lit::Str(s) => s.value().contains(name),
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
            _ => {}
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
    fn visit_path(&mut self, p: &'ast syn::Path) {
        for seg in &p.segments {
            self.names.insert(seg.ident.to_string());
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        collect_token_idents(&m.tokens, self.names);
        visit::visit_macro(self, m);
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if let syn::Meta::List(list) = &a.meta {
            collect_token_idents(&list.tokens, self.names);
        }
        visit::visit_attribute(self, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Vec<String> {
        run_with_external(src, &HashSet::new())
    }

    fn run_with_external(src: &str, external: &HashSet<&str>) -> Vec<String> {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file, external, &mut out);
        out.into_iter().map(|v| v.key).collect()
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
