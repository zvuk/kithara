use std::{collections::BTreeSet, fs, ops::Range};

use anyhow::{Context as _, Result};
use proc_macro2::{Delimiter, TokenTree};
use quote::{ToTokens, quote};
use syn::{
    Expr, ExprAwait, ExprField, ExprMethodCall, FnArg, Ident, ImplItem, ItemImpl, Member, Pat,
    Stmt, parse_quote,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{
    Check, Context,
    derivable_support::{crate_manifest, deletion_range, item_blocks, line_start},
};
use crate::common::{
    exclude::attrs_have_cfg_test,
    fix::{FixOutcome, SourceRewriter, block::BlockRange},
    parse::{collect_scopes, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "derivable_delegation";
const MAX_RAW_STRING_HASHES: usize = 255;

pub(crate) struct DerivableDelegation;

impl Check for DerivableDelegation {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.derivable_delegation;
        if !cfg.enabled {
            return Ok(Vec::new());
        }
        let mut violations = Vec::new();
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
            for candidate in candidates(
                &src,
                &file,
                cfg.trait_min_methods,
                cfg.inherent_min_methods,
                &cfg.blocking_impl_attrs,
            ) {
                let detail = candidate.skip.map_or_else(
                    || {
                        if candidate.existing_blocks == 0 {
                            format!(
                                "{} impl for {} has {} derivable forwarders across {} field group(s); wrap the forwarding subset in a delegate::delegate! block.",
                                candidate.kind,
                                candidate.target,
                                candidate.methods,
                                candidate.fields
                            )
                        } else {
                            format!(
                                "{} impl for {} has {} existing delegate block(s) and {} derivable forwarder(s) across {} target(s); normalize them into one delegate::delegate! block.",
                                candidate.kind,
                                candidate.target,
                                candidate.existing_blocks,
                                candidate.methods,
                                candidate.fields
                            )
                        }
                    },
                    |ref reason| {
                        format!(
                            "{} impl for {} has {} existing delegate block(s) and {} derivable forwarder(s) across {} target(s) but autofix will skip: {reason}",
                            candidate.kind,
                            candidate.target,
                            candidate.existing_blocks,
                            candidate.methods,
                            candidate.fields
                        )
                    },
                );
                violations.push(Violation::warn(
                    ID,
                    format!("{rel}:{}:0", candidate.line),
                    detail,
                ));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.derivable_delegation;
        if !cfg.enabled {
            return Ok(FixOutcome::default());
        }
        let mut outcome = FixOutcome::default();
        let mut manifests = BTreeSet::new();
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
            let mut rewriter = SourceRewriter::new(&src);
            for candidate in candidates(
                &src,
                &file,
                cfg.trait_min_methods,
                cfg.inherent_min_methods,
                &cfg.blocking_impl_attrs,
            ) {
                if let Some(reason) = &candidate.skip {
                    outcome.skipped.push(format!(
                        "{rel}:{}: {}: {reason}",
                        candidate.line, candidate.target
                    ));
                    continue;
                }
                for edit in candidate.edits {
                    rewriter.replace(edit.range, edit.text);
                }
                outcome.changes.push(format!(
                    "{rel}:{}: normalized {} delegate block(s) and {} new method(s) for {} across {} target(s)",
                    candidate.line,
                    candidate.existing_blocks,
                    candidate.methods,
                    candidate.target,
                    candidate.fields
                ));
            }
            if rewriter.is_empty() {
                continue;
            }
            let rewritten = rewriter.finish().context("apply delegation edits")?;
            fs::write(&path, rewritten).with_context(|| format!("write {}", path.display()))?;
            outcome.writes += 1;
            if let Some(manifest) = crate_manifest(ctx.workspace_root, &path) {
                manifests.insert(manifest);
            }
        }

        for manifest in manifests {
            let source = fs::read_to_string(&manifest)
                .with_context(|| format!("read {}", manifest.display()))?;
            let Some(rewritten) = crate::manifest::add_workspace_dependency(&source, "delegate")?
            else {
                continue;
            };
            fs::write(&manifest, rewritten)
                .with_context(|| format!("write {}", manifest.display()))?;
            outcome.writes += 1;
            outcome.changes.push(format!(
                "{}: added workspace delegate dependency",
                relative_to(ctx.workspace_root, &manifest).display()
            ));
        }
        Ok(outcome)
    }
}

struct Candidate {
    target: String,
    kind: &'static str,
    existing_blocks: usize,
    fields: usize,
    methods: usize,
    line: usize,
    edits: Vec<Edit>,
    skip: Option<String>,
}

struct Edit {
    range: Range<usize>,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FieldRef {
    Named(String),
    Unnamed(u32),
}

impl FieldRef {
    fn target(&self) -> String {
        match self {
            Self::Named(field) => format!("self.{field}"),
            Self::Unnamed(index) => format!("self.{index}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DelegateTarget {
    key: String,
    source: String,
    simple: bool,
}

impl DelegateTarget {
    fn field(field: &FieldRef) -> Self {
        let source = field.target();
        Self {
            key: source.clone(),
            source,
            simple: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Modifier {
    None,
    Into,
    TryInto,
    Unwrap,
    Expr(String),
    Field(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParamModifier {
    None,
    Into,
    AsRef,
    Newtype,
}

struct ForwardSpec {
    target: DelegateTarget,
    call: Option<Ident>,
    modifier: Modifier,
    param_modifiers: Vec<ParamModifier>,
}

struct ForwardMethod<'a> {
    index: usize,
    method: &'a syn::ImplItemFn,
    spec: ForwardSpec,
}

struct TargetGroup<'a> {
    target: DelegateTarget,
    methods: Vec<ForwardMethod<'a>>,
}

struct ExistingSegment {
    attrs: Vec<String>,
    target: String,
    body: String,
}

struct DelegateSegment<'a> {
    attrs: Vec<String>,
    target: String,
    bodies: Vec<String>,
    methods: Vec<ForwardMethod<'a>>,
}

fn candidates(
    src: &str,
    file: &syn::File,
    trait_min_methods: usize,
    inherent_min_methods: usize,
    blocking: &[String],
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for scope in collect_scopes(file) {
        for impl_block in scope.impls {
            if let Some(candidate) = candidate(
                src,
                impl_block,
                trait_min_methods,
                inherent_min_methods,
                blocking,
            ) {
                out.push(candidate);
            }
        }
    }
    out
}

fn collect_impl_items<'a>(
    src: &str,
    impl_block: &'a ItemImpl,
) -> (Vec<TargetGroup<'a>>, Vec<(usize, &'a syn::ImplItemMacro)>) {
    let mut groups = Vec::<TargetGroup<'_>>::new();
    let mut delegate_macros = Vec::new();
    for (index, item) in impl_block.items.iter().enumerate() {
        match item {
            ImplItem::Fn(method) => {
                if !method_is_supported(src, method) {
                    continue;
                }
                let Some(spec) = forward_spec(src, method) else {
                    continue;
                };
                let group = groups
                    .iter_mut()
                    .find(|group| group.target.key == spec.target.key);
                if let Some(group) = group {
                    group.methods.push(ForwardMethod {
                        index,
                        method,
                        spec,
                    });
                } else {
                    groups.push(TargetGroup {
                        target: spec.target.clone(),
                        methods: vec![ForwardMethod {
                            index,
                            method,
                            spec,
                        }],
                    });
                }
            }
            ImplItem::Macro(item) if is_delegate_macro(item) => {
                delegate_macros.push((index, item));
            }
            _ => {}
        }
    }
    (groups, delegate_macros)
}

fn collect_existing_segments<'a>(
    src: &str,
    delegate_macros: &[(usize, &syn::ImplItemMacro)],
) -> Result<(Vec<DelegateSegment<'a>>, bool), ()> {
    let mut segments = Vec::<DelegateSegment<'_>>::new();
    let mut coalesced = false;
    for (_, item) in delegate_macros {
        for parsed in parse_delegate_segments(src, item)? {
            if let Some(segment) = segments
                .iter_mut()
                .find(|segment| segment.target == parsed.target && segment.attrs == parsed.attrs)
            {
                segment.bodies.push(parsed.body);
                coalesced = true;
            } else {
                segments.push(DelegateSegment {
                    attrs: parsed.attrs,
                    target: parsed.target,
                    bodies: vec![parsed.body],
                    methods: Vec::new(),
                });
            }
        }
    }
    Ok((segments, coalesced))
}

fn candidate(
    src: &str,
    impl_block: &ItemImpl,
    trait_min_methods: usize,
    inherent_min_methods: usize,
    blocking: &[String],
) -> Option<Candidate> {
    if impl_has_blocking_attr(&impl_block.attrs, blocking) {
        return None;
    }
    let (kind, min_methods) = if impl_block.trait_.is_some() {
        ("trait", trait_min_methods)
    } else {
        ("inherent", inherent_min_methods)
    };
    let (groups, delegate_macros) = collect_impl_items(src, impl_block);
    let target = self_ty_name(&impl_block.self_ty).unwrap_or_else(|| "impl".to_owned());
    let line = impl_block.impl_token.span.start().line;
    let Ok((mut segments, coalesced)) = collect_existing_segments(src, &delegate_macros) else {
        return Some(Candidate {
            target,
            kind,
            existing_blocks: delegate_macros.len(),
            fields: groups.len(),
            methods: groups.iter().map(|group| group.methods.len()).sum(),
            line,
            edits: Vec::new(),
            skip: Some("impl has a non-simple delegate! target".to_owned()),
        });
    };

    let mut augmented = false;
    let mut new_group = false;
    let mut augment_groups = Vec::new();
    let mut new_groups = Vec::new();
    for group in groups {
        let group_target = &group.target.source;
        if group.target.simple
            && segments
                .iter()
                .any(|segment| segment.target == *group_target)
        {
            augmented = true;
            augment_groups.push(group);
        } else {
            new_groups.push(group);
        }
    }
    for group in augment_groups {
        let group_target = group.target.source.clone();
        if let Some(segment) = segments
            .iter_mut()
            .find(|segment| segment.target == group_target && segment.attrs.is_empty())
        {
            segment.methods.extend(group.methods);
        } else {
            segments.push(DelegateSegment {
                attrs: Vec::new(),
                target: group_target,
                bodies: Vec::new(),
                methods: group.methods,
            });
        }
    }
    for group in new_groups {
        if group.methods.len() >= min_methods {
            new_group = true;
            segments.push(DelegateSegment {
                attrs: Vec::new(),
                target: group.target.source,
                bodies: Vec::new(),
                methods: group.methods,
            });
        }
    }
    if delegate_macros.len() < 2 && !coalesced && !augmented && !new_group {
        return None;
    }

    let methods = segments.iter().map(|segment| segment.methods.len()).sum();
    let fields = segments
        .iter()
        .map(|segment| segment.target.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let blocks = match item_blocks(src, impl_block) {
        Ok(blocks) => blocks,
        Err(reason) => {
            return Some(Candidate {
                target,
                kind,
                existing_blocks: delegate_macros.len(),
                fields,
                methods,
                line,
                edits: Vec::new(),
                skip: Some(format!("impl item ranges are unsafe: {reason}")),
            });
        }
    };
    let first_index = delegate_macros
        .iter()
        .map(|(index, _)| *index)
        .chain(
            segments
                .iter()
                .flat_map(|segment| &segment.methods)
                .map(|method| method.index),
        )
        .min()?;
    let replacement = render_delegate(src, &segments, &blocks, first_index)?;
    let mut relevant: Vec<_> = delegate_macros
        .iter()
        .map(|(index, _)| *index)
        .chain(
            segments
                .iter()
                .flat_map(|segment| &segment.methods)
                .map(|method| method.index),
        )
        .collect();
    relevant.sort_unstable();
    relevant.dedup();
    let mut replacement = Some(replacement);
    let edits = relevant
        .into_iter()
        .map(|index| Edit {
            range: deletion_range(src, blocks[index].bytes.clone()),
            text: if index == first_index {
                replacement.take().unwrap_or_default()
            } else {
                String::new()
            },
        })
        .collect();
    Some(Candidate {
        target,
        kind,
        existing_blocks: delegate_macros.len(),
        fields,
        methods,
        line,
        edits,
        skip: None,
    })
}

fn is_delegate_macro(item: &syn::ImplItemMacro) -> bool {
    let path = &item.mac.path;
    path.leading_colon.is_none()
        && (path.is_ident("delegate")
            || (path.segments.len() == 2
                && path.segments[0].ident == "delegate"
                && path.segments[1].ident == "delegate"))
}

fn parse_delegate_segments(
    src: &str,
    item: &syn::ImplItemMacro,
) -> Result<Vec<ExistingSegment>, ()> {
    if !item.attrs.is_empty() {
        return Err(());
    }
    let tokens: Vec<_> = item.mac.tokens.clone().into_iter().collect();
    let mut segments = Vec::new();
    let mut cursor = 0;
    while cursor < tokens.len() {
        let mut attrs = Vec::new();
        while let (Some(TokenTree::Punct(hash)), Some(TokenTree::Group(group))) =
            (tokens.get(cursor), tokens.get(cursor + 1))
        {
            if hash.as_char() != '#' || group.delimiter() != Delimiter::Bracket {
                break;
            }
            let start = hash.span().byte_range().start;
            let end = group.span().byte_range().end;
            attrs.push(src.get(start..end).ok_or(())?.to_owned());
            cursor += 2;
        }
        let Some(TokenTree::Ident(to)) = tokens.get(cursor) else {
            return Err(());
        };
        if to != "to" {
            return Err(());
        }
        cursor += 1;
        let target_start = cursor;
        let body = loop {
            let Some(token) = tokens.get(cursor) else {
                return Err(());
            };
            if let TokenTree::Group(group) = token
                && group.delimiter() == Delimiter::Brace
            {
                cursor += 1;
                break group;
            }
            cursor += 1;
        };
        let target = simple_delegate_target(&tokens[target_start..cursor - 1]).ok_or(())?;
        let range = body.span().byte_range();
        let inner = range.start.checked_add(1).ok_or(())?..range.end.checked_sub(1).ok_or(())?;
        let body = src.get(inner).ok_or(())?.to_owned();
        segments.push(ExistingSegment {
            attrs,
            target,
            body,
        });
    }
    Ok(segments)
}

fn simple_delegate_target(tokens: &[TokenTree]) -> Option<String> {
    let [
        TokenTree::Ident(root),
        TokenTree::Punct(dot),
        member,
        rest @ ..,
    ] = tokens
    else {
        return None;
    };
    if root != "self" || dot.as_char() != '.' {
        return None;
    }
    let member = match member {
        TokenTree::Ident(ident) => ident.to_string(),
        TokenTree::Literal(literal) if literal.to_string().parse::<u32>().is_ok() => {
            literal.to_string()
        }
        _ => return None,
    };
    let mut target = format!("self.{member}");
    let mut chunks = rest.chunks_exact(2);
    for chunk in &mut chunks {
        let [TokenTree::Punct(dot), TokenTree::Ident(field)] = chunk else {
            return None;
        };
        if dot.as_char() != '.' {
            return None;
        }
        target.push('.');
        target.push_str(&field.to_string());
    }
    chunks.remainder().is_empty().then_some(target)
}

fn render_delegate(
    src: &str,
    segments: &[DelegateSegment<'_>],
    blocks: &[BlockRange],
    first_index: usize,
) -> Option<String> {
    let item_start = blocks.get(first_index)?.item_bytes.start;
    let item_line = line_start(src, item_start);
    let indent = if src[item_line..item_start]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t'))
    {
        &src[item_line..item_start]
    } else {
        ""
    };
    let segment_indent = format!("{indent}    ");
    let method_indent = format!("{segment_indent}    ");
    let mut output = format!("{indent}delegate::delegate! {{\n");
    for segment in segments {
        for attr in &segment.attrs {
            push_indented(&mut output, attr, &segment_indent);
        }
        output.push_str(&segment_indent);
        output.push_str("to ");
        output.push_str(&segment.target);
        output.push_str(" {\n");
        for body in &segment.bodies {
            push_raw_body(&mut output, body, &method_indent);
        }
        for delegated in &segment.methods {
            render_method(&mut output, src, blocks, delegated, &method_indent)?;
        }
        output.push_str(&segment_indent);
        output.push_str("}\n");
    }
    output.push_str(indent);
    output.push_str("}\n");
    Some(output)
}

fn render_method(
    output: &mut String,
    src: &str,
    blocks: &[BlockRange],
    delegated: &ForwardMethod<'_>,
    indent: &str,
) -> Option<()> {
    let method = delegated.method;
    let item = method.span().byte_range();
    let block = &blocks[delegated.index];
    push_leading_trivia(output, src.get(block.bytes.start..item.start)?, indent);
    for attr in &method.attrs {
        let source = src.get(attr.span().byte_range())?;
        if source.trim_start().starts_with("/**") {
            output.push_str(indent);
            output.push_str(&block_doc_attribute(attr)?);
            output.push('\n');
        } else {
            push_indented(output, source, indent);
        }
    }
    match &delegated.spec.modifier {
        Modifier::None => {}
        Modifier::Into => push_indented(output, "#[into]", indent),
        Modifier::TryInto => push_indented(output, "#[try_into]", indent),
        Modifier::Unwrap => push_indented(output, "#[unwrap]", indent),
        Modifier::Expr(template) => {
            push_indented(output, &format!("#[expr({template})]"), indent);
        }
        Modifier::Field(attr) => push_indented(output, attr, indent),
    }
    if let Some(call) = &delegated.spec.call {
        push_indented(output, &format!("#[call({call})]"), indent);
    }
    let visibility = &method.vis;
    let mut signature = method.sig.clone();
    for (input, modifier) in signature
        .inputs
        .iter_mut()
        .filter(|input| matches!(input, FnArg::Typed(_)))
        .zip(&delegated.spec.param_modifiers)
    {
        let FnArg::Typed(input) = input else {
            continue;
        };
        let attr = match modifier {
            ParamModifier::None => continue,
            ParamModifier::Into => parse_quote!(#[into]),
            ParamModifier::AsRef => parse_quote!(#[as_ref]),
            ParamModifier::Newtype => parse_quote!(#[newtype]),
        };
        input.attrs.push(attr);
    }
    output.push_str(indent);
    output.push_str(
        &quote!(#visibility #signature)
            .to_string()
            .replace("# [", "#["),
    );
    output.push(';');
    let trailing = src.get(item.end..block.bytes.end)?.trim();
    if !trailing.is_empty() {
        output.push(' ');
        output.push_str(trailing);
    }
    output.push('\n');
    Some(())
}

fn push_raw_body(output: &mut String, body: &str, indent: &str) {
    let lines: Vec<_> = body.lines().collect();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |index| index + 1);
    let lines = &lines[start..end];
    let common_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count()
        })
        .min()
        .unwrap_or(0);
    for line in lines {
        if line.trim().is_empty() {
            output.push('\n');
        } else {
            output.push_str(indent);
            output.push_str(&line[common_indent..]);
            output.push('\n');
        }
    }
}

fn push_leading_trivia(output: &mut String, trivia: &str, indent: &str) {
    for line in trivia.lines().filter(|line| !line.trim().is_empty()) {
        push_indented(output, line.trim_start(), indent);
    }
}

fn push_indented(output: &mut String, text: &str, indent: &str) {
    for line in text.lines() {
        output.push_str(indent);
        output.push_str(line);
        output.push('\n');
    }
}

fn impl_has_blocking_attr(attrs: &[syn::Attribute], blocking: &[String]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr
            .path()
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
        // Whitespace-stripped meta tokens catch `#[cfg_attr(cond, uniffi::export)]`.
        let meta: String = attr
            .meta
            .to_token_stream()
            .to_string()
            .split_whitespace()
            .collect();
        blocking.iter().any(|entry| {
            path == *entry || path.ends_with(&format!("::{entry}")) || meta.contains(entry.as_str())
        })
    })
}

fn method_is_supported(src: &str, method: &syn::ImplItemFn) -> bool {
    if attrs_have_cfg_test(&method.attrs) {
        return false;
    }
    for attr in &method.attrs {
        // delegate 0.13.5 forwards other attrs verbatim (attributes.rs:446,
        // lib.rs:1069), but these names are interpreted as its method DSL.
        if attr.path().segments.last().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "call"
                    | "into"
                    | "try_into"
                    | "unwrap"
                    | "await"
                    | "expr"
                    | "field"
                    | "through"
                    | "const"
            )
        }) {
            return false;
        }
        let source = src.get(attr.span().byte_range());
        if is_doc_attr(attr)
            && source.is_some_and(|text| text.trim_start().starts_with("/**"))
            && block_doc_attribute(attr).is_none()
        {
            return false;
        }
    }
    true
}

fn is_doc_attr(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("doc")
}

fn block_doc_attribute(attr: &syn::Attribute) -> Option<String> {
    let syn::Meta::NameValue(meta) = &attr.meta else {
        return None;
    };
    let Expr::Lit(value) = &meta.value else {
        return None;
    };
    let syn::Lit::Str(value) = &value.lit else {
        return None;
    };
    Some(format!("#[doc = {}]", raw_string_literal(&value.value())?))
}

fn raw_string_literal(value: &str) -> Option<String> {
    for count in 0..=MAX_RAW_STRING_HASHES {
        let hashes = "#".repeat(count);
        if !value.contains(&format!("\"{hashes}")) {
            return Some(format!("r{hashes}\"{value}\"{hashes}"));
        }
    }
    None
}

fn forward_spec(src: &str, method: &syn::ImplItemFn) -> Option<ForwardSpec> {
    let [Stmt::Expr(expr, None)] = method.block.stmts.as_slice() else {
        return None;
    };
    if let Some(spec) = nested_field_spec(method, expr) {
        return Some(spec);
    }
    if !has_single_self_call_chain(expr) {
        return None;
    }
    if let Expr::MethodCall(wrapper) = expr
        && wrapper.args.is_empty()
        && wrapper.turbofish.is_none()
    {
        let modifier = match wrapper.method.to_string().as_str() {
            "into" => Some(Modifier::Into),
            "try_into" => Some(Modifier::TryInto),
            "unwrap" => Some(Modifier::Unwrap),
            _ => None,
        };
        if let Some(modifier) = modifier
            && let Some(call) = exact_delegated_call(&wrapper.receiver, method)
            && let Some(spec) = call_spec(src, method, call, modifier)
        {
            return Some(spec);
        }
    }
    if let Some(call) = exact_delegated_call(expr, method)
        && let Some(spec) = call_spec(src, method, call, Modifier::None)
    {
        return Some(spec);
    }
    let analysis = unique_delegated_call(expr, method)?;
    if matches!(method.sig.output, syn::ReturnType::Default) {
        return None;
    }
    let expr_range = expr.span().byte_range();
    let call_range = delegated_call_range(expr, analysis.call, method)?;
    if call_range.start < expr_range.start || call_range.end > expr_range.end {
        return None;
    }
    let mut template = src.get(expr_range.start..call_range.start)?.to_owned();
    template.push('$');
    template.push_str(src.get(call_range.end..expr_range.end)?);
    call_spec_with_modifiers(
        src,
        method,
        analysis.call,
        Modifier::Expr(template),
        analysis.param_modifiers,
    )
}

fn nested_field_spec(method: &syn::ImplItemFn, expr: &Expr) -> Option<ForwardSpec> {
    let (expr, reference) = match expr {
        Expr::Reference(reference) => (
            reference.expr.as_ref(),
            Some(reference.mutability.is_some()),
        ),
        expr => (expr, None),
    };
    let Expr::Field(field) = expr else {
        return None;
    };
    let Expr::Field(owner) = field.base.as_ref() else {
        return None;
    };
    if !matches!(&*owner.base, Expr::Path(path) if path.path.is_ident("self")) {
        return None;
    }
    let field_ref = match &owner.member {
        Member::Named(field) => FieldRef::Named(field.to_string()),
        Member::Unnamed(index) => FieldRef::Unnamed(index.index),
    };
    let member = match &field.member {
        Member::Named(field) => field.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    };
    let attr = match reference {
        None if method.sig.ident == member => "#[field]".to_owned(),
        None => format!("#[field({member})]"),
        Some(false) => format!("#[field(&{member})]"),
        Some(true) => format!("#[field(&mut {member})]"),
    };
    Some(ForwardSpec {
        target: DelegateTarget::field(&field_ref),
        call: None,
        modifier: Modifier::Field(attr),
        param_modifiers: method_param_modifiers(method)?,
    })
}

fn method_param_modifiers(method: &syn::ImplItemFn) -> Option<Vec<ParamModifier>> {
    Some(
        method_params(method)?
            .into_iter()
            .map(|_| ParamModifier::None)
            .collect(),
    )
}

fn method_params(method: &syn::ImplItemFn) -> Option<Vec<String>> {
    method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Receiver(_) => None,
            FnArg::Typed(arg) => Some(match &*arg.pat {
                Pat::Ident(ident) if ident.subpat.is_none() && arg.attrs.is_empty() => {
                    Some(ident.ident.to_string())
                }
                _ => None,
            }),
        })
        .collect()
}

fn exact_delegated_call<'a>(
    expr: &'a Expr,
    method: &syn::ImplItemFn,
) -> Option<&'a ExprMethodCall> {
    match (expr, method.sig.asyncness.is_some()) {
        (Expr::MethodCall(call), false) => Some(call),
        (Expr::Await(await_expr), true) => match await_expr.base.as_ref() {
            Expr::MethodCall(call) => Some(call),
            _ => None,
        },
        _ => None,
    }
}

