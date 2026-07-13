use std::{collections::BTreeMap, fs, ops::Range};

use anyhow::{Context as _, Result};
use proc_macro2::Span;
use quote::ToTokens;
use syn::{
    Attribute, Expr, Fields, FnArg, GenericParam, ImplItem, Item, ItemImpl, ItemStruct, Lit,
    Member, Pat, Path, Stmt, Type, spanned::Spanned,
};

use super::Context;
use crate::common::{
    exclude::{attrs_have_cfg_test, collect_cfg_test_ranges},
    fix::{FixOutcome, SourceRewriter},
    parse::{collect_scopes, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    From,
    Deref,
    Display,
}

impl Kind {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::From => "derivable_from",
            Self::Deref => "derivable_deref",
            Self::Display => "derivable_display",
        }
    }

    const fn derive(self) -> &'static str {
        match self {
            Self::From => "derive_more::From",
            Self::Deref => "derive_more::Deref",
            Self::Display => "derive_more::Display",
        }
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    type_name: String,
    impl_ranges: Vec<Range<usize>>,
    line: usize,
    additions: Vec<Insertion>,
    replacements: Vec<Replacement>,
    skip: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Insertion {
    at: usize,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Replacement {
    range: Range<usize>,
    text: String,
}

pub(super) fn run(ctx: &Context<'_>, kind: Kind, enabled: bool) -> Result<Vec<Violation>> {
    if !enabled {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&src) else {
            continue;
        };
        let rel = relative_to(ctx.workspace_root, &path)
            .to_string_lossy()
            .replace('\\', "/");
        for candidate in candidates(&src, &file, kind) {
            let detail = candidate.skip.map_or_else(
                || {
                    format!(
                        "manual implementation for {} can be collapsed to #[derive({})]",
                        candidate.type_name,
                        kind.derive()
                    )
                },
                |reason| {
                    format!(
                        "manual implementation for {} is derivable but autofix will skip: {reason}",
                        candidate.type_name
                    )
                },
            );
            out.push(Violation::warn(
                kind.id(),
                format!("{rel}:{}:0", candidate.line),
                detail,
            ));
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

pub(super) fn fix(ctx: &Context<'_>, kind: Kind, enabled: bool) -> Result<FixOutcome> {
    if !enabled {
        return Ok(FixOutcome::default());
    }
    let mut outcome = FixOutcome::default();
    for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&src) else {
            continue;
        };
        let rel = relative_to(ctx.workspace_root, &path)
            .to_string_lossy()
            .replace('\\', "/");
        let found = candidates(&src, &file, kind);
        let mut insertions = BTreeMap::<usize, Vec<String>>::new();
        let mut replacements = Vec::<Replacement>::new();
        let mut rewriter = SourceRewriter::new(&src);
        for candidate in found {
            if let Some(reason) = candidate.skip {
                outcome.skipped.push(format!(
                    "{rel}:{}: {}: {reason}",
                    candidate.line, candidate.type_name
                ));
                continue;
            }
            for insertion in candidate.additions {
                let values = insertions.entry(insertion.at).or_default();
                if !values.contains(&insertion.text) {
                    values.push(insertion.text);
                }
            }
            replacements.extend(candidate.replacements);
            for range in candidate.impl_ranges {
                rewriter.replace(range, "");
            }
            outcome.changes.push(format!(
                "{rel}:{}: collapsed {} implementation for {}",
                candidate.line,
                kind.id().trim_start_matches("derivable_"),
                candidate.type_name
            ));
        }
        for (at, values) in insertions {
            rewriter.replace(at..at, values.concat());
        }
        replacements.sort_by(|a, b| {
            (a.range.start, a.range.end, &a.text).cmp(&(b.range.start, b.range.end, &b.text))
        });
        replacements.dedup();
        for replacement in replacements {
            rewriter.replace(replacement.range, replacement.text);
        }
        if rewriter.is_empty() {
            continue;
        }
        let rewritten = rewriter.finish().context("apply derive-collapse edits")?;
        fs::write(&path, rewritten).with_context(|| format!("write {}", path.display()))?;
        outcome.writes += 1;
    }
    Ok(outcome)
}

#[cfg(test)]
pub(super) fn fix_source(source: &str, kind: Kind) -> Result<(String, FixOutcome)> {
    let file = syn::parse_file(source)?;
    let mut outcome = FixOutcome::default();
    let mut insertions = BTreeMap::<usize, Vec<String>>::new();
    let mut replacements = Vec::<Replacement>::new();
    let mut rewriter = SourceRewriter::new(source);
    for candidate in candidates(source, &file, kind) {
        if let Some(reason) = candidate.skip {
            outcome.skipped.push(reason.to_owned());
            continue;
        }
        for insertion in candidate.additions {
            let values = insertions.entry(insertion.at).or_default();
            if !values.contains(&insertion.text) {
                values.push(insertion.text);
            }
        }
        replacements.extend(candidate.replacements);
        for range in candidate.impl_ranges {
            rewriter.replace(range, "");
        }
        outcome.changes.push(candidate.type_name);
    }
    for (at, values) in insertions {
        rewriter.replace(at..at, values.concat());
    }
    replacements.sort_by(|a, b| {
        (a.range.start, a.range.end, &a.text).cmp(&(b.range.start, b.range.end, &b.text))
    });
    replacements.dedup();
    for replacement in replacements {
        rewriter.replace(replacement.range, replacement.text);
    }
    let changed = !rewriter.is_empty();
    let result = rewriter.finish()?;
    outcome.writes = usize::from(changed);
    Ok((result, outcome))
}

fn candidates(src: &str, file: &syn::File, kind: Kind) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut cfg_test_ranges = Vec::new();
    collect_cfg_test_ranges(&file.items, &mut cfg_test_ranges);
    for scope in collect_scopes(file) {
        for &impl_block in &scope.impls {
            if attrs_have_cfg_test(&impl_block.attrs)
                || cfg_test_ranges
                    .iter()
                    .any(|range| range.contains(&impl_block.span().start().line))
            {
                continue;
            }
            let Some(mut candidate) = (match kind {
                Kind::From => from_candidate(src, impl_block),
                Kind::Deref => deref_candidate(src, impl_block, &scope.impls),
                Kind::Display => display_candidate(src, impl_block),
            }) else {
                continue;
            };
            let Some(item) = find_type(&file.items, &candidate.type_name) else {
                candidate.skip = Some("type declared in another file");
                out.push(candidate);
                continue;
            };
            if attrs_have_cfg_test(item_attrs(item)) {
                continue;
            }
            if has_nontrivial_generics(item) || impl_block.generics.where_clause.is_some() {
                candidate.skip = Some("generics with non-trivial bounds or where-clause");
            } else if cfg_tokens(item_attrs(item)) != cfg_tokens(&impl_block.attrs) {
                candidate.skip = Some("#[cfg] on impl differs from type");
            } else {
                complete_additions(src, item, impl_block, kind, &scope.impls, &mut candidate);
            }
            out.push(candidate);
        }
    }
    coalesce_deref_mut(src, file, out, kind)
}

