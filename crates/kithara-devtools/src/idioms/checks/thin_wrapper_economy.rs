use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    ops::Range,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Block, Expr, ExprPath, FieldValue, FnArg, ImplItemFn, Item, ItemFn, ItemUse, Lit, Macro,
    Member, Meta, Pat, PathArguments, Safety, Stmt, TraitItemFn, UseTree, Visibility,
    punctuated::Punctuated,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    exclude::{attrs_have_cfg_test, item_attrs},
    fix::{SourceRewriter, deletion_range},
    scope::Scope,
    violation::Violation,
    walker::relative_to,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "thin_wrapper_economy";
}

type SymbolKey = (String, String);

pub(crate) struct ThinWrapperEconomy;

impl Check for ThinWrapperEconomy {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let files = load_sources(ctx)?;
        let config = &ctx.config.thresholds.thin_wrapper_economy;
        let threshold = config.min_net_saved_lines;
        let mut violations = analyze(ctx.workspace_root, &files)
            .into_iter()
            .filter(|finding| ctx.scope.key_in_scope(&finding.candidate.rel))
            .filter_map(|finding| finding.violation(threshold))
            .collect::<Vec<_>>();
        violations.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(violations)
    }
}

struct SourceFile {
    syntax: syn::File,
    rel: String,
    scope: String,
    source: String,
    production: bool,
}

#[derive(Clone)]
struct Candidate {
    comment_refusal: Option<&'static str>,
    params: Option<Vec<String>>,
    substitution_refusal: Option<&'static str>,
    body: Range<usize>,
    definition: Range<usize>,
    name: String,
    rel: String,
    scope: String,
    module: Vec<String>,
    param_uses: Vec<ParamUse>,
    trailing_semicolon: bool,
    file: usize,
    line: usize,
}

#[derive(Clone)]
struct ParamUse {
    shorthand: Option<String>,
    range: Range<usize>,
    param: usize,
}

#[derive(Clone)]
struct CallSite {
    range: Range<usize>,
    args: Vec<Range<usize>>,
    module: Vec<String>,
    attributed: bool,
    discarded: bool,
    file: usize,
}

#[derive(Clone, Copy)]
struct Metric {
    net_saved: i64,
    call_sites: usize,
}

struct Finding {
    candidate: Candidate,
    metric: Option<Metric>,
    unknown: Option<String>,
    calls: Vec<CallSite>,
}

impl Finding {
    fn violation(self, threshold: usize) -> Option<Violation> {
        if self
            .metric
            .is_some_and(|metric| !below_threshold(metric.net_saved, threshold))
        {
            return None;
        }
        let key = format!(
            "{}:{}:{}",
            self.candidate.rel, self.candidate.line, self.candidate.name
        );
        let economy = self.metric.map_or_else(
            || {
                format!(
                    "call_sites=unknown(known_direct={}), net_saved=unknown",
                    self.calls.len()
                )
            },
            |metric| {
                format!(
                    "call_sites={}, net_saved={}",
                    metric.call_sites, metric.net_saved
                )
            },
        );
        let uncertainty = self.unknown.map_or_else(String::new, |reason| {
            format!("; exact metric unavailable: {reason}")
        });
        Some(Violation::warn(
            consts::ID,
            key,
            format!(
                "thin wrapper `{}`: {economy}, threshold={threshold}{uncertainty}",
                self.candidate.name
            ),
        ))
    }
}

fn load_sources(ctx: &Context<'_>) -> Result<Vec<SourceFile>> {
    let mut files = Vec::new();
    for path in ctx.scan.rs_files(&Scope::default())?.iter() {
        let rel = relative_to(ctx.workspace_root, path)
            .to_string_lossy()
            .replace('\\', "/");
        let source =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let syntax =
            syn::parse_file(&source).with_context(|| format!("parse {}", path.display()))?;
        files.push(SourceFile {
            scope: source_scope(&rel),
            production: is_production_source(&rel),
            rel,
            source,
            syntax,
        });
    }
    Ok(files)
}

fn source_scope(rel: &str) -> String {
    rel.split_once("/src/")
        .map_or_else(|| ".".to_owned(), |(root, _)| root.to_owned())
}

fn symbol_key(scope: &str, name: &str) -> SymbolKey {
    (scope.to_owned(), name.to_owned())
}

fn is_production_source(rel: &str) -> bool {
    if !(rel.starts_with("src/") || rel.contains("/src/")) {
        return false;
    }
    let file = rel.rsplit('/').next().unwrap_or(rel);
    file != "tests.rs"
        && !file.starts_with("test_")
        && !file.ends_with("_test.rs")
        && !file.ends_with("_tests.rs")
        && !rel.contains("/tests/")
}