fn call_spec(
    src: &str,
    method: &syn::ImplItemFn,
    call: &ExprMethodCall,
    modifier: Modifier,
) -> Option<ForwardSpec> {
    let param_modifiers = call_param_modifiers(method, call)?;
    call_spec_with_modifiers(src, method, call, modifier, param_modifiers)
}

fn call_spec_with_modifiers(
    src: &str,
    method: &syn::ImplItemFn,
    call: &ExprMethodCall,
    modifier: Modifier,
    param_modifiers: Vec<ParamModifier>,
) -> Option<ForwardSpec> {
    if call.turbofish.is_some() {
        return None;
    }
    Some(ForwardSpec {
        target: delegate_target(src, &call.receiver)?,
        call: (call.method != method.sig.ident).then(|| call.method.clone()),
        modifier,
        param_modifiers,
    })
}

fn delegate_target(src: &str, receiver: &Expr) -> Option<DelegateTarget> {
    if let Some(source) = simple_target(receiver) {
        return Some(DelegateTarget {
            key: source.clone(),
            source,
            simple: true,
        });
    }
    if !contains_self_field(receiver) {
        return None;
    }
    let source = src.get(receiver.span().byte_range())?.to_owned();
    Some(DelegateTarget {
        key: receiver.to_token_stream().to_string(),
        source,
        simple: false,
    })
}