fn find_type<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
    for item in items {
        match item {
            Item::Struct(value) if value.ident == name => return Some(item),
            Item::Enum(value) if value.ident == name => return Some(item),
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content
                    && let Some(found) = find_type(nested, name)
                {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Struct(value) => &value.attrs,
        Item::Enum(value) => &value.attrs,
        _ => &[],
    }
}

fn item_generics(item: &Item) -> &syn::Generics {
    match item {
        Item::Struct(value) => &value.generics,
        Item::Enum(value) => &value.generics,
        _ => unreachable!(),
    }
}

fn has_nontrivial_generics(item: &Item) -> bool {
    let generics = item_generics(item);
    generics.where_clause.is_some()
        || generics.params.iter().any(|param| match param {
            GenericParam::Type(value) => !value.bounds.is_empty() || value.default.is_some(),
            GenericParam::Lifetime(value) => !value.bounds.is_empty(),
            GenericParam::Const(_) => true,
        })
}

fn cfg_tokens(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .map(|attr| attr.meta.to_token_stream().to_string())
        .collect()
}

fn impl_trait(impl_block: &ItemImpl) -> Option<&Path> {
    impl_block.trait_.as_ref().map(|(_, path, _)| path)
}

fn path_ends(path: &Path, expected: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == expected)
}

fn impl_range(src: &str, impl_block: &ItemImpl) -> Range<usize> {
    let mut range = impl_block.span().byte_range();
    let start = line_start(src, range.start);
    if src[start..range.start]
        .bytes()
        .all(|byte| byte.is_ascii_whitespace())
    {
        range.start = start;
    }
    while range.end < src.len() && src.as_bytes()[range.end].is_ascii_whitespace() {
        range.end += 1;
    }
    if range.end < src.len()
        && let Some(last_newline) = src[..range.end].rfind('\n')
    {
        range.end = last_newline + 1;
    }
    range
}