fn analyze(workspace_root: &Path, files: &[SourceFile]) -> Vec<Finding> {
    let mut definitions = BTreeMap::<SymbolKey, usize>::new();
    let mut candidates = Vec::new();
    for (file, source) in files.iter().enumerate() {
        if !source.production {
            continue;
        }
        collect_definitions(
            &source.syntax.items,
            file,
            source,
            &mut Vec::new(),
            &mut definitions,
            &mut candidates,
        );
    }
    let candidate_names = candidates
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect::<BTreeSet<_>>();
    let mut references = References::new(&candidate_names);
    for (file, source) in files.iter().enumerate() {
        if !source.production {
            continue;
        }
        references.file = file;
        references.scope.clone_from(&source.scope);
        references.visit_file(&source.syntax);
    }
    let mut baseline_loc = BTreeMap::new();
    let mut findings = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let key = symbol_key(&candidate.scope, &candidate.name);
        let calls = references.calls.get(&key).cloned().unwrap_or_default();
        let unknown =
            exact_refusal(files, &candidate, &calls, &definitions, &references).map(str::to_owned);
        let (metric, unknown) = unknown.map_or_else(
            || match measure_candidate(workspace_root, files, &candidate, &calls, &mut baseline_loc)
            {
                Ok(metric) => (Some(metric), None),
                Err(error) => (None, Some(format!("rustfmt measurement failed: {error:#}"))),
            },
            |reason| (None, Some(reason)),
        );
        findings.push(Finding {
            candidate,
            metric,
            unknown,
            calls,
        });
    }
    findings
}

fn collect_definitions(
    items: &[Item],
    file: usize,
    source: &SourceFile,
    module: &mut Vec<String>,
    definitions: &mut BTreeMap<SymbolKey, usize>,
    candidates: &mut Vec<Candidate>,
) {
    for item in items {
        if attrs_have_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            Item::Fn(function) => {
                *definitions
                    .entry(symbol_key(&source.scope, &function.sig.ident.to_string()))
                    .or_default() += 1;
                if let Some(candidate) = candidate(
                    file,
                    &source.rel,
                    &source.scope,
                    &source.source,
                    module,
                    function,
                ) {
                    candidates.push(candidate);
                }
            }
            Item::Mod(item_mod) => {
                if let Some((_, inner)) = &item_mod.content {
                    module.push(item_mod.ident.to_string());
                    collect_definitions(inner, file, source, module, definitions, candidates);
                    module.pop();
                }
            }
            _ => {}
        }
    }
}

fn candidate(
    file: usize,
    rel: &str,
    scope: &str,
    source: &str,
    module: &[String],
    function: &ItemFn,
) -> Option<Candidate> {
    let name = function.sig.ident.to_string();
    if !function.attrs.is_empty()
        || matches!(function.vis, Visibility::Public(_))
        || function.modifiers.defaultness.is_some()
        || function.sig.constness.is_some()
        || function.sig.asyncness.is_some()
        || !matches!(function.sig.safety, Safety::Default)
        || function.sig.abi.is_some()
        || function.sig.variadic.is_some()
        || !function.sig.generics.params.is_empty()
        || function.sig.generics.where_clause.is_some()
    {
        return None;
    }
    let [Stmt::Expr(body, semicolon)] = function.block.stmts.as_slice() else {
        return None;
    };
    if !is_call_tree_root(body) {
        return None;
    }
    let params = simple_params(function);
    let param_uses = params
        .as_ref()
        .map_or_else(Vec::new, |params| collect_param_uses(body, params));
    let substitution_refusal = substitution_refusal(body, params.as_deref(), &param_uses);
    let definition = function.span().byte_range();
    let body_range = body.span().byte_range();
    let comment_refusal = if source
        .get(definition.clone())
        .is_some_and(|text| text.contains("//") || text.contains("/*"))
    {
        Some("function contains comments")
    } else if has_attached_leading_comment(source, definition.start) {
        Some("function has an attached leading comment")
    } else if has_attached_trailing_comment(source, definition.end) {
        Some("function has an attached trailing comment")
    } else {
        None
    };
    Some(Candidate {
        file,
        name,
        definition,
        params,
        param_uses,
        substitution_refusal,
        comment_refusal,
        rel: rel.to_owned(),
        scope: scope.to_owned(),
        module: module.to_vec(),
        line: function.sig.fn_token.span.start().line,
        body: body_range,
        trailing_semicolon: semicolon.is_some(),
    })
}

fn has_attached_leading_comment(source: &str, definition_start: usize) -> bool {
    let before = &source[..definition_start.min(source.len())];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let same_line = before[line_start..].trim();
    if same_line.contains("//") || same_line.contains("/*") || same_line.contains("*/") {
        return true;
    }

    let Some(previous_line) = before[..line_start].lines().next_back() else {
        return false;
    };
    let previous_line = previous_line.trim();
    !previous_line.is_empty()
        && (previous_line.starts_with("//")
            || previous_line.starts_with("/*")
            || previous_line.starts_with('*')
            || previous_line.ends_with("*/"))
}

fn has_attached_trailing_comment(source: &str, definition_end: usize) -> bool {
    source[definition_end.min(source.len())..]
        .lines()
        .next()
        .is_some_and(|line| line.contains("//") || line.contains("/*"))
}

fn is_call_tree_root(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(_) | Expr::MethodCall(_)) && is_call_tree(expr)
}

fn is_call_tree(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => is_call_tree(&call.func) && call.args.iter().all(is_call_tree),
        Expr::MethodCall(call) => {
            is_call_tree(&call.receiver) && call.args.iter().all(is_call_tree)
        }
        Expr::Path(_) | Expr::Lit(_) => true,
        Expr::Field(field) => is_call_tree(&field.base),
        Expr::Reference(reference) => is_call_tree(&reference.expr),
        Expr::Paren(paren) => is_call_tree(&paren.expr),
        Expr::Group(group) => is_call_tree(&group.expr),
        Expr::Cast(cast) => is_call_tree(&cast.expr),
        Expr::Struct(strukt) => {
            strukt.fields.iter().all(|field| is_call_tree(&field.expr))
                && strukt.rest.as_deref().is_none_or(is_call_tree)
        }
        _ => false,
    }
}