fn simple_target(expr: &Expr) -> Option<String> {
    let Expr::Field(field) = expr else {
        return None;
    };
    if matches!(&*field.base, Expr::Path(path) if path.path.is_ident("self")) {
        return Some(format!("self.{}", member_text(&field.member)));
    }
    let base = simple_target(&field.base)?;
    let Member::Named(member) = &field.member else {
        return None;
    };
    Some(format!("{base}.{member}"))
}

fn member_text(member: &Member) -> String {
    match member {
        Member::Named(member) => member.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    }
}

fn call_param_modifiers(
    method: &syn::ImplItemFn,
    call: &ExprMethodCall,
) -> Option<Vec<ParamModifier>> {
    if call.turbofish.is_some() {
        return None;
    }
    let params = method_params(method)?;
    if params.len() != call.args.len() {
        return None;
    }
    call.args
        .iter()
        .zip(params)
        .map(|(arg, param)| argument_modifier(arg, &param))
        .collect()
}

struct CallAnalysis<'a> {
    call: &'a ExprMethodCall,
    param_modifiers: Vec<ParamModifier>,
}

fn unique_delegated_call<'a>(expr: &'a Expr, method: &syn::ImplItemFn) -> Option<CallAnalysis<'a>> {
    let mut collector = SelfMethodCallCollector::default();
    collector.visit_expr(expr);
    let calls = collector.calls;
    if independent_self_call_chains(&calls) != 1 {
        return None;
    }
    let matching: Vec<_> = calls
        .iter()
        .filter_map(|call| call_param_modifiers(method, call).map(|modifiers| (*call, modifiers)))
        .collect();
    let mut outermost = matching.iter().filter(|(call, _)| {
        !matching.iter().any(|(outer, _)| {
            call.span().byte_range() != outer.span().byte_range()
                && receiver_spine_contains(&outer.receiver, call)
        })
    });
    let (call, param_modifiers) = outermost.next()?;
    if outermost.next().is_some() {
        return None;
    }
    Some(CallAnalysis {
        call,
        param_modifiers: param_modifiers.clone(),
    })
}