fn from_candidate(src: &str, impl_block: &ItemImpl) -> Option<Candidate> {
    let trait_path = impl_trait(impl_block)?;
    if !path_ends(trait_path, "From") || impl_block.items.len() != 1 {
        return None;
    }
    let segment = trait_path.segments.last()?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(source_ty) = args.args.first()? else {
        return None;
    };
    let ImplItem::Fn(method) = &impl_block.items[0] else {
        return None;
    };
    if method.sig.ident != "from" || method.sig.inputs.len() != 1 {
        return None;
    }
    let FnArg::Typed(arg) = method.sig.inputs.first()? else {
        return None;
    };
    if arg.ty.as_ref() != source_ty {
        return None;
    }
    let Pat::Ident(param) = arg.pat.as_ref() else {
        return None;
    };
    let [Stmt::Expr(body, None)] = method.block.stmts.as_slice() else {
        return None;
    };
    let type_name = self_ty_name(&impl_block.self_ty)?;
    if !is_from_ctor(body, &param.ident, &type_name) {
        return None;
    }
    Some(Candidate {
        type_name,
        impl_ranges: vec![impl_range(src, impl_block)],
        line: impl_block.impl_token.span.start().line,
        additions: Vec::new(),
        replacements: Vec::new(),
        skip: impl_block
            .generics
            .where_clause
            .as_ref()
            .map(|_| "impl has a where-clause"),
    })
}

fn is_from_ctor(expr: &Expr, param: &syn::Ident, type_name: &str) -> bool {
    match expr {
        Expr::Call(call) if call.args.len() == 1 => {
            let Some(arg) = call.args.first() else {
                return false;
            };
            if !matches!(arg, Expr::Path(path) if path.path.is_ident(param)) {
                return false;
            }
            matches!(call.func.as_ref(), Expr::Path(path) if {
                let names: Vec<_> = path.path.segments.iter().map(|s| s.ident.to_string()).collect();
                names == ["Self"] || names == [type_name] || (names.len() == 2 && (names[0] == "Self" || names[0] == type_name))
            })
        }
        Expr::Struct(value) if value.fields.len() == 1 && value.rest.is_none() => {
            let names: Vec<_> = value
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            (names == ["Self"] || names == [type_name])
                && matches!(value.fields.first().map(|f| &f.expr), Some(Expr::Path(path)) if path.path.is_ident(param))
        }
        _ => false,
    }
}

fn deref_candidate(src: &str, impl_block: &ItemImpl, _impls: &[&ItemImpl]) -> Option<Candidate> {
    let trait_path = impl_trait(impl_block)?;
    if !path_ends(trait_path, "Deref") || impl_block.items.len() != 2 {
        return None;
    }
    let target = impl_block.items.iter().find_map(|item| match item {
        ImplItem::Type(value) if value.ident == "Target" => Some(&value.ty),
        _ => None,
    })?;
    let method = impl_block.items.iter().find_map(|item| match item {
        ImplItem::Fn(value) if value.sig.ident == "deref" => Some(value),
        _ => None,
    })?;
    let [Stmt::Expr(Expr::Reference(reference), None)] = method.block.stmts.as_slice() else {
        return None;
    };
    let Expr::Field(field) = reference.expr.as_ref() else {
        return None;
    };
    if reference.mutability.is_some()
        || !matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self"))
    {
        return None;
    }
    let type_name = self_ty_name(&impl_block.self_ty)?;
    Some(Candidate {
        type_name,
        impl_ranges: vec![impl_range(src, impl_block)],
        line: impl_block.impl_token.span.start().line,
        additions: vec![Insertion {
            at: 0,
            text: format!(
                "{}|{}",
                member_name(&field.member),
                target.to_token_stream()
            ),
        }],
        replacements: Vec::new(),
        skip: None,
    })
}

fn member_name(member: &Member) -> String {
    match member {
        Member::Named(value) => value.to_string(),
        Member::Unnamed(value) => value.index.to_string(),
    }
}