fn simple_params(function: &ItemFn) -> Option<Vec<String>> {
    let mut params = Vec::with_capacity(function.sig.inputs.len());
    for input in &function.sig.inputs {
        let FnArg::Typed(typed) = input else {
            return None;
        };
        if !typed.attrs.is_empty() {
            return None;
        }
        let Pat::Ident(ident) = typed.pat.as_ref() else {
            return None;
        };
        if ident.by_ref.is_some() || ident.mutability.is_some() || ident.subpat.is_some() {
            return None;
        }
        params.push(ident.ident.to_string());
    }
    let unique = params.iter().collect::<BTreeSet<_>>();
    (unique.len() == params.len()).then_some(params)
}

fn collect_param_uses(expr: &Expr, params: &[String]) -> Vec<ParamUse> {
    let indices = params
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut collector = ParamUses {
        indices: &indices,
        uses: Vec::new(),
    };
    collector.visit_expr(expr);
    collector.uses
}

struct ParamUses<'a> {
    indices: &'a BTreeMap<&'a str, usize>,
    uses: Vec<ParamUse>,
}

impl<'ast> Visit<'ast> for ParamUses<'_> {
    fn visit_expr_path(&mut self, path: &'ast ExprPath) {
        if path.qself.is_none() && path.path.segments.len() == 1 {
            let name = path.path.segments[0].ident.to_string();
            if let Some(index) = self.indices.get(name.as_str()) {
                self.uses.push(ParamUse {
                    param: *index,
                    range: path.span().byte_range(),
                    shorthand: None,
                });
            }
        }
    }

    fn visit_field_value(&mut self, field: &'ast FieldValue) {
        if field.colon_token.is_none()
            && let Expr::Path(path) = &field.expr
            && path.qself.is_none()
            && path.path.segments.len() == 1
        {
            let name = path.path.segments[0].ident.to_string();
            if let Some(index) = self.indices.get(name.as_str()) {
                let Member::Named(member) = &field.member else {
                    return;
                };
                self.uses.push(ParamUse {
                    param: *index,
                    range: path.span().byte_range(),
                    shorthand: Some(member.to_string()),
                });
                return;
            }
        }
        visit::visit_field_value(self, field);
    }
}

#[derive(Clone, Copy)]
enum EvalEvent {
    Param(usize),
    Effect,
}

fn substitution_refusal(
    expr: &Expr,
    params: Option<&[String]>,
    uses: &[ParamUse],
) -> Option<&'static str> {
    let Some(params) = params else {
        return Some("parameters are not simple identifiers");
    };
    let mut counts = vec![0_usize; params.len()];
    for usage in uses {
        counts[usage.param] += 1;
    }
    if counts.iter().any(|count| *count != 1) {
        return Some("parameter is repeated or unused");
    }
    let mut events = Vec::new();
    collect_eval_events(expr, params, &mut events);
    let order = events
        .iter()
        .filter_map(|event| match event {
            EvalEvent::Param(index) => Some(*index),
            EvalEvent::Effect => None,
        })
        .collect::<Vec<_>>();
    if order != (0..params.len()).collect::<Vec<_>>() {
        return Some("parameter evaluation order changes");
    }
    if !params.is_empty() {
        let mut seen = 0_usize;
        for event in events {
            match event {
                EvalEvent::Param(_) => seen += 1,
                EvalEvent::Effect if seen < params.len() => {
                    return Some("argument evaluation order changes");
                }
                EvalEvent::Effect => {}
            }
        }
    }
    None
}

fn collect_eval_events(expr: &Expr, params: &[String], out: &mut Vec<EvalEvent>) {
    match expr {
        Expr::Call(call) => {
            collect_eval_events(&call.func, params, out);
            for arg in &call.args {
                collect_eval_events(arg, params, out);
            }
            out.push(EvalEvent::Effect);
        }
        Expr::MethodCall(call) => {
            collect_eval_events(&call.receiver, params, out);
            for arg in &call.args {
                collect_eval_events(arg, params, out);
            }
            out.push(EvalEvent::Effect);
        }
        Expr::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let name = path.path.segments[0].ident.to_string();
            if let Some(index) = params.iter().position(|param| param == &name) {
                out.push(EvalEvent::Param(index));
            }
        }
        Expr::Field(field) => {
            collect_eval_events(&field.base, params, out);
            out.push(EvalEvent::Effect);
        }
        Expr::Reference(reference) => collect_eval_events(&reference.expr, params, out),
        Expr::Paren(paren) => collect_eval_events(&paren.expr, params, out),
        Expr::Group(group) => collect_eval_events(&group.expr, params, out),
        Expr::Cast(cast) => collect_eval_events(&cast.expr, params, out),
        Expr::Struct(strukt) => {
            for field in &strukt.fields {
                collect_eval_events(&field.expr, params, out);
            }
            if let Some(rest) = &strukt.rest {
                collect_eval_events(rest, params, out);
            }
        }
        _ => {}
    }
}