fn has_single_self_call_chain(expr: &Expr) -> bool {
    let mut collector = SelfMethodCallCollector::default();
    collector.visit_expr(expr);
    independent_self_call_chains(&collector.calls) == 1
}

fn independent_self_call_chains(calls: &[&ExprMethodCall]) -> usize {
    calls
        .iter()
        .filter(|call| {
            !calls.iter().any(|outer| {
                call.span().byte_range() != outer.span().byte_range()
                    && receiver_spine_contains(&outer.receiver, call)
            })
        })
        .count()
}

#[derive(Default)]
struct SelfMethodCallCollector<'a> {
    calls: Vec<&'a ExprMethodCall>,
}

impl<'ast> Visit<'ast> for SelfMethodCallCollector<'ast> {
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if contains_self_field(&call.receiver) {
            self.calls.push(call);
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn contains_self_field(expr: &Expr) -> bool {
    let mut finder = SelfFieldFinder(false);
    finder.visit_expr(expr);
    finder.0
}

struct SelfFieldFinder(bool);

impl<'ast> Visit<'ast> for SelfFieldFinder {
    fn visit_expr_field(&mut self, field: &'ast ExprField) {
        if matches!(&*field.base, Expr::Path(path) if path.path.is_ident("self")) {
            self.0 = true;
        } else {
            visit::visit_expr_field(self, field);
        }
    }
}

fn delegated_call_range(
    expr: &Expr,
    call: &ExprMethodCall,
    method: &syn::ImplItemFn,
) -> Option<Range<usize>> {
    if method.sig.asyncness.is_none() {
        return Some(call.span().byte_range());
    }
    let call_range = call.span().byte_range();
    let mut collector = AwaitCollector {
        call_range: call_range.clone(),
        ranges: Vec::new(),
    };
    collector.visit_expr(expr);
    let [range] = collector.ranges.as_slice() else {
        return None;
    };
    Some(range.clone())
}

struct AwaitCollector {
    call_range: Range<usize>,
    ranges: Vec<Range<usize>>,
}

impl<'ast> Visit<'ast> for AwaitCollector {
    fn visit_expr_await(&mut self, expr: &'ast ExprAwait) {
        if expr.base.span().byte_range() == self.call_range {
            self.ranges.push(expr.span().byte_range());
        }
        visit::visit_expr_await(self, expr);
    }
}

fn receiver_spine_contains(receiver: &Expr, call: &ExprMethodCall) -> bool {
    if receiver.span().byte_range() == call.span().byte_range() {
        return true;
    }
    let next = match receiver {
        Expr::Await(expr) => expr.base.as_ref(),
        Expr::Call(expr) => expr.func.as_ref(),
        Expr::Cast(expr) => expr.expr.as_ref(),
        Expr::Field(expr) => expr.base.as_ref(),
        Expr::Group(expr) => expr.expr.as_ref(),
        Expr::Index(expr) => expr.expr.as_ref(),
        Expr::MethodCall(expr) => expr.receiver.as_ref(),
        Expr::Paren(expr) => expr.expr.as_ref(),
        Expr::Reference(expr) => expr.expr.as_ref(),
        Expr::Try(expr) => expr.expr.as_ref(),
        Expr::Unary(expr) => expr.expr.as_ref(),
        _ => return false,
    };
    receiver_spine_contains(next, call)
}

fn argument_modifier(arg: &Expr, param: &str) -> Option<ParamModifier> {
    match arg {
        Expr::Path(path) if path.path.is_ident(param) => Some(ParamModifier::None),
        Expr::MethodCall(call)
            if call.args.is_empty()
                && call.turbofish.is_none()
                && matches!(&*call.receiver, Expr::Path(path) if path.path.is_ident(param)) =>
        {
            match call.method.to_string().as_str() {
                "into" => Some(ParamModifier::Into),
                "as_ref" => Some(ParamModifier::AsRef),
                _ => None,
            }
        }
        Expr::Field(field)
            if field.member == Member::Unnamed(0.into())
                && matches!(&*field.base, Expr::Path(path) if path.path.is_ident(param)) =>
        {
            Some(ParamModifier::Newtype)
        }
        _ => None,
    }
}

#[cfg(test)]
fn fix_source(source: &str) -> Result<(String, FixOutcome)> {
    fix_source_with_thresholds(source, 2, 2)
}

#[cfg(test)]
fn test_blocking() -> Vec<String> {
    vec![
        "async_trait".to_owned(),
        "uniffi::export".to_owned(),
        "wasm_bindgen".to_owned(),
    ]
}

#[cfg(test)]
fn fix_source_with_thresholds(
    source: &str,
    trait_min_methods: usize,
    inherent_min_methods: usize,
) -> Result<(String, FixOutcome)> {
    let file = syn::parse_file(source)?;
    let mut outcome = FixOutcome::default();
    let mut rewriter = SourceRewriter::new(source);
    let blocking = test_blocking();
    for candidate in candidates(
        source,
        &file,
        trait_min_methods,
        inherent_min_methods,
        &blocking,
    ) {
        if let Some(reason) = candidate.skip {
            outcome.skipped.push(reason);
            continue;
        }
        outcome.changes.push(candidate.target);
        for edit in candidate.edits {
            rewriter.replace(edit.range, edit.text);
        }
    }
    let changed = !rewriter.is_empty();
    let result = rewriter.finish()?;
    syn::parse_file(&result)?;
    outcome.writes = usize::from(changed);
    Ok((result, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(source: &str) -> usize {
        count_with_thresholds(source, 2, 2)
    }

    fn count_with_thresholds(
        source: &str,
        trait_min_methods: usize,
        inherent_min_methods: usize,
    ) -> usize {
        let file = syn::parse_file(source).expect("valid Rust source");
        candidates(
            source,
            &file,
            trait_min_methods,
            inherent_min_methods,
            &test_blocking(),
        )
        .len()
    }

    #[test]
    fn trait_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }"
            ),
            1
        );
    }

    #[test]
    fn inherent_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } fn flush(&self) { self.inner.flush() } }"
            ),
            1
        );
    }

    #[test]
    fn inherent_impl_is_fixed_and_idempotent() -> Result<()> {
        let source = "impl Wrapper {\n    pub fn read(&self, b: Buf) -> Read { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n    fn flush<T>(&self, value: T) -> T { self.inner.flush(value) }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("delegate::delegate!"));
        assert!(fixed.contains("to self.inner"));
        assert!(!fixed.contains("self.inner.read(b)"));
        assert!(!fixed.contains("self.inner.close()"));
        assert!(!fixed.contains("self.inner.flush(value)"));
        let (again, second) = fix_source(&fixed)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn forwarding_subset_is_fixed() -> Result<()> {
        let source = "impl Wrapper {\n    pub fn new() -> Self { Self { inner: Inner } }\n    fn a(&self) { self.inner.a() }\n    fn b(&self, x: u8) { self.inner.b(x) }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("pub fn new() -> Self { Self { inner: Inner } }"));
        assert!(fixed.contains("delegate::delegate!"));
        assert!(fixed.contains("to self.inner"));
        assert!(fixed.contains("fn a (& self);"));
        assert!(fixed.contains("fn b (& self , x : u8);"));
        assert!(!fixed.contains("self.inner.a()"));
        assert!(!fixed.contains("self.inner.b(x)"));
        Ok(())
    }

    #[test]
    fn renamed_call_emits_call_attribute() -> Result<()> {
        let source = "impl Wrapper {\n    fn size(&self) -> usize { self.inner.len() }\n    fn clear(&mut self) { self.inner.clear() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[call(len)]"));
        assert!(fixed.contains("fn size (& self) -> usize;"));
        assert!(fixed.contains("fn clear (& mut self);"));
        Ok(())
    }

    #[test]
    fn into_result_emits_into_and_call_attributes() -> Result<()> {
        let source = "impl Wrapper {\n    fn x(&self, n: u32) -> u64 { self.inner.y(n).into() }\n    fn clear(&mut self) { self.inner.clear() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[into]"));
        assert!(fixed.contains("#[call(y)]"));
        assert!(fixed.contains("fn x (& self , n : u32) -> u64;"));
        Ok(())
    }

    #[test]
    fn unwrap_result_emits_unwrap_and_call_attributes() -> Result<()> {
        let source = "impl Wrapper {\n    fn x(&self) -> u32 { self.inner.y().unwrap() }\n    fn clear(&mut self) { self.inner.clear() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[unwrap]"));
        assert!(fixed.contains("#[call(y)]"));
        assert!(fixed.contains("fn x (& self) -> u32;"));
        Ok(())
    }

    #[test]
    fn arbitrary_result_wrapper_emits_exact_expr_template() -> Result<()> {
        let source =
            "impl Wrapper {\n    fn value(&self, i: usize) -> u8 { *self.0.get(i).unwrap() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.0 {\n            #[expr(*$.unwrap())]\n            #[call(get)]\n            fn value (& self , i : usize) -> u8;\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn expr_templates_preserve_surrounding_source_exactly() -> Result<()> {
        let cases = [
            (
                "fn squared(&self, i: usize) -> Option<u8> { self.0.get(i)?.checked_pow(2) }",
                "to self.0",
                "#[expr($?.checked_pow(2))]\n            #[call(get)]\n            fn squared (& self , i : usize) -> Option < u8 >;",
            ),
            (
                "fn mapped(&self, x: Input) -> Output { self.inner.m(x).map(Foo::from) }",
                "to self.inner",
                "#[expr($.map(Foo::from))]\n            #[call(m)]\n            fn mapped (& self , x : Input) -> Output;",
            ),
            (
                "fn mapped(&self, x: Input) -> Output { self.inner.m(x.into()).map(Foo::from) }",
                "to self.inner",
                "#[expr($.map(Foo::from))]\n            #[call(m)]\n            fn mapped (& self , #[into] x : Input) -> Output;",
            ),
            (
                "async fn squared(&self, i: usize) -> Option<u8> { self.0.get(i).await?.checked_pow(2) }",
                "to self.0",
                "#[expr($?.checked_pow(2))]\n            #[call(get)]\n            async fn squared (& self , i : usize) -> Option < u8 >;",
            ),
        ];
        for (method, target, rendered) in cases {
            let source = format!("impl Wrapper {{\n    {method}\n}}\n");
            let expected = format!(
                "impl Wrapper {{\n    delegate::delegate! {{\n        {target} {{\n            {rendered}\n        }}\n    }}\n}}\n"
            );
            let (fixed, outcome) = fix_source_with_thresholds(&source, 1, 1)?;
            assert_eq!(outcome.writes, 1);
            assert_eq!(fixed, expected);
            let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
            assert_eq!(again, fixed);
            assert_eq!(second.writes, 0);
        }
        Ok(())
    }

    #[test]
    fn try_into_result_emits_dedicated_modifier() -> Result<()> {
        let source = "impl Wrapper {\n    fn value(&self, x: Input) -> Result<Output, Error> { self.inner.read(x).try_into() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner {\n            #[try_into]\n            #[call(read)]\n            fn value (& self , x : Input) -> Result < Output , Error >;\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn multiple_self_calls_and_unsupported_argument_transforms_stay_manual() -> Result<()> {
        let sources = [
            "impl Wrapper {\n    fn value(&self) -> u8 { self.inner.a() + self.inner.b() }\n}\n",
            "impl Wrapper {\n    fn value(&self, x: u8) -> u8 { self.inner.a(x + 1).map(Foo::from) }\n}\n",
            "impl Wrapper {\n    fn value(&self, x: u8) -> u8 { self.inner.a(0).map(Foo::from) }\n}\n",
            "impl Wrapper {\n    fn value(&self, x: u8, y: u8) -> u8 { self.inner.a(y, x).map(Foo::from) }\n}\n",
            "impl Wrapper {\n    fn value(&self) -> u8 { self.inner.a(self.extra).map(Foo::from) }\n}\n",
            "impl Wrapper {\n    fn value(&self) -> u8 { (self.inner.a(), self.inner.b()).0.c() }\n}\n",
            "impl Wrapper {\n    fn value(&self, x: u8) { consume(self.inner.a(x)) }\n}\n",
        ];
        for source in sources {
            let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
            assert_eq!(outcome.writes, 0, "{source}");
            assert_eq!(fixed, source);
        }
        Ok(())
    }

    #[test]
    fn async_into_result_preserves_async_signature() -> Result<()> {
        let source = "impl Wrapper {\n    async fn x(&self) -> u64 { self.inner.y().await.into() }\n    async fn clear(&mut self) { self.inner.clear().await }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[into]"));
        assert!(fixed.contains("#[call(y)]"));
        assert!(fixed.contains("async fn x (& self) -> u64;"));
        assert!(!fixed.contains("self.inner.y().await.into()"));
        Ok(())
    }

    #[test]
    fn async_unwrap_result_emits_unwrap_attribute() -> Result<()> {
        let source = "impl Wrapper {\n    async fn x(&self) -> u32 { self.inner.y().await.unwrap() }\n    async fn clear(&mut self) { self.inner.clear().await }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[unwrap]"));
        assert!(fixed.contains("#[call(y)]"));
        assert!(fixed.contains("async fn x (& self) -> u32;"));
        Ok(())
    }

    #[test]
    fn tuple_field_is_delegated() -> Result<()> {
        let source =
            "impl Wrapper {\n    fn a(&self) { self.0.a() }\n    fn b(&self) { self.0.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("to self.0 {"));
        assert!(fixed.contains("fn a (& self);"));
        assert!(fixed.contains("fn b (& self);"));
        Ok(())
    }

    #[test]
    fn nested_field_access_emits_field_modifier() -> Result<()> {
        let source = "impl Wrapper {\n    fn value(&self) -> u8 { self.0.value }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.0 {\n            #[field]\n            fn value (& self) -> u8;\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn nested_field_variants_emit_exact_modifiers() -> Result<()> {
        let cases = [
            (
                "fn value(&self) -> u8 { self.0.inner_name }",
                "#[field(inner_name)]\n            fn value (& self) -> u8;",
            ),
            (
                "fn value(&self) -> &u8 { &self.0.value }",
                "#[field(&value)]\n            fn value (& self) -> & u8;",
            ),
            (
                "fn value(&mut self) -> &mut u8 { &mut self.0.value }",
                "#[field(&mut value)]\n            fn value (& mut self) -> & mut u8;",
            ),
        ];
        for (method, rendered) in cases {
            let source = format!("impl Wrapper {{\n    {method}\n}}\n");
            let expected = format!(
                "impl Wrapper {{\n    delegate::delegate! {{\n        to self.0 {{\n            {rendered}\n        }}\n    }}\n}}\n"
            );
            let (fixed, outcome) = fix_source_with_thresholds(&source, 1, 1)?;
            assert_eq!(outcome.writes, 1);
            assert_eq!(fixed, expected);
            let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
            assert_eq!(again, fixed);
            assert_eq!(second.writes, 0);
        }
        Ok(())
    }

    #[test]
    fn single_level_field_access_stays_manual() -> Result<()> {
        let source = "impl Wrapper {\n    fn value(&self) -> u8 { self.value }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn field_group_threshold_is_honored() {
        let one = "impl Wrapper { fn a(&self) { self.inner.a() } }";
        let two = "impl Wrapper { fn a(&self) { self.inner.a() } fn b(&self) { self.inner.b() } }";
        assert_eq!(count(one), 0);
        assert_eq!(count(two), 1);
        assert_eq!(count_with_thresholds(two, 2, 3), 0);
    }

    #[test]
    fn shared_complex_receiver_is_delegated() -> Result<()> {
        let source = "impl Wrapper {\n    fn a(&self) { self.inner.lock().a() }\n    fn b(&mut self) { self . inner.lock ().b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner.lock() {\n            fn a (& self);\n            fn b (& mut self);\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source(&fixed)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn complex_receiver_below_threshold_stays_manual() -> Result<()> {
        let source = "impl Wrapper {\n    fn a(&self) { self.inner.lock().a() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn multiple_fields_share_one_delegate_block() -> Result<()> {
        let source = "impl Wrapper {\n    fn a1(&self) { self.a.a1() }\n    fn b1(&self) { self.b.b1() }\n    fn a2(&self) { self.a.a2() }\n    fn b2(&self) { self.b.b2() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.a {").count(), 1);
        assert_eq!(fixed.matches("to self.b {").count(), 1);
        assert!(fixed.find("to self.a {") < fixed.find("to self.b {"));
        Ok(())
    }

    #[test]
    fn existing_same_target_delegate_blocks_are_coalesced() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { to self.playhead { fn a(&self); } }\n    delegate! { to self.playhead { fn b(&self); } }\n    delegate! { to self.playhead { fn c(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.playhead {").count(), 1);
        assert!(fixed.contains("fn a(&self);"));
        assert!(fixed.contains("fn b(&self);"));
        assert!(fixed.contains("fn c(&self);"));
        let (again, second) = fix_source(&fixed)?;
        assert_eq!(second.writes, 0);
        assert_eq!(again, fixed);
        Ok(())
    }

    #[test]
    fn nested_simple_existing_target_is_coalesced() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { to self.0.inner { fn a(&self); } }\n    delegate! { to self.0.inner { fn b(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("to self.0.inner {").count(), 1);
        assert!(fixed.contains("fn a(&self);"));
        assert!(fixed.contains("fn b(&self);"));
        Ok(())
    }

    #[test]
    fn existing_segment_absorbs_manual_forwarder_below_threshold() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { to self.inner { fn a(&self); } }\n    fn b(&self) { self.inner.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.inner {").count(), 1);
        assert!(fixed.contains("fn a(&self);"));
        assert!(fixed.contains("fn b (& self);"));
        assert!(!fixed.contains("self.inner.b()"));
        Ok(())
    }

    #[test]
    fn new_field_below_threshold_does_not_augment_existing_segment() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { to self.inner { fn a(&self); } }\n    fn b(&self) { self.other.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn existing_different_target_blocks_collapse_into_two_segments() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { to self.a { fn a(&self); } }\n    delegate::delegate! { to self.b { fn b(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.a {").count(), 1);
        assert_eq!(fixed.matches("to self.b {").count(), 1);
        assert!(fixed.find("to self.a {") < fixed.find("to self.b {"));
        Ok(())
    }

    #[test]
    fn identical_segment_attributes_allow_same_target_coalescing() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { #[await] to self.inner { #[call(a)] async fn one(&self); } }\n    delegate! { #[await] to self.inner { #[call(b)] async fn two(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("#[await]").count(), 1);
        assert_eq!(fixed.matches("to self.inner {").count(), 1);
        assert!(fixed.contains("#[call(a)] async fn one(&self);"));
        assert!(fixed.contains("#[call(b)] async fn two(&self);"));
        Ok(())
    }

    #[test]
    fn differing_segment_attributes_keep_same_target_segments_separate() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { #[await] to self.inner { async fn one(&self); } }\n    delegate! { #[through(Trait)] to self.inner { fn two(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.inner {").count(), 2);
        assert_eq!(fixed.matches("#[await]").count(), 1);
        assert_eq!(fixed.matches("#[through(Trait)]").count(), 1);
        Ok(())
    }

    #[test]
    fn manual_forwarder_does_not_inherit_existing_segment_attributes() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { #[await] to self.inner { async fn a(&self); } }\n    async fn b(&self) { self.inner.b().await }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("delegate::delegate!").count(), 1);
        assert_eq!(fixed.matches("to self.inner {").count(), 2);
        assert_eq!(fixed.matches("#[await]").count(), 1);
        assert!(fixed.contains("async fn b (& self);"));
        Ok(())
    }

    #[test]
    fn existing_segments_precede_new_groups_in_source_order() -> Result<()> {
        let source = "impl Wrapper {\n    fn a1(&self) { self.a.a1() }\n    fn a2(&self) { self.a.a2() }\n    delegate! { to self.inner { fn inner(&self); } }\n    fn b1(&self) { self.b.b1() }\n    fn b2(&self) { self.b.b2() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        let inner = fixed.find("to self.inner {").expect("existing segment");
        let a = fixed.find("to self.a {").expect("first new group");
        let b = fixed.find("to self.b {").expect("second new group");
        assert!(inner < a && a < b);
        assert!(!fixed.contains("self.a.a1()"));
        assert!(!fixed.contains("self.b.b1()"));
        Ok(())
    }

    #[test]
    fn non_simple_existing_targets_skip_the_impl() -> Result<()> {
        let sources = [
            "impl Wrapper {\n    delegate! { to self.inner.lock() { fn a(&self); } }\n}\n",
            "impl Wrapper {\n    delegate! { to match self { Self::Inner(inner) => inner } { fn a(&self); } }\n}\n",
        ];
        for source in sources {
            let (fixed, outcome) = fix_source(source)?;
            assert_eq!(outcome.writes, 0);
            assert_eq!(outcome.skipped, ["impl has a non-simple delegate! target"]);
            assert_eq!(fixed, source);
        }
        Ok(())
    }

    #[test]
    fn unparseable_existing_block_keeps_manual_forwarders_unchanged() -> Result<()> {
        let source = "impl Wrapper {\n    delegate! { unexpected }\n    fn a(&self) { self.inner.a() }\n    fn b(&self) { self.inner.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(outcome.skipped, ["impl has a non-simple delegate! target"]);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn trait_impl_is_fixed() -> Result<()> {
        let source = "impl Read for Wrapper {\n    fn read(&self, b: Buf) -> Read { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("delegate::delegate!"));
        assert!(!fixed.contains("self.inner.read(b)"));
        Ok(())
    }

    #[test]
    fn associated_const_is_preserved() -> Result<()> {
        let source = "impl Read for Wrapper { const OPEN: bool = true; fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("const OPEN: bool = true;"));
        assert!(fixed.contains("delegate::delegate!"));
        Ok(())
    }

    #[test]
    fn cfg_method_is_delegated_with_its_sibling() -> Result<()> {
        let source = "impl Read for Wrapper {\n    #[cfg(unix)]\n    fn read(&self, b: Buf) { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n    fn flush(&self) { self.inner.flush() }\n}";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[cfg(unix)]\n            fn read (& self , b : Buf);"));
        assert!(fixed.contains("delegate::delegate!"));
        assert!(!fixed.contains("self.inner.read(b)"));
        assert!(!fixed.contains("self.inner.close()"));
        assert!(!fixed.contains("self.inner.flush()"));
        Ok(())
    }

    #[test]
    fn cfg_test_method_stays_manual_while_siblings_are_fixed() -> Result<()> {
        let source = "impl Wrapper {\n    #[cfg(test)]\n    fn test_only(&self) { self.inner.test_only() }\n    fn a(&self) { self.inner.a() }\n    fn b(&self) { self.inner.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[cfg(test)]\n    fn test_only(&self)"));
        assert!(fixed.contains("self.inner.test_only()"));
        assert!(!fixed.contains("self.inner.a()"));
        assert!(!fixed.contains("self.inner.b()"));
        Ok(())
    }

    #[test]
    fn block_doc_method_emits_equivalent_doc_attribute() -> Result<()> {
        let source = "impl Wrapper {\n    /** Doc. */\n    fn read(&self, b: Buf) { self.inner.read(b) }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner {\n            #[doc = r\" Doc. \"]\n            fn read (& self , b : Buf);\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn block_doc_uses_collision_free_raw_string_delimiter() -> Result<()> {
        let source = "impl Wrapper {\n    /** A \"quoted\" value. */\n    fn read(&self) { self.inner.read() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner {\n            #[doc = r#\" A \"quoted\" value. \"#]\n            fn read (& self);\n        }\n    }\n}\n"
        );
        Ok(())
    }

    #[test]
    fn line_doc_comment_is_preserved() -> Result<()> {
        let source = "impl Read for Wrapper {\n    /// Reads bytes.\n    pub(crate) fn read(&self, b: Buf) { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("/// Reads bytes."));
        assert!(fixed.contains("pub (crate) fn read"));
        Ok(())
    }

    #[test]
    fn must_use_attribute_is_preserved_on_delegated_method() -> Result<()> {
        let source = "impl Wrapper {\n    #[must_use]\n    fn len(&self) -> Option<u64> { self.inner.len() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[must_use]\n            fn len (& self) -> Option < u64 >;"));
        assert!(!fixed.contains("self.inner.len()"));
        Ok(())
    }

    #[test]
    fn deprecated_attribute_is_preserved_on_delegated_method() -> Result<()> {
        let source = "impl Wrapper {\n    #[deprecated = \"x\"]\n    fn old(&self) { self.inner.old() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[deprecated = \"x\"]\n            fn old (& self);"));
        assert!(!fixed.contains("self.inner.old()"));
        Ok(())
    }

    #[test]
    fn delegate_dsl_attribute_collision_stays_manual() -> Result<()> {
        let source =
            "impl Wrapper {\n    #[into]\n    fn value(&self) -> u64 { self.inner.value() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn user_attributes_precede_generated_delegate_attributes() -> Result<()> {
        let source = "impl Wrapper {\n    #[must_use]\n    fn value(&self) -> u64 { self.inner.read().into() }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        let must_use = fixed.find("#[must_use]").expect("preserved attribute");
        let into = fixed.find("#[into]").expect("into attribute");
        let call = fixed.find("#[call(read)]").expect("call attribute");
        let signature = fixed.find("fn value (& self) -> u64;").expect("signature");
        assert!(must_use < into && into < call && call < signature);
        Ok(())
    }

    #[test]
    fn attached_comments_are_preserved() -> Result<()> {
        let source = "impl Wrapper {\n    // First forwarder.\n    fn a(&self) { self.inner.a() } // a tail\n    fn manual(&self) { work() }\n    /* Second forwarder. */\n    fn b(&self) { self.inner.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(fixed.matches("// First forwarder.").count(), 1);
        assert_eq!(fixed.matches("// a tail").count(), 1);
        assert_eq!(fixed.matches("/* Second forwarder. */").count(), 1);
        assert!(fixed.contains("fn manual(&self) { work() }"));
        Ok(())
    }

    #[test]
    fn async_impl_is_fixed() -> Result<()> {
        let source = "impl Read for Wrapper { async fn read(&self, b: Buf) { self.inner.read(b).await } async fn close(&self) { self.inner.close().await } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("async fn read"));
        assert!(!fixed.contains("self.inner.read(b).await"));
        Ok(())
    }

    #[test]
    fn into_argument_emits_parameter_modifier() -> Result<()> {
        let source = "impl Wrapper {\n    fn f(&self, x: Input) { self.inner.f(x.into()) }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner {\n            fn f (& self , #[into] x : Input);\n        }\n    }\n}\n"
        );
        let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn remaining_argument_modifiers_are_exact_and_idempotent() -> Result<()> {
        let cases = [
            (
                "fn f(&self, x: Input) { self.inner.f(x.as_ref()) }",
                "fn f (& self , #[as_ref] x : Input);",
            ),
            (
                "fn f(&self, x: Input) { self.inner.f(x.0) }",
                "fn f (& self , #[newtype] x : Input);",
            ),
            (
                "fn f(&self, x: Input, y: Other) { self.inner.f(x, y.into()) }",
                "fn f (& self , x : Input , #[into] y : Other);",
            ),
        ];
        for (method, signature) in cases {
            let source = format!("impl Wrapper {{\n    {method}\n}}\n");
            let expected = format!(
                "impl Wrapper {{\n    delegate::delegate! {{\n        to self.inner {{\n            {signature}\n        }}\n    }}\n}}\n"
            );
            let (fixed, outcome) = fix_source_with_thresholds(&source, 1, 1)?;
            assert_eq!(outcome.writes, 1);
            assert_eq!(fixed, expected);
            let (again, second) = fix_source_with_thresholds(&fixed, 1, 1)?;
            assert_eq!(again, fixed);
            assert_eq!(second.writes, 0);
        }
        Ok(())
    }

    #[test]
    fn supported_attributes_and_transformed_arguments_are_delegated() -> Result<()> {
        let source = "impl Wrapper {\n    #[must_use]\n    fn marked(&self) -> bool { self.inner.marked() }\n    fn transformed(&self, x: Input) { self.inner.transformed(x.into()) }\n    fn a(&self) { self.inner.a() }\n    fn b(&self) { self.inner.b() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("#[must_use]\n            fn marked (& self) -> bool;"));
        assert!(!fixed.contains("self.inner.marked()"));
        assert!(fixed.contains("fn transformed (& self , #[into] x : Input);"));
        assert!(!fixed.contains("self.inner.transformed(x.into())"));
        assert!(!fixed.contains("self.inner.a()"));
        assert!(!fixed.contains("self.inner.b()"));
        Ok(())
    }

    #[test]
    fn extra_statement_is_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { trace(); self.inner.read(b) } fn close(&self) { self.inner.close() } }"
            ),
            0
        );
    }

    #[test]
    fn different_fields_are_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.other.close() } }"
            ),
            0
        );
    }

    #[test]
    fn async_trait_attribute_impl_is_skipped() {
        assert_eq!(
            count(
                "#[async_trait::async_trait] impl Net for Client { async fn get(&self, r: Req) { self.net.get(r).await } async fn head(&self, r: Req) { self.net.head(r).await } }"
            ),
            0
        );
    }

    #[test]
    fn cfg_attr_async_trait_impl_is_skipped() {
        assert_eq!(
            count(
                "#[cfg_attr(not(target_arch = \"wasm32\"), async_trait)] #[cfg_attr(target_arch = \"wasm32\", async_trait(?Send))] impl Net for Client { async fn get(&self, r: Req) { self.net.get(r).await } async fn head(&self, r: Req) { self.net.head(r).await } }"
            ),
            0
        );
    }

    #[test]
    fn uniffi_export_impl_is_skipped() {
        assert_eq!(
            count(
                "#[cfg_attr(feature = \"uniffi\", uniffi::export)] impl AudioPlayer { pub fn a(&self) { self.inner.a() } pub fn b(&self) { self.inner.b() } }"
            ),
            0
        );
    }

    #[test]
    fn blocking_impl_attribute_prevents_existing_block_merge() -> Result<()> {
        let source = "#[uniffi::export]\nimpl Wrapper {\n    delegate! { to self.inner { fn a(&self); } }\n    delegate! { to self.inner { fn b(&self); } }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 0);
        assert_eq!(fixed, source);
        Ok(())
    }

    #[test]
    fn wasm_bindgen_impl_is_skipped() {
        assert_eq!(
            count(
                "#[wasm_bindgen] impl X { fn a(&self) { self.inner.a() } fn b(&self) { self.inner.b() } }"
            ),
            0
        );
    }

    #[test]
    fn non_blocking_impl_is_fixed() -> Result<()> {
        let source =
            "#[cfg(test)] impl X { fn a(&self) { self.inner.a() } fn b(&self) { self.inner.b() } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("delegate::delegate!"));
        assert!(!fixed.contains("self.inner.a()"));
        assert!(!fixed.contains("self.inner.b()"));
        Ok(())
    }

    #[test]
    fn async_trait_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Read for Wrapper { async fn read(&self, b: Buf) { self.inner.read(b).await } async fn close(&self) { self.inner.close().await } }"
            ),
            1
        );
    }

    #[test]
    fn transformed_async_argument_is_delegated() -> Result<()> {
        let source = "impl Wrapper {\n    async fn read(&self, b: Buf) { self.inner.read(b.into()).await }\n}\n";
        let (fixed, outcome) = fix_source_with_thresholds(source, 1, 1)?;
        assert_eq!(outcome.writes, 1);
        assert_eq!(
            fixed,
            "impl Wrapper {\n    delegate::delegate! {\n        to self.inner {\n            async fn read (& self , #[into] b : Buf);\n        }\n    }\n}\n"
        );
        Ok(())
    }

    #[test]
    fn associated_type_is_preserved() -> Result<()> {
        let source = "impl Read for Wrapper {\n    type Context = Context;\n    fn read(&self, b: Buf) { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n}\n";
        assert_eq!(count(source), 1);
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("type Context = Context;"));
        assert!(fixed.contains("delegate::delegate!"));
        Ok(())
    }
}