fn display_candidate(src: &str, impl_block: &ItemImpl) -> Option<Candidate> {
    let trait_path = impl_trait(impl_block)?;
    if !path_ends(trait_path, "Display") || impl_block.items.len() != 1 {
        return None;
    }
    let ImplItem::Fn(method) = &impl_block.items[0] else {
        return None;
    };
    if method.sig.ident != "fmt" || method.block.stmts.len() != 1 {
        return None;
    }
    let formatter = method.sig.inputs.iter().find_map(|input| match input {
        FnArg::Typed(value) => match value.pat.as_ref() {
            Pat::Ident(ident) => Some(ident.ident.to_string()),
            _ => None,
        },
        FnArg::Receiver(_) => None,
    })?;
    let Stmt::Expr(body, None) = &method.block.stmts[0] else {
        return None;
    };
    let attr = display_attribute(src, body, &formatter)?;
    Some(Candidate {
        type_name: self_ty_name(&impl_block.self_ty)?,
        impl_ranges: vec![impl_range(src, impl_block)],
        line: impl_block.impl_token.span.start().line,
        additions: vec![Insertion { at: 0, text: attr }],
        replacements: Vec::new(),
        skip: None,
    })
}

fn display_attribute(src: &str, body: &Expr, formatter: &str) -> Option<String> {
    if let Expr::MethodCall(call) = body {
        if call.method == "write_str"
            && matches!(call.receiver.as_ref(), Expr::Path(path) if path.path.is_ident(formatter))
            && call.args.len() == 1
        {
            let Expr::Lit(value) = call.args.first()? else {
                return None;
            };
            if !matches!(value.lit, Lit::Str(_)) {
                return None;
            }
            return Some(format!(
                "#[display({})]\n",
                source_slice(src, value.span())?
            ));
        }
        if call.method == "fmt" && call.args.len() == 1 {
            let Expr::Field(field) = call.receiver.as_ref() else {
                return None;
            };
            if matches!(call.args.first(), Some(Expr::Path(path)) if path.path.is_ident(formatter))
                && matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self"))
                && matches!(&field.member, Member::Unnamed(index) if index.index == 0)
            {
                return Some("#[display(\"{_0}\")]\n".to_owned());
            }
        }
    }
    let Expr::Macro(mac) = body else { return None };
    if !path_ends(&mac.mac.path, "write") {
        return None;
    }
    let args = mac.mac.tokens.clone();
    let parsed = syn::parse2::<WriteArgs>(args).ok()?;
    if !matches!(&parsed.formatter, Expr::Path(path) if path.path.is_ident(formatter)) {
        return None;
    }
    if parsed.format.value().contains("{:#") {
        return None;
    }
    if parsed.args.len() == 1 && parsed.format.value() == "{}" && is_self_zero(&parsed.args[0]) {
        return Some("#[display(\"{_0}\")]\n".to_owned());
    }
    let lit = source_slice(src, parsed.format.span())?;
    let rendered = parsed
        .args
        .iter()
        .map(|arg| display_arg(src, arg))
        .collect::<Option<Vec<_>>>()?;
    if rendered.is_empty() {
        Some(format!("#[display({lit})]\n"))
    } else {
        Some(format!("#[display({lit}, {})]\n", rendered.join(", ")))
    }
}

struct WriteArgs {
    formatter: Expr,
    format: syn::LitStr,
    args: Vec<Expr>,
}

impl syn::parse::Parse for WriteArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let formatter = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let format = input.parse()?;
        let mut args = Vec::new();
        while input.parse::<Option<syn::Token![,]>>()?.is_some() && !input.is_empty() {
            args.push(input.parse()?);
        }
        Ok(Self {
            formatter,
            format,
            args,
        })
    }
}

fn source_slice(src: &str, span: Span) -> Option<&str> {
    src.get(span.byte_range())
}

fn display_arg(src: &str, arg: &Expr) -> Option<String> {
    let raw = source_slice(src, arg.span())?;
    Some(raw.strip_prefix("self.").unwrap_or(raw).to_owned())
}

fn is_self_zero(expr: &Expr) -> bool {
    matches!(expr, Expr::Field(field)
        if matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self"))
            && matches!(&field.member, Member::Unnamed(index) if index.index == 0))
}

fn complete_additions(
    src: &str,
    item: &Item,
    impl_block: &ItemImpl,
    kind: Kind,
    impls: &[&ItemImpl],
    candidate: &mut Candidate,
) {
    let item_start = item_declaration_start(src, item);
    match kind {
        Kind::From => complete_from(src, item, impl_block, impls, item_start, candidate),
        Kind::Deref => complete_deref(src, item, item_start, candidate),
        Kind::Display => {
            if matches!(item, Item::Enum(_)) {
                candidate.skip = Some("enum Display requires per-variant analysis");
                return;
            }
            let attr = candidate.additions.pop().expect("display attribute");
            add_derive(src, item, kind.derive(), item_start, candidate);
            candidate.additions.push(Insertion {
                at: item_start,
                text: indent_attribute(src, item_start, &attr.text),
            });
        }
    }
}