struct References<'a> {
    candidate_names: &'a BTreeSet<String>,
    calls: BTreeMap<SymbolKey, Vec<CallSite>>,
    imports: BTreeMap<usize, BTreeSet<String>>,
    unknown: BTreeMap<SymbolKey, BTreeSet<&'static str>>,
    bindings: BTreeSet<String>,
    glob_files: BTreeSet<usize>,
    discarded_root: Option<Range<usize>>,
    scope: String,
    module: Vec<String>,
    file: usize,
}

impl<'a> References<'a> {
    fn new(candidate_names: &'a BTreeSet<String>) -> Self {
        Self {
            candidate_names,
            calls: BTreeMap::new(),
            unknown: BTreeMap::new(),
            imports: BTreeMap::new(),
            glob_files: BTreeSet::new(),
            file: 0,
            scope: String::new(),
            module: Vec::new(),
            bindings: BTreeSet::new(),
            discarded_root: None,
        }
    }

    fn mark_unknown(&mut self, name: &str, reason: &'static str) {
        self.unknown
            .entry(symbol_key(&self.scope, name))
            .or_default()
            .insert(reason);
    }

    fn visit_function(&mut self, inputs: &Punctuated<FnArg, syn::Token![,]>, block: &Block) {
        let previous = std::mem::take(&mut self.bindings);
        let mut bindings = Bindings::default();
        for input in inputs {
            if let FnArg::Typed(typed) = input {
                bindings.visit_pat(&typed.pat);
            }
        }
        bindings.visit_block(block);
        self.bindings = bindings.names;
        visit::visit_block(self, block);
        self.bindings = previous;
    }
}

impl<'ast> Visit<'ast> for References<'_> {
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        let names = self.candidate_names.iter().cloned().collect::<Vec<_>>();
        for name in names {
            if attribute_contains_reference(attribute, &name) {
                self.mark_unknown(&name, "attribute-token reference");
            }
        }
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::Call(call) = expr
            && let Expr::Path(path) = call.func.as_ref()
            && let Some(name) = path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
            && self.candidate_names.contains(&name)
        {
            if direct_free_path(path)
                && !(path.path.segments.len() == 1 && self.bindings.contains(&name))
            {
                let range = call.span().byte_range();
                let discarded = self.discarded_root.as_ref() == Some(&range);
                self.calls
                    .entry(symbol_key(&self.scope, &name))
                    .or_default()
                    .push(CallSite {
                        range,
                        discarded,
                        file: self.file,
                        module: self.module.clone(),
                        args: call
                            .args
                            .iter()
                            .map(|arg| arg.span().byte_range())
                            .collect(),
                        attributed: !call.attrs.is_empty(),
                    });
            } else {
                self.mark_unknown(&name, "unresolved direct-call target");
            }
            for arg in &call.args {
                self.visit_expr(arg);
            }
            return;
        }
        visit::visit_expr(self, expr);
    }

    fn visit_expr_path(&mut self, path: &'ast ExprPath) {
        let Some(name) = path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        if self.candidate_names.contains(&name)
            && !(path.path.segments.len() == 1 && self.bindings.contains(&name))
        {
            self.mark_unknown(&name, "non-call function reference");
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        if !attrs_have_cfg_test(&function.attrs) {
            for attribute in &function.attrs {
                self.visit_attribute(attribute);
            }
            self.visit_function(&function.sig.inputs, &function.block);
        }
    }

    fn visit_item(&mut self, item: &'ast Item) {
        if !attrs_have_cfg_test(item_attrs(item)) {
            for attribute in item_attrs(item) {
                self.visit_attribute(attribute);
            }
            visit::visit_item(self, item);
        }
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if !attrs_have_cfg_test(&function.attrs) {
            self.visit_function(&function.sig.inputs, &function.block);
        }
    }

    fn visit_item_mod(&mut self, item_mod: &'ast syn::ItemMod) {
        if attrs_have_cfg_test(&item_mod.attrs) {
            return;
        }
        if let Some((_, items)) = &item_mod.content {
            self.module.push(item_mod.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.module.pop();
        }
    }

    fn visit_item_use(&mut self, item_use: &'ast ItemUse) {
        let mut names = Vec::new();
        let mut glob = false;
        collect_imports(&item_use.tree, &mut names, &mut glob);
        for name in names {
            if self.candidate_names.contains(&name) {
                self.mark_unknown(&name, "imported or aliased function reference");
                self.imports.entry(self.file).or_default().insert(name);
            }
        }
        if glob {
            self.glob_files.insert(self.file);
        }
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        for name in self.candidate_names {
            if macro_tokens_contain_reference(mac.tokens.clone(), name) {
                self.mark_unknown(name, "macro-token reference");
            }
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        if let Stmt::Expr(expr, Some(_)) = stmt {
            let previous = self.discarded_root.replace(expr.span().byte_range());
            self.visit_expr(expr);
            self.discarded_root = previous;
        } else {
            visit::visit_stmt(self, stmt);
        }
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        if !attrs_have_cfg_test(&function.attrs)
            && let Some(block) = &function.default
        {
            for attribute in &function.attrs {
                self.visit_attribute(attribute);
            }
            self.visit_function(&function.sig.inputs, block);
        }
    }
}

#[derive(Default)]
struct Bindings {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Bindings {
    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        self.names.insert(item.ident.to_string());
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.names.insert(function.sig.ident.to_string());
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        self.names.insert(item.ident.to_string());
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.names.insert(item.ident.to_string());
    }

    fn visit_pat_ident(&mut self, ident: &'ast syn::PatIdent) {
        self.names.insert(ident.ident.to_string());
        visit::visit_pat_ident(self, ident);
    }
}

fn direct_free_path(path: &ExprPath) -> bool {
    if path.qself.is_some()
        || path
            .path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, PathArguments::None))
    {
        return false;
    }
    path.path.segments.len() == 1
}