fn complete_from(
    src: &str,
    item: &Item,
    impl_block: &ItemImpl,
    impls: &[&ItemImpl],
    item_start: usize,
    candidate: &mut Candidate,
) {
    if let Item::Struct(value) = item {
        if field_count(&value.fields) != 1 {
            candidate.skip = Some("struct is not a single-field newtype");
            return;
        }
        let other_from = impls.iter().any(|other| {
            !std::ptr::eq(*other, impl_block)
                && self_ty_name(&other.self_ty).as_deref() == Some(&candidate.type_name)
                && impl_trait(other).is_some_and(|path| path_ends(path, "From"))
        });
        if other_from {
            candidate.skip = Some("other manual From impl conflicts with blanket newtype derive");
            return;
        }
    }
    add_derive(src, item, Kind::From.derive(), item_start, candidate);
    if let Item::Enum(value) = item {
        let Some(variant_name) = from_variant(impl_block, &candidate.type_name) else {
            candidate.skip = Some("enum constructor is not an explicit variant");
            return;
        };
        let Some(variant) = value.variants.iter().find(|v| v.ident == variant_name) else {
            candidate.skip = Some("enum variant not found");
            return;
        };
        candidate.additions.push(Insertion {
            at: line_start(src, variant.ident.span().byte_range().start),
            text: indent_attribute(
                src,
                line_start(src, variant.ident.span().byte_range().start),
                "#[from]\n",
            ),
        });
    }
}

fn from_variant(impl_block: &ItemImpl, type_name: &str) -> Option<String> {
    let method = impl_block.items.iter().find_map(|item| match item {
        ImplItem::Fn(value) => Some(value),
        _ => None,
    })?;
    let [Stmt::Expr(Expr::Call(call), None)] = method.block.stmts.as_slice() else {
        return None;
    };
    let Expr::Path(path) = call.func.as_ref() else {
        return None;
    };
    let names: Vec<_> = path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    if names.len() == 2 && (names[0] == "Self" || names[0] == type_name) {
        Some(names[1].clone())
    } else {
        None
    }
}

fn complete_deref(src: &str, item: &Item, item_start: usize, candidate: &mut Candidate) {
    let Item::Struct(value) = item else {
        candidate.skip = Some("Deref derive is limited to structs in this wave");
        return;
    };
    let Some(encoded) = candidate.additions.pop() else {
        return;
    };
    let Some((member, target)) = encoded.text.split_once('|') else {
        return;
    };
    let Some((field, field_ty)) = struct_field(value, member) else {
        candidate.skip = Some("multi-field struct has no clear deref field");
        return;
    };
    if normalized_tokens(field_ty) != target.replace(' ', "") {
        candidate.skip = Some("Target type differs from selected field type");
        return;
    }
    add_derive(src, item, Kind::Deref.derive(), item_start, candidate);
    if field_count(&value.fields) > 1 {
        let field_start = field_declaration_start(src, field);
        candidate.additions.push(Insertion {
            at: field_start,
            text: indent_attribute(src, field_start, "#[deref]\n"),
        });
    }
}

fn add_derive(src: &str, item: &Item, derive: &str, item_start: usize, candidate: &mut Candidate) {
    if let Some(attr) = item_attrs(item)
        .iter()
        .find(|attr| attr.path().is_ident("derive"))
    {
        let range = attr.span().byte_range();
        let Some(existing) = src.get(range.clone()) else {
            return;
        };
        if existing.contains(derive) {
            return;
        }
        candidate.replacements.push(Replacement {
            range,
            text: merge_derive(existing, derive),
        });
        return;
    }
    candidate.additions.push(Insertion {
        at: item_start,
        text: indent_attribute(src, item_start, &format!("#[derive({derive})]\n")),
    });
}

fn merge_derive(existing: &str, derive: &str) -> String {
    let Some(close) = existing.rfind(')') else {
        return existing.to_owned();
    };
    if !existing.contains('\n') {
        let mut merged = existing.to_owned();
        merged.insert_str(close, &format!(", {derive}"));
        return merged;
    }
    let close_line = existing[..close].rfind('\n').map_or(0, |pos| pos + 1);
    let closing_indent = &existing[close_line..close];
    let before_close = existing[..close_line].trim_end_matches([' ', '\t', '\n']);
    let entry_start = before_close.rfind('\n').map_or(0, |pos| pos + 1);
    let entry_line = &before_close[entry_start..];
    let entry_indent: String = entry_line
        .chars()
        .take_while(char::is_ascii_whitespace)
        .collect();
    let indent = if entry_line.trim().is_empty() {
        format!("{closing_indent}    ")
    } else {
        entry_indent
    };
    let comma = if before_close.ends_with(',') { "" } else { "," };
    format!(
        "{}{}\n{indent}{derive},\n{}{}",
        before_close,
        comma,
        &existing[close_line..close],
        &existing[close..]
    )
}