fn collect_imports(tree: &UseTree, out: &mut Vec<String>, glob: &mut bool) {
    match tree {
        UseTree::Rename(rename) => {
            out.push(rename.ident.to_string());
            out.push(rename.rename.to_string());
        }
        UseTree::Name(name) => out.push(name.ident.to_string()),
        UseTree::Path(path) => collect_imports(&path.tree, out, glob),
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_imports(tree, out, glob);
            }
        }
        UseTree::Glob(_) => *glob = true,
    }
}

fn attribute_contains_reference(attribute: &syn::Attribute, name: &str) -> bool {
    match &attribute.meta {
        Meta::Path(path) => path.segments.iter().any(|segment| segment.ident == name),
        Meta::List(list) => tokens_contain_reference(list.tokens.clone(), name),
        Meta::NameValue(name_value) if !name_value.path.is_ident("doc") => {
            match &name_value.value {
                Expr::Path(path) => path
                    .path
                    .segments
                    .iter()
                    .any(|segment| segment.ident == name),
                Expr::Lit(literal) => literal_contains_reference(&literal.lit, name),
                _ => false,
            }
        }
        Meta::NameValue(_) => false,
    }
}

fn tokens_contain_reference(tokens: TokenStream, name: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => ident == name,
        TokenTree::Group(group) => tokens_contain_reference(group.stream(), name),
        TokenTree::Literal(literal) => text_contains_identifier(&literal.to_string(), name),
        TokenTree::Punct(_) => false,
    })
}

fn macro_tokens_contain_reference(tokens: TokenStream, name: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => ident == name,
        TokenTree::Group(group) => macro_tokens_contain_reference(group.stream(), name),
        TokenTree::Literal(_) | TokenTree::Punct(_) => false,
    })
}

fn literal_contains_reference(literal: &Lit, name: &str) -> bool {
    match literal {
        Lit::Str(value) => text_contains_identifier(&value.value(), name),
        _ => false,
    }
}

fn text_contains_identifier(text: &str, name: &str) -> bool {
    text.split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .any(|part| part == name)
}

fn exact_refusal<'a>(
    files: &[SourceFile],
    candidate: &'a Candidate,
    calls: &[CallSite],
    definitions: &BTreeMap<SymbolKey, usize>,
    references: &'a References<'_>,
) -> Option<&'a str> {
    if let Some(reason) = candidate.comment_refusal {
        return Some(reason);
    }
    if calls.iter().any(|call| call.attributed) {
        return Some("call site has attributes");
    }
    if calls.iter().any(|call| {
        files[call.file]
            .source
            .get(call.range.clone())
            .is_some_and(|source| source.contains("//") || source.contains("/*"))
    }) {
        return Some("call site contains comments");
    }
    let key = symbol_key(&candidate.scope, &candidate.name);
    if definitions.get(&key).copied().unwrap_or_default() != 1 {
        return Some("ambiguous free-function name");
    }
    if let Some(reasons) = references.unknown.get(&key)
        && let Some(reason) = reasons.iter().next()
    {
        return Some(reason);
    }
    if calls.iter().any(|call| {
        references
            .imports
            .get(&call.file)
            .is_some_and(|names| names.contains(&candidate.name))
    }) {
        return Some("imported function name can shadow the candidate");
    }
    if calls
        .iter()
        .any(|call| references.glob_files.contains(&call.file))
    {
        return Some("glob import can shadow the candidate");
    }
    if calls.iter().any(|call| {
        call.file == candidate.file
            && call.range.start >= candidate.definition.start
            && call.range.end <= candidate.definition.end
    }) {
        return Some("recursive wrapper reference");
    }
    if calls
        .iter()
        .any(|call| call.file != candidate.file || call.module != candidate.module)
    {
        return Some("call target requires cross-file or cross-module resolution");
    }
    if candidate.trailing_semicolon && calls.iter().any(|call| !call.discarded) {
        return Some("unit wrapper call is not a discarded statement");
    }
    candidate.substitution_refusal
}

#[derive(Clone)]
struct Edit {
    range: Range<usize>,
    text: String,
    file: usize,
}

fn candidate_edits(
    files: &[SourceFile],
    candidate: &Candidate,
    calls: &[CallSite],
) -> Result<Vec<Edit>> {
    let mut edits = Vec::with_capacity(calls.len() + 1);
    for call in calls {
        edits.push(Edit {
            file: call.file,
            range: call.range.clone(),
            text: expand_call(files, candidate, call)?,
        });
    }
    edits.push(Edit {
        file: candidate.file,
        range: deletion_range(&files[candidate.file].source, candidate.definition.clone()),
        text: String::new(),
    });
    Ok(edits)
}