fn item_declaration_start(src: &str, item: &Item) -> usize {
    let byte = match item {
        Item::Struct(value) => match &value.vis {
            syn::Visibility::Inherited => value.struct_token.span.byte_range().start,
            visibility => visibility.span().byte_range().start,
        },
        Item::Enum(value) => match &value.vis {
            syn::Visibility::Inherited => value.enum_token.span.byte_range().start,
            visibility => visibility.span().byte_range().start,
        },
        _ => item.span().byte_range().start,
    };
    line_start(src, byte)
}

fn field_declaration_start(src: &str, field: &syn::Field) -> usize {
    let byte = match &field.vis {
        syn::Visibility::Inherited => field.ident.as_ref().map_or_else(
            || field.ty.span().byte_range().start,
            |ident| ident.span().byte_range().start,
        ),
        visibility => visibility.span().byte_range().start,
    };
    line_start(src, byte)
}

fn line_start(src: &str, at: usize) -> usize {
    src[..at].rfind('\n').map_or(0, |pos| pos + 1)
}

fn indent_attribute(src: &str, at: usize, attribute: &str) -> String {
    let indent: String = src[at..]
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect();
    attribute
        .lines()
        .map(|line| format!("{indent}{line}\n"))
        .collect()
}

fn normalized_tokens(ty: &Type) -> String {
    ty.to_token_stream().to_string().replace(' ', "")
}

fn field_count(fields: &Fields) -> usize {
    fields.iter().count()
}

fn struct_field<'a>(value: &'a ItemStruct, member: &str) -> Option<(&'a syn::Field, &'a Type)> {
    value.fields.iter().enumerate().find_map(|(index, field)| {
        let matches = field
            .ident
            .as_ref()
            .map_or_else(|| index.to_string() == member, |ident| ident == member);
        matches.then_some((field, &field.ty))
    })
}

fn coalesce_deref_mut(
    src: &str,
    file: &syn::File,
    mut candidates: Vec<Candidate>,
    kind: Kind,
) -> Vec<Candidate> {
    if kind != Kind::Deref {
        return candidates;
    }
    let mut cfg_test_ranges = Vec::new();
    collect_cfg_test_ranges(&file.items, &mut cfg_test_ranges);
    for scope in collect_scopes(file) {
        for mutable in scope.impls {
            if attrs_have_cfg_test(&mutable.attrs)
                || cfg_test_ranges
                    .iter()
                    .any(|range| range.contains(&mutable.span().start().line))
            {
                continue;
            }
            let Some(path) = impl_trait(mutable) else {
                continue;
            };
            if !path_ends(path, "DerefMut") || mutable.items.len() != 1 {
                continue;
            }
            let Some(type_name) = self_ty_name(&mutable.self_ty) else {
                continue;
            };
            let Some(candidate) = candidates.iter_mut().find(|c| c.type_name == type_name) else {
                continue;
            };
            let ImplItem::Fn(method) = &mutable.items[0] else {
                continue;
            };
            let [Stmt::Expr(Expr::Reference(reference), None)] = method.block.stmts.as_slice()
            else {
                continue;
            };
            let Expr::Field(field) = reference.expr.as_ref() else {
                continue;
            };
            if reference.mutability.is_none()
                || !matches!(field.base.as_ref(), Expr::Path(p) if p.path.is_ident("self"))
            {
                continue;
            }
            candidate.impl_ranges.push(impl_range(src, mutable));
            if let Some(item) = find_type(&file.items, &type_name) {
                let item_start = item_declaration_start(src, item);
                add_derive(src, item, "derive_more::DerefMut", item_start, candidate);
                if let Item::Struct(value) = item
                    && field_count(&value.fields) > 1
                    && let Some((selected, _)) = struct_field(value, &member_name(&field.member))
                {
                    let field_start = field_declaration_start(src, selected);
                    candidate.additions.push(Insertion {
                        at: field_start,
                        text: indent_attribute(src, field_start, "#[deref_mut]\n"),
                    });
                }
            }
        }
    }
    candidates
}