fn expand_call(files: &[SourceFile], candidate: &Candidate, call: &CallSite) -> Result<String> {
    if candidate.trailing_semicolon && !call.discarded {
        bail!(
            "unit wrapper `{}` is not used as a discarded statement",
            candidate.name
        );
    }
    let params = candidate
        .params
        .as_ref()
        .context("inline candidate has non-identifier parameters")?;
    if params.len() != call.args.len() {
        bail!(
            "inline candidate `{}` expects {} arguments but call has {}",
            candidate.name,
            params.len(),
            call.args.len()
        );
    }
    let definition_source = &files[candidate.file].source;
    let body = definition_source
        .get(candidate.body.clone())
        .with_context(|| format!("slice body of `{}`", candidate.name))?;
    let mut rewriter = SourceRewriter::new(body);
    for usage in &candidate.param_uses {
        let argument = files[call.file]
            .source
            .get(call.args[usage.param].clone())
            .with_context(|| format!("slice argument for `{}`", candidate.name))?;
        let range = usage.range.start.saturating_sub(candidate.body.start)
            ..usage.range.end.saturating_sub(candidate.body.start);
        let argument = format!("({argument})");
        let replacement = usage
            .shorthand
            .as_ref()
            .map_or_else(|| argument.clone(), |field| format!("{field}: {argument}"));
        rewriter.replace(range, replacement);
    }
    rewriter
        .finish()
        .with_context(|| format!("substitute arguments into `{}`", candidate.name))
}

fn measure_candidate(
    workspace_root: &Path,
    files: &[SourceFile],
    candidate: &Candidate,
    calls: &[CallSite],
    baseline_loc: &mut BTreeMap<usize, usize>,
) -> Result<Metric> {
    let edits = candidate_edits(files, candidate, calls)?;
    let mut grouped = BTreeMap::<usize, Vec<Edit>>::new();
    for edit in edits {
        grouped.entry(edit.file).or_default().push(edit);
    }
    let mut net_saved = 0_i64;
    for (file, edits) in grouped {
        let baseline = if let Some(loc) = baseline_loc.get(&file) {
            *loc
        } else {
            let formatted = format_source(workspace_root, &files[file].source)?;
            let loc = nonblank_loc(&formatted);
            baseline_loc.insert(file, loc);
            loc
        };
        let rewritten = apply_edits(&files[file].source, &edits)?;
        let inlined = nonblank_loc(&format_source(workspace_root, &rewritten)?);
        net_saved += checked_i64(inlined)? - checked_i64(baseline)?;
    }
    Ok(Metric {
        net_saved,
        call_sites: calls.len(),
    })
}

fn apply_edits(source: &str, edits: &[Edit]) -> Result<String> {
    let mut rewriter = SourceRewriter::new(source);
    for edit in edits {
        rewriter.replace(edit.range.clone(), edit.text.clone());
    }
    rewriter.finish().context("apply thin-wrapper edits")
}

fn format_source(workspace_root: &Path, source: &str) -> Result<String> {
    let toolchain = crate::util::nightly_toolchain();
    let mut child = Command::new("rustup")
        .args(["run", toolchain.as_str(), "rustfmt"])
        .args(["--emit", "stdout", "--edition", "2024", "--config-path"])
        .arg(workspace_root.join("rustfmt.toml"))
        .args(["--config", "skip_children=true"])
        .current_dir(workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn rustfmt for thin-wrapper LOC measurement")?;
    let mut stdin = child
        .stdin
        .take()
        .context("open rustfmt stdin for thin-wrapper LOC measurement")?;
    stdin
        .write_all(source.as_bytes())
        .context("write source to rustfmt for thin-wrapper LOC measurement")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("wait for rustfmt thin-wrapper LOC measurement")?;
    if !output.status.success() {
        bail!(
            "rustfmt thin-wrapper LOC measurement failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("rustfmt emitted non-UTF-8 source")
}

fn nonblank_loc(source: &str) -> usize {
    source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn checked_i64(value: usize) -> Result<i64> {
    i64::try_from(value).context("thin-wrapper LOC exceeds i64")
}

fn threshold_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn below_threshold(net_saved: i64, threshold: usize) -> bool {
    net_saved < threshold_i64(threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::violation::Severity;

    #[test]
    fn nested_call_wrapper_reports_three_lines_of_net_loss() {
        let source = r#"
struct AudioSpec;

fn join_frames(_: AudioSpec) -> u32 {
    1
}

pub(crate) fn join_frame_count(spec: AudioSpec) -> u64 {
    u64::from(join_frames(spec))
}

struct Player;

impl Player {
    fn caller(spec: AudioSpec) -> u64 {
        join_frame_count(spec)
    }
}
"#;

        let findings = fixture_violations(&[("src/lib.rs", source)], 10);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("net_saved=-3"));
    }

    #[test]
    fn unit_wrapper_statement_is_measured_without_an_artificial_block() {
        let source = r#"
struct Slot;

impl Slot {
    fn cancel(&mut self) {}
}

fn cancel_slot(slot: &mut Slot) {
    slot.cancel();
}

struct Dispatcher;

impl Dispatcher {
    fn cancel(slot: &mut Slot) {
        cancel_slot(slot);
    }
}
"#;

        let findings = fixture_violations(&[("src/lib.rs", source)], 10);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("net_saved=-3"));
    }

    #[test]
    fn long_error_constructor_earns_the_wrapper() {
        let source = profitable_source(7);
        let findings = fixture_findings(&[("src/lib.rs", &source)]);
        let metric = findings[0]
            .metric
            .unwrap_or_else(|| panic!("{:?}", findings[0].unknown));

        assert!(metric.net_saved >= 10, "net_saved={}", metric.net_saved);
        assert!(fixture_violations(&[("src/lib.rs", &source)], 10).is_empty());
    }

    #[test]
    fn threshold_ten_flags_nine_but_accepts_ten() {
        assert!(below_threshold(9, 10));
        assert!(!below_threshold(10, 10));
    }

    #[test]
    fn qualified_call_refuses_unproven_symbol_resolution() {
        let source = r#"
fn other(value: u32) -> u64 {
    u64::from(value)
}

const wrap: fn(u32) -> u64 = other;

mod nested {
    fn wrap(value: u32) -> u64 {
        u64::from(value)
    }

    fn call(value: u32) -> u64 {
        crate::wrap(value)
    }
}
"#;
        let findings = fixture_findings(&[("src/lib.rs", source)]);
        let finding = findings
            .iter()
            .find(|finding| finding.candidate.name == "wrap")
            .expect("nested wrapper finding");

        assert!(finding.metric.is_none());
        assert_eq!(
            finding.unknown.as_deref(),
            Some("unresolved direct-call target")
        );
    }

    #[test]
    fn aliased_reference_refuses_exact_metric() {
        let source = r#"
fn wrap(value: u32) -> u64 {
    u64::from(value)
}

fn caller(value: u32) -> u64 {
    use self::wrap as callback;
    callback(value)
}
"#;
        let findings = fixture_findings(&[("src/lib.rs", source)]);
        let finding = findings
            .iter()
            .find(|finding| finding.candidate.name == "wrap")
            .expect("wrapper finding");

        assert!(finding.metric.is_none());
        assert_eq!(
            finding.unknown.as_deref(),
            Some("imported or aliased function reference")
        );
    }

    #[test]
    fn production_attribute_references_refuse_exact_metric() {
        for attribute in ["#[case(wrap)]", "#[serde(default = \"wrap\")]"] {
            let source = format!(
                r#"
fn wrap(value: u32) -> u64 {{
    u64::from(value)
}}

{attribute}
fn generated() {{}}
"#
            );
            let findings = fixture_findings(&[("src/lib.rs", &source)]);
            let finding = findings
                .iter()
                .find(|finding| finding.candidate.name == "wrap")
                .expect("wrapper finding");

            assert!(finding.metric.is_none());
            assert_eq!(
                finding.unknown.as_deref(),
                Some("attribute-token reference")
            );
        }
    }

    #[test]
    fn doc_text_is_not_a_function_reference() {
        let source = r#"
fn wrap(value: u32) -> u64 {
    u64::from(value)
}

/// A wrap keeps the example concise.
struct Documented;

documented! {
    /// A wrap inside macro input is still prose.
    struct Generated;
}

impl Documented {
    fn call(value: u32) -> u64 {
        wrap(value)
    }
}
"#;
        let finding = fixture_findings(&[("src/lib.rs", source)])
            .into_iter()
            .find(|finding| finding.candidate.name == "wrap")
            .expect("wrapper finding");

        assert!(finding.metric.is_some(), "{:?}", finding.unknown);
    }

    #[test]
    fn repeated_or_reordered_parameters_refuse_exact_inline() {
        let source = r#"
fn repeated(value: u32) -> u32 {
    combine(value, value)
}

fn reordered(left: u32, right: u32) -> u32 {
    combine(right, left)
}

struct Calls;

impl Calls {
    fn repeated(value: u32) -> u32 {
        super::repeated(value)
    }

    fn reordered(left: u32, right: u32) -> u32 {
        super::reordered(left, right)
    }
}
"#;
        let violations = fixture_violations(&[("src/lib.rs", source)], 10);

        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .all(|violation| violation.message.contains("net_saved=unknown"))
        );
        assert!(
            violations
                .iter()
                .all(|violation| violation.message.contains("exact metric unavailable"))
        );
    }

    #[test]
    fn function_item_and_macro_references_refuse_exact_metric() {
        for (extra, reason) in [
            (
                "const CALLBACK: fn(u32) -> u64 = wrap;",
                "non-call function reference",
            ),
            ("register!(wrap);", "macro-token reference"),
        ] {
            let source = format!(
                r#"
fn wrap(value: u32) -> u64 {{
    u64::from(value)
}}

{extra}

struct Calls;

impl Calls {{
    fn call(value: u32) -> u64 {{
        wrap(value)
    }}
}}
"#
            );
            let violations = fixture_violations(&[("src/lib.rs", &source)], 10);

            assert_eq!(violations.len(), 1);
            assert!(violations[0].message.contains("net_saved=unknown"));
            assert!(violations[0].message.contains(reason));
        }
    }

    #[test]
    fn ambiguous_name_and_associated_call_never_claim_savings() {
        let first = r#"
fn wrap(value: u32) -> u64 {
    u64::from(value)
}

struct Calls;

impl Calls {
    fn call(value: u32) -> u64 {
        Type::wrap(value)
    }
}
"#;
        let second = r#"
fn wrap(value: u16) -> u64 {
    u64::from(value)
}
"#;
        let violations =
            fixture_violations(&[("src/first.rs", first), ("src/second.rs", second)], 10);

        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .all(|violation| violation.message.contains("net_saved=unknown"))
        );
        assert!(
            violations
                .iter()
                .all(|violation| violation.message.contains("ambiguous free-function name"))
        );
    }

    #[test]
    fn imports_that_can_shadow_the_helper_refuse_exact_metric() {
        for import in [
            "use external::wrap;",
            "use external::other as wrap;",
            "use external::*;",
        ] {
            let source = format!(
                r#"
fn wrap(value: u32) -> u64 {{
    u64::from(value)
}}

struct Calls;

impl Calls {{
    fn call(value: u32) -> u64 {{
        {import}
        wrap(value)
    }}
}}
"#
            );
            let violations = fixture_violations(&[("src/lib.rs", &source)], 10);

            assert_eq!(violations.len(), 1);
            assert!(violations[0].message.contains("net_saved=unknown"));
            assert!(
                violations[0]
                    .message
                    .contains("imported or aliased function reference")
                    || violations[0].message.contains("glob import can shadow")
            );
        }
    }

    #[test]
    fn dependency_qualified_call_is_not_assigned_to_local_helper() {
        let source = r#"
fn wrap(value: u32) -> u64 {
    u64::from(value)
}

struct Calls;

impl Calls {
    fn call(value: u32) -> u64 {
        dependency::wrap(value)
    }
}
"#;
        let violations = fixture_violations(&[("src/lib.rs", source)], 10);

        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("net_saved=unknown"));
        assert!(
            violations[0]
                .message
                .contains("unresolved direct-call target")
        );
    }

    #[test]
    fn attached_comments_refuse_exact_metric() {
        for source in [
            r#"
// Keep this semantic boundary visible.
fn widen(value: u32) -> u64 {
    u64::from(value)
}

struct Calls;

impl Calls {
    fn call(value: u32) -> u64 {
        widen(value)
    }
}
"#,
            r#"
fn widen(value: u32) -> u64 {
    u64::from(value)
} // Keep this semantic boundary visible.

struct Calls;

impl Calls {
    fn call(value: u32) -> u64 {
        widen(value)
    }
}
"#,
        ] {
            let finding = fixture_findings(&[("src/lib.rs", source)])
                .into_iter()
                .find(|finding| finding.candidate.name == "widen")
                .expect("wrapper finding");

            assert!(finding.metric.is_none());
            assert!(
                finding
                    .unknown
                    .is_some_and(|reason| reason.contains("attached"))
            );
        }
    }

    #[test]
    fn call_site_comment_refuses_exact_metric() {
        let source = r#"
fn widen(value: u32) -> u64 {
    u64::from(value)
}

fn caller(value: u32) -> u64 {
    widen(
        // Preserve the unit conversion boundary.
        value,
    )
}
"#;
        let finding = fixture_findings(&[("src/lib.rs", source)])
            .into_iter()
            .find(|finding| finding.candidate.name == "widen")
            .expect("wrapper finding");

        assert!(finding.metric.is_none());
        assert_eq!(
            finding.unknown.as_deref(),
            Some("call site contains comments")
        );
    }

    #[test]
    fn attributed_call_refuses_exact_metric() {
        let source = r#"
fn convert(value: u32) -> u64 {
    value as u64
}

fn widen(value: u32) -> u64 {
    crate::convert(value)
}

fn caller(value: u32) -> u64 {
    #[allow(unused_parens)]
    widen(value)
}
"#;
        let finding = fixture_findings(&[("src/lib.rs", source)])
            .into_iter()
            .find(|finding| finding.candidate.name == "widen")
            .expect("wrapper finding");

        assert!(finding.metric.is_none());
        assert_eq!(finding.unknown.as_deref(), Some("call site has attributes"));
    }

    fn profitable_source(call_sites: usize) -> String {
        let methods = (0..call_sites)
            .map(|index| {
                format!(
                    "    fn call_{index}() -> Result<(), DecodeError> {{\n        \
                     Err(channel_disconnected(\"output\"))\n    }}\n"
                )
            })
            .collect::<String>();
        format!(
            r#"
fn channel_disconnected(channel: &'static str) -> DecodeError {{
    DecodeError::backend(WebCodecsError::ChannelDisconnected {{ channel }})
}}

struct Codec;

impl Codec {{
{methods}}}
"#
        )
    }

    fn fixture_violations(sources: &[(&str, &str)], threshold: usize) -> Vec<Violation> {
        fixture_findings(sources)
            .into_iter()
            .filter_map(|finding| finding.violation(threshold))
            .collect()
    }

    fn fixture_findings(sources: &[(&str, &str)]) -> Vec<Finding> {
        let files = fixture_files(sources);
        analyze(fixture_root(), &files)
    }

    fn fixture_files(sources: &[(&str, &str)]) -> Vec<SourceFile> {
        sources
            .iter()
            .map(|(rel, source)| SourceFile {
                rel: (*rel).to_owned(),
                scope: source_scope(rel),
                production: is_production_source(rel),
                source: (*source).to_owned(),
                syntax: syn::parse_file(source).expect("valid Rust fixture"),
            })
            .collect()
    }

    fn fixture_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crate lives below workspace/crates")
    }
}
