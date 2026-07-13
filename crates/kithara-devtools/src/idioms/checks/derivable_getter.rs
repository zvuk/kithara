use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use quote::ToTokens;
use syn::{
    Attribute, Expr, Fields, FnArg, ImplItem, ImplItemFn, ItemImpl, ItemStruct, Member,
    PathArguments, ReturnType, Stmt, Type, Visibility, parse_quote, spanned::Spanned,
};

use super::{
    Check, Context,
    derivable_support::{
        cfg_tokens, field_declaration_start, impl_range, indent_attribute, line_start, merge_derive,
    },
};
use crate::common::{
    exclude::{attrs_have_cfg_test, collect_cfg_test_ranges},
    fix::{FixOutcome, SourceRewriter, expand_blocks},
    parse::{collect_scopes, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "derivable_getter";

pub(crate) struct DerivableGetter;

impl Check for DerivableGetter {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        if !ctx.config.thresholds.derivable_getter.enabled {
            return Ok(Vec::new());
        }
        let redundant =
            crate::arch::redundant_accessor_keys(ctx.metadata, ctx.workspace_root, ctx.scope)?;
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
            let analysis = analyze(&src, &file, &rel, &redundant);
            for finding in analysis.findings {
                let detail = finding.skip.map_or_else(
                    || {
                        format!(
                            "{} accessor {}::{} for field `{}` can be generated with fieldwork",
                            finding.kind.label(),
                            finding.type_name,
                            finding.method,
                            finding.field
                        )
                    },
                    |reason| {
                        format!(
                            "{} accessor {}::{} for field `{}` is derivable but autofix will skip: {reason}",
                            finding.kind.label(),
                            finding.type_name,
                            finding.method,
                            finding.field
                        )
                    },
                );
                violations.push(Violation::warn(
                    ID,
                    format!("{rel}:{}:0", finding.line),
                    detail,
                ));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        if !ctx.config.thresholds.derivable_getter.enabled {
            return Ok(FixOutcome::default());
        }
        let redundant =
            crate::arch::redundant_accessor_keys(ctx.metadata, ctx.workspace_root, ctx.scope)?;
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
            let analysis = analyze(&src, &file, &rel, &redundant);
            for finding in &analysis.findings {
                if let Some(reason) = &finding.skip {
                    outcome.skipped.push(format!(
                        "{rel}:{}: {}::{}: {reason}",
                        finding.line, finding.type_name, finding.method
                    ));
                } else {
                    outcome.changes.push(format!(
                        "{rel}:{}: derived {}::{} from field `{}`",
                        finding.line, finding.type_name, finding.method, finding.field
                    ));
                }
            }
            if analysis.edits.is_empty() {
                continue;
            }
            let mut rewriter = SourceRewriter::new(&src);
            for edit in analysis.edits {
                rewriter.replace(edit.range, edit.text);
            }
            let rewritten = rewriter.finish().context("apply derivable getter edits")?;
            fs::write(&path, rewritten).with_context(|| format!("write {}", path.display()))?;
            outcome.writes += 1;
            if let Some(manifest) = crate_manifest(ctx.workspace_root, &path) {
                manifests.insert(manifest);
            }
        }

        for manifest in manifests {
            let source = fs::read_to_string(&manifest)
                .with_context(|| format!("read {}", manifest.display()))?;
            let Some(rewritten) = crate::manifest::add_workspace_dependency(&source, "fieldwork")?
            else {
                continue;
            };
            fs::write(&manifest, rewritten)
                .with_context(|| format!("write {}", manifest.display()))?;
            outcome.writes += 1;
            outcome.changes.push(format!(
                "{}: added workspace fieldwork dependency",
                relative_to(ctx.workspace_root, &manifest).display()
            ));
        }
        Ok(outcome)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AccessorKind {
    Get,
    GetMut,
}

impl AccessorKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Get => "read",
            Self::GetMut => "mutable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BodyKind {
    Borrow,
    Move,
    Clone,
    BorrowMut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Adjustment {
    Default,
    CopyFalse,
    CopyTrue,
    DerefFalse,
}

#[derive(Clone, Copy)]
struct MethodPlan {
    adjustment: Adjustment,
}

#[derive(Clone)]
struct RawAccessor<'a> {
    block: Option<Range<usize>>,
    block_error: Option<String>,
    body: BodyKind,
    field: String,
    impl_block: &'a ItemImpl,
    kind: AccessorKind,
    method: &'a ImplItemFn,
    return_ty: &'a Type,
    type_name: String,
}

struct Converted<'a> {
    plan: MethodPlan,
    raw: RawAccessor<'a>,
}

struct Finding {
    field: String,
    kind: AccessorKind,
    line: usize,
    method: String,
    skip: Option<String>,
    type_name: String,
}

struct Edit {
    range: Range<usize>,
    text: String,
}

struct FileAnalysis {
    edits: Vec<Edit>,
    findings: Vec<Finding>,
}

enum FieldworkMode {
    Concise { get_visibility: String },
    OptIn,
}

struct Completion<'a, 'out> {
    edits: &'out mut Vec<Edit>,
    findings: &'out mut Vec<Finding>,
    insertions: &'out mut BTreeMap<usize, Vec<String>>,
    mod_prefix: &'a str,
    redundant: &'a BTreeSet<String>,
    rel: &'a str,
    src: &'a str,
}

#[derive(Default)]
struct FieldMethods<'a> {
    field: Option<&'a syn::Field>,
    get: Option<&'a Converted<'a>>,
    get_mut: Option<&'a Converted<'a>>,
}

fn analyze(src: &str, file: &syn::File, rel: &str, redundant: &BTreeSet<String>) -> FileAnalysis {
    let mut findings = Vec::new();
    let mut insertions = BTreeMap::<usize, Vec<String>>::new();
    let mut edits = Vec::new();
    let mut cfg_test_ranges = Vec::new();
    collect_cfg_test_ranges(&file.items, &mut cfg_test_ranges);

    for scope in collect_scopes(file) {
        let mut by_type = BTreeMap::<String, Vec<RawAccessor<'_>>>::new();
        for &impl_block in &scope.impls {
            if impl_block.trait_.is_some()
                || attrs_have_cfg_test(&impl_block.attrs)
                || cfg_test_ranges
                    .iter()
                    .any(|range| range.contains(&impl_block.span().start().line))
            {
                continue;
            }
            let Some(type_name) = self_ty_name(&impl_block.self_ty) else {
                continue;
            };
            let blocks = method_blocks(src, impl_block);
            for (index, item) in impl_block.items.iter().enumerate() {
                let ImplItem::Fn(method) = item else {
                    continue;
                };
                if attrs_have_cfg_test(&method.attrs)
                    || cfg_test_ranges
                        .iter()
                        .any(|range| range.contains(&method.span().start().line))
                {
                    continue;
                }
                let Some((kind, body, field, return_ty)) = detect(method) else {
                    continue;
                };
                let (block, block_error) = match &blocks {
                    Ok(blocks) => (blocks.get(index).cloned(), None),
                    Err(reason) => (None, Some(reason.clone())),
                };
                by_type
                    .entry(type_name.clone())
                    .or_default()
                    .push(RawAccessor {
                        block,
                        block_error,
                        body,
                        field,
                        impl_block,
                        kind,
                        method,
                        return_ty,
                        type_name: type_name.clone(),
                    });
            }
        }

        let mod_prefix = if scope.path.is_empty() {
            String::new()
        } else {
            format!("{}::", scope.path.join("::"))
        };
        for (type_name, raw) in by_type {
            let local = scope
                .structs
                .iter()
                .copied()
                .find(|item| item.ident == type_name);
            if local.is_some_and(|item| attrs_have_cfg_test(&item.attrs)) {
                continue;
            }
            let mut completion = Completion {
                edits: &mut edits,
                findings: &mut findings,
                insertions: &mut insertions,
                mod_prefix: &mod_prefix,
                redundant,
                rel,
                src,
            };
            complete_type(&mut completion, local, raw);
        }
    }

    for (at, values) in insertions {
        edits.push(Edit {
            range: at..at,
            text: values.concat(),
        });
    }
    FileAnalysis { edits, findings }
}

fn complete_type<'a>(
    completion: &mut Completion<'_, '_>,
    strukt: Option<&'a ItemStruct>,
    raw: Vec<RawAccessor<'a>>,
) {
    let mut claimed = BTreeSet::new();
    let mut converted = Vec::new();
    for accessor in raw {
        let result = conversion_reason(
            completion.src,
            completion.rel,
            completion.mod_prefix,
            strukt,
            &accessor,
            completion.redundant,
            &claimed,
        );
        let (plan, skip) = match result {
            Ok(plan) => (Some(plan), None),
            Err(reason) => (None, Some(reason.to_owned())),
        };
        completion.findings.push(Finding {
            field: accessor.field.clone(),
            kind: accessor.kind,
            line: accessor.method.sig.fn_token.span.start().line,
            method: accessor.method.sig.ident.to_string(),
            skip,
            type_name: accessor.type_name.clone(),
        });
        if let Some(plan) = plan {
            claimed.insert((accessor.field.clone(), accessor.kind));
            converted.push(Converted {
                plan,
                raw: accessor,
            });
        }
    }
    let Some(strukt) = strukt else { return };
    if converted.is_empty() {
        return;
    }

    let mode = fieldwork_mode(strukt, &converted);

    add_struct_attributes(
        completion.src,
        strukt,
        &converted,
        &mode,
        completion.insertions,
        completion.edits,
    );
    let mut fields = BTreeMap::<String, FieldMethods<'_>>::new();
    for conversion in &converted {
        let entry = fields.entry(conversion.raw.field.clone()).or_default();
        entry.field = struct_field(strukt, &conversion.raw.field);
        match conversion.raw.kind {
            AccessorKind::Get => entry.get = Some(conversion),
            AccessorKind::GetMut => entry.get_mut = Some(conversion),
        }
    }
    for methods in fields.values() {
        let Some(field) = methods.field else { continue };
        add_field_attributes(completion.src, field, methods, &mode, completion.insertions);
    }

    for &impl_block in collect_impls(&converted).values() {
        let matching: Vec<_> = converted
            .iter()
            .filter(|conversion| std::ptr::eq(conversion.raw.impl_block, impl_block))
            .collect();
        if matching.len() == impl_block.items.len() {
            completion.edits.push(Edit {
                range: impl_range(completion.src, impl_block),
                text: String::new(),
            });
        } else {
            for conversion in matching {
                if let Some(range) = &conversion.raw.block {
                    completion.edits.push(Edit {
                        range: deletion_range(completion.src, range.clone()),
                        text: String::new(),
                    });
                }
            }
        }
    }
}

fn deletion_range(src: &str, range: Range<usize>) -> Range<usize> {
    let line = line_start(src, range.start);
    let start = if src[line..range.start]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t'))
    {
        line
    } else {
        range.start
    };
    let next = src[range.end..]
        .find(|character: char| !character.is_whitespace())
        .map_or(src.len(), |offset| range.end + offset);
    let next_line = line_start(src, next);
    let end = if src[range.end..next].contains('\n') && next_line >= range.end {
        next_line
    } else {
        range.end
    };
    start..end
}

fn conversion_reason<'a>(
    src: &str,
    rel: &str,
    mod_prefix: &str,
    strukt: Option<&ItemStruct>,
    accessor: &RawAccessor<'a>,
    redundant: &BTreeSet<String>,
    claimed: &BTreeSet<(String, AccessorKind)>,
) -> Result<MethodPlan, &'static str> {
    let Some(strukt) = strukt else {
        return Err("struct declared in another file");
    };
    if strukt
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("fieldwork"))
    {
        return Err("struct already has fieldwork configuration");
    }
    if cfg_tokens(&strukt.attrs) != cfg_tokens(&accessor.impl_block.attrs) {
        return Err("#[cfg] on impl differs from struct");
    }
    if accessor
        .impl_block
        .attrs
        .iter()
        .any(|attr| !attr.path().is_ident("cfg"))
    {
        return Err("impl has a non-cfg attribute");
    }
    if accessor
        .method
        .attrs
        .iter()
        .any(|attr| !attr.path().is_ident("doc") && !attr.path().is_ident("must_use"))
    {
        return Err("method has a non-droppable attribute");
    }
    if method_body_has_comment(src, accessor.method) {
        return Err("method body contains a comment");
    }
    if accessor.block_error.is_some() {
        return Err("comment-preserving block expansion failed");
    }
    if claimed.contains(&(accessor.field.clone(), accessor.kind)) {
        return Err("field already has a convertible accessor of this kind");
    }
    let key = format!(
        "{rel}::{mod_prefix}{}::{}",
        accessor.type_name, accessor.method.sig.ident
    );
    if redundant.contains(&key) {
        return Err("redundant_accessors owns this method for deletion");
    }
    let Some(field) = struct_field(strukt, &accessor.field) else {
        return Err("target is not a field on the same-file struct");
    };
    if field.attrs.iter().any(|attr| attr.path().is_ident("field")) {
        return Err("field already has fieldwork configuration");
    }
    if field.attrs.iter().any(|attr| attr.path().is_ident("cfg")) {
        return Err("field has conditional compilation");
    }
    if !field_is_line_aligned(src, field) {
        return Err("field declaration is not line-aligned");
    }
    plan_method(&field.ty, accessor.return_ty, accessor.kind, accessor.body)
}

fn field_is_line_aligned(src: &str, field: &syn::Field) -> bool {
    let byte = match &field.vis {
        Visibility::Inherited => field.ident.as_ref().map_or_else(
            || field.ty.span().byte_range().start,
            |ident| ident.span().byte_range().start,
        ),
        visibility => visibility.span().byte_range().start,
    };
    src[line_start(src, byte)..byte]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t'))
}

fn plan_method(
    field_ty: &Type,
    return_ty: &Type,
    kind: AccessorKind,
    body: BodyKind,
) -> Result<MethodPlan, &'static str> {
    if body == BodyKind::Clone {
        return Err("clone getter is not generated by fieldwork");
    }
    if kind == AccessorKind::Get
        && option_inner(field_ty).is_some()
        && is_reference_to(return_ty, field_ty, false)
    {
        return Err("direct &Option<T> access is outside this autofix");
    }

    let default = match kind {
        AccessorKind::Get => Some(get_return_type(field_ty, true, true)),
        AccessorKind::GetMut => get_mut_return_type(field_ty, true),
    };
    if default.as_ref().is_some_and(|ty| ty == return_ty) {
        return Ok(MethodPlan {
            adjustment: Adjustment::Default,
        });
    }
    if kind == AccessorKind::Get && body == BodyKind::Move && field_ty == return_ty {
        return Ok(MethodPlan {
            adjustment: Adjustment::CopyTrue,
        });
    }
    if kind == AccessorKind::Get && is_copy_type(field_ty) {
        let copy_false = get_return_type(field_ty, false, true);
        if &copy_false == return_ty {
            return Ok(MethodPlan {
                adjustment: Adjustment::CopyFalse,
            });
        }
    }
    if auto_deref_target(field_ty, kind).is_some()
        || option_inner(field_ty).is_some_and(|inner| auto_deref_target(inner, kind).is_some())
    {
        let deref_false = match kind {
            AccessorKind::Get => Some(get_return_type(field_ty, true, false)),
            AccessorKind::GetMut => get_mut_return_type(field_ty, false),
        };
        if deref_false.as_ref().is_some_and(|ty| ty == return_ty) {
            return Ok(MethodPlan {
                adjustment: Adjustment::DerefFalse,
            });
        }
    }
    Err("fieldwork cannot reproduce the exact return type")
}

fn detect(method: &ImplItemFn) -> Option<(AccessorKind, BodyKind, String, &Type)> {
    let signature = &method.sig;
    if signature.constness.is_some()
        || signature.asyncness.is_some()
        || signature.unsafety.is_some()
        || signature.abi.is_some()
        || signature.variadic.is_some()
        || !signature.generics.params.is_empty()
        || signature.generics.where_clause.is_some()
        || signature.inputs.len() != 1
    {
        return None;
    }
    let FnArg::Receiver(receiver) = signature.inputs.first()? else {
        return None;
    };
    if receiver.colon_token.is_some()
        || !receiver
            .reference
            .as_ref()
            .is_some_and(|(_, lifetime)| lifetime.is_none())
    {
        return None;
    }
    let ReturnType::Type(_, return_ty) = &signature.output else {
        return None;
    };
    let [Stmt::Expr(body, None)] = method.block.stmts.as_slice() else {
        return None;
    };

    if receiver.mutability.is_some() {
        let Expr::Reference(reference) = body else {
            return None;
        };
        reference.mutability?;
        let field = direct_self_field(&reference.expr)?;
        return Some((AccessorKind::GetMut, BodyKind::BorrowMut, field, return_ty));
    }

    match body {
        Expr::Reference(reference) if reference.mutability.is_none() => Some((
            AccessorKind::Get,
            BodyKind::Borrow,
            direct_self_field(&reference.expr)?,
            return_ty,
        )),
        Expr::Field(_) => Some((
            AccessorKind::Get,
            BodyKind::Move,
            direct_self_field(body)?,
            return_ty,
        )),
        Expr::MethodCall(call)
            if call.method == "clone" && call.args.is_empty() && call.turbofish.is_none() =>
        {
            Some((
                AccessorKind::Get,
                BodyKind::Clone,
                direct_self_field(&call.receiver)?,
                return_ty,
            ))
        }
        _ => None,
    }
}

fn direct_self_field(expr: &Expr) -> Option<String> {
    let Expr::Field(field) = expr else {
        return None;
    };
    if !matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self")) {
        return None;
    }
    match &field.member {
        Member::Named(ident) => Some(ident.to_string()),
        Member::Unnamed(index) => Some(index.index.to_string()),
    }
}

fn method_blocks(src: &str, impl_block: &ItemImpl) -> Result<Vec<Range<usize>>, String> {
    let scope = impl_block.brace_token.span.open().byte_range().end
        ..impl_block.brace_token.span.close().byte_range().start;
    let spans: Vec<_> = impl_block
        .items
        .iter()
        .map(|item| item.span().byte_range())
        .collect();
    expand_blocks(src, scope, &spans)
        .map(|blocks| blocks.into_iter().map(|block| block.bytes).collect())
        .map_err(|error| format!("{error:?}"))
}

fn method_body_has_comment(src: &str, method: &ImplItemFn) -> bool {
    let range = method.block.span().byte_range();
    src.get(range)
        .is_some_and(|body| body.contains("//") || body.contains("/*"))
}

fn add_struct_attributes(
    src: &str,
    strukt: &ItemStruct,
    converted: &[Converted<'_>],
    mode: &FieldworkMode,
    insertions: &mut BTreeMap<usize, Vec<String>>,
    edits: &mut Vec<Edit>,
) {
    let start = struct_declaration_start(src, strukt);
    let derive = strukt
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("derive"));
    if let Some(attr) = derive {
        let range = attr.span().byte_range();
        if let Some(existing) = src.get(range.clone())
            && !derive_has_fieldwork(attr)
        {
            edits.push(Edit {
                range,
                text: merge_derive(existing, "fieldwork::Fieldwork"),
            });
        }
    } else {
        insertions.entry(start).or_default().push(indent_attribute(
            src,
            start,
            "#[derive(fieldwork::Fieldwork)]\n",
        ));
    }
    let has_mut = converted
        .iter()
        .any(|conversion| conversion.raw.kind == AccessorKind::GetMut);
    let attribute = match mode {
        FieldworkMode::Concise { get_visibility } if get_visibility == "pub" => {
            "#[fieldwork(get)]\n".to_owned()
        }
        FieldworkMode::Concise { get_visibility } => {
            format!("#[fieldwork(get, vis = \"{get_visibility}\")]\n")
        }
        FieldworkMode::OptIn if has_mut => "#[fieldwork(opt_in, get, get_mut)]\n".to_owned(),
        FieldworkMode::OptIn => "#[fieldwork(opt_in, get)]\n".to_owned(),
    };
    insertions
        .entry(start)
        .or_default()
        .push(indent_attribute(src, start, &attribute));
}

fn fieldwork_mode(strukt: &ItemStruct, converted: &[Converted<'_>]) -> FieldworkMode {
    if !has_full_get_coverage(strukt, converted) {
        return FieldworkMode::OptIn;
    }
    let mut visibilities = converted
        .iter()
        .filter(|conversion| conversion.raw.kind == AccessorKind::Get)
        .map(|conversion| visibility_text(&conversion.raw.method.vis));
    let first = visibilities.next().unwrap_or_else(|| "pub".to_owned());
    let get_visibility = if visibilities.all(|visibility| visibility == first) {
        first
    } else {
        "pub".to_owned()
    };
    FieldworkMode::Concise { get_visibility }
}

fn has_full_get_coverage(strukt: &ItemStruct, converted: &[Converted<'_>]) -> bool {
    let fields: BTreeSet<_> = converted
        .iter()
        .filter(|conversion| conversion.raw.kind == AccessorKind::Get)
        .map(|conversion| conversion.raw.field.as_str())
        .collect();
    match &strukt.fields {
        Fields::Named(named) => {
            !named.named.is_empty()
                && named.named.iter().all(|field| {
                    field
                        .ident
                        .as_ref()
                        .is_some_and(|ident| fields.contains(ident.to_string().as_str()))
                })
        }
        Fields::Unnamed(unnamed) => {
            !unnamed.unnamed.is_empty()
                && unnamed
                    .unnamed
                    .iter()
                    .enumerate()
                    .all(|(index, _)| fields.contains(index.to_string().as_str()))
        }
        Fields::Unit => false,
    }
}

fn derive_has_fieldwork(attr: &Attribute) -> bool {
    attr.meta
        .to_token_stream()
        .to_string()
        .replace(' ', "")
        .contains("fieldwork::Fieldwork")
}

fn struct_declaration_start(src: &str, strukt: &ItemStruct) -> usize {
    let byte = match &strukt.vis {
        Visibility::Inherited => strukt.struct_token.span.byte_range().start,
        visibility => visibility.span().byte_range().start,
    };
    line_start(src, byte)
}

fn add_field_attributes(
    src: &str,
    field: &syn::Field,
    methods: &FieldMethods<'_>,
    mode: &FieldworkMode,
    insertions: &mut BTreeMap<usize, Vec<String>>,
) {
    let start = field_declaration_start(src, field);
    let indent = leading_indent(&src[start..]);
    let mut text = String::new();
    let mut comments = BTreeSet::new();
    for conversion in [methods.get, methods.get_mut].into_iter().flatten() {
        for comment in method_comments(src, &conversion.raw, &indent) {
            comments.insert(comment);
        }
    }
    for comment in comments {
        text.push_str(&comment);
    }
    let config = match mode {
        FieldworkMode::Concise { get_visibility } => concise_field_config(methods, get_visibility),
        FieldworkMode::OptIn => Some(field_config(methods)),
    };
    if let Some(config) = config {
        text.push_str(&indent);
        text.push_str("#[field(");
        text.push_str(&config);
        text.push_str(")]\n");
    }
    if text.is_empty() {
        return;
    }
    insertions.entry(start).or_default().push(text);
}

fn concise_field_config(methods: &FieldMethods<'_>, get_visibility: &str) -> Option<String> {
    let field_name = methods
        .field
        .and_then(|field| field.ident.as_ref())
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut parts = Vec::new();
    if let Some(get) = methods.get
        && let Some(config) =
            method_override_config(AccessorKind::Get, get, &field_name, get_visibility, false)
    {
        parts.push(config);
    }
    if let Some(get_mut) = methods.get_mut
        && let Some(config) = method_override_config(
            AccessorKind::GetMut,
            get_mut,
            &field_name,
            get_visibility,
            true,
        )
    {
        parts.push(config);
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn method_override_config(
    kind: AccessorKind,
    conversion: &Converted<'_>,
    field: &str,
    default_visibility: &str,
    required: bool,
) -> Option<String> {
    let method = match kind {
        AccessorKind::Get => "get",
        AccessorKind::GetMut => "get_mut",
    };
    let default_name = match kind {
        AccessorKind::Get => field.to_owned(),
        AccessorKind::GetMut => format!("{field}_mut"),
    };
    let actual_name = conversion.raw.method.sig.ident.to_string();
    let visibility = visibility_text(&conversion.raw.method.vis);
    let mut options = Vec::new();
    if actual_name != default_name {
        options.push(format!("name = {actual_name}"));
    }
    if visibility != default_visibility {
        options.push(format!("vis = \"{visibility}\""));
    }
    match conversion.plan.adjustment {
        Adjustment::Default => {}
        Adjustment::CopyFalse => options.push("copy = false".to_owned()),
        Adjustment::CopyTrue => options.push("copy".to_owned()),
        Adjustment::DerefFalse => options.push("deref = false".to_owned()),
    }
    if options.is_empty() {
        return required.then(|| method.to_owned());
    }
    if options.len() == 1 && actual_name != default_name {
        Some(format!("{method} = {actual_name}"))
    } else {
        Some(format!("{method}({})", options.join(", ")))
    }
}

fn field_config(methods: &FieldMethods<'_>) -> String {
    let get_vis = methods
        .get
        .map(|value| visibility_text(&value.raw.method.vis));
    let get_mut_vis = methods
        .get_mut
        .map(|value| visibility_text(&value.raw.method.vis));
    let common_vis = match (&get_vis, &get_mut_vis) {
        (Some(left), Some(right)) if left == right => Some(left.clone()),
        (Some(value), None) | (None, Some(value)) => Some(value.clone()),
        _ => None,
    };
    let common_deref = [methods.get, methods.get_mut]
        .into_iter()
        .flatten()
        .all(|value| value.plan.adjustment == Adjustment::DerefFalse);
    let common_copy = methods.get.and_then(|value| match value.plan.adjustment {
        Adjustment::CopyFalse => Some(false),
        Adjustment::CopyTrue => Some(true),
        _ => None,
    });
    let field_name = methods
        .field
        .and_then(|field| field.ident.as_ref())
        .map(ToString::to_string)
        .unwrap_or_default();

    let mut parts = Vec::new();
    if let Some(get) = methods.get {
        parts.push(method_config(
            AccessorKind::Get,
            get,
            &field_name,
            common_vis.as_deref(),
            common_copy,
            common_deref,
        ));
    }
    if let Some(get_mut) = methods.get_mut {
        parts.push(method_config(
            AccessorKind::GetMut,
            get_mut,
            &field_name,
            common_vis.as_deref(),
            common_copy,
            common_deref,
        ));
    }
    if let Some(visibility) = common_vis
        && visibility != "pub"
    {
        parts.push(format!("vis = \"{visibility}\""));
    }
    if let Some(copy) = common_copy {
        parts.push(if copy { "copy" } else { "copy = false" }.to_owned());
    }
    if common_deref {
        parts.push("deref = false".to_owned());
    }
    parts.join(", ")
}

fn method_config(
    kind: AccessorKind,
    conversion: &Converted<'_>,
    field: &str,
    common_vis: Option<&str>,
    common_copy: Option<bool>,
    common_deref: bool,
) -> String {
    let method = match kind {
        AccessorKind::Get => "get",
        AccessorKind::GetMut => "get_mut",
    };
    let default_name = match kind {
        AccessorKind::Get => field.to_owned(),
        AccessorKind::GetMut => format!("{field}_mut"),
    };
    let actual_name = conversion.raw.method.sig.ident.to_string();
    let visibility = visibility_text(&conversion.raw.method.vis);
    let mut options = Vec::new();
    if actual_name != default_name {
        options.push(format!("name = {actual_name}"));
    }
    if common_vis != Some(visibility.as_str()) && visibility != "pub" {
        options.push(format!("vis = \"{visibility}\""));
    }
    match conversion.plan.adjustment {
        Adjustment::CopyFalse if common_copy != Some(false) => {
            options.push("copy = false".to_owned());
        }
        Adjustment::CopyTrue if common_copy != Some(true) => options.push("copy".to_owned()),
        Adjustment::DerefFalse if !common_deref => options.push("deref = false".to_owned()),
        _ => {}
    }
    if options.is_empty() {
        method.to_owned()
    } else if options.len() == 1 && actual_name != default_name {
        format!("{method} = {actual_name}")
    } else {
        format!("{method}({})", options.join(", "))
    }
}

fn visibility_text(visibility: &Visibility) -> String {
    match visibility {
        Visibility::Inherited => String::new(),
        _ => visibility
            .to_token_stream()
            .to_string()
            .replace("pub (", "pub(")
            .replace(" :: ", "::")
            .replace(" ::", "::")
            .replace(":: ", "::"),
    }
}

fn method_comments(src: &str, raw: &RawAccessor<'_>, indent: &str) -> Vec<String> {
    let mut comments = Vec::new();
    if let Some(block) = &raw.block {
        let method = raw.method.span().byte_range();
        for range in [block.start..method.start, method.end..block.end] {
            if let Some(fragment) = src.get(range)
                && (fragment.contains("//") || fragment.contains("/*"))
            {
                comments.push(reindent(fragment, indent));
            }
        }
    }
    for attr in &raw.method.attrs {
        if attr.path().is_ident("doc")
            && let Some(fragment) = src.get(attr.span().byte_range())
        {
            comments.push(reindent(fragment, indent));
        }
    }
    comments
}

fn reindent(fragment: &str, indent: &str) -> String {
    let lines: Vec<_> = fragment.lines().collect();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |index| index + 1);
    let mut output = String::new();
    for line in &lines[start..end] {
        output.push_str(indent);
        output.push_str(line.trim_start());
        output.push('\n');
    }
    output
}

fn leading_indent(src: &str) -> String {
    src.chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect()
}

fn collect_impls<'a>(converted: &'a [Converted<'a>]) -> BTreeMap<usize, &'a ItemImpl> {
    converted
        .iter()
        .map(|conversion| {
            (
                conversion.raw.impl_block.span().byte_range().start,
                conversion.raw.impl_block,
            )
        })
        .collect()
}

fn struct_field<'a>(strukt: &'a ItemStruct, name: &str) -> Option<&'a syn::Field> {
    match &strukt.fields {
        Fields::Named(fields) => fields
            .named
            .iter()
            .find(|field| field.ident.as_ref().is_some_and(|ident| ident == name)),
        Fields::Unnamed(fields) => name
            .parse::<usize>()
            .ok()
            .and_then(|index| fields.unnamed.iter().nth(index)),
        Fields::Unit => None,
    }
}

fn is_copy_type(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path.path.segments.last().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "bool"
                    | "char"
                    | "f32"
                    | "f64"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
            )
        }),
        Type::Reference(reference) => reference.mutability.is_none(),
        _ => false,
    }
}

fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn get_return_type(field_ty: &Type, copy: bool, deref: bool) -> Type {
    if copy && (is_copy_type(field_ty) || option_inner(field_ty).is_some_and(is_copy_type)) {
        return field_ty.clone();
    }
    if let Some(inner) = option_inner(field_ty) {
        if matches!(inner, Type::Reference(_)) {
            return parse_quote!(&#field_ty);
        }
        let target = if deref {
            auto_deref_target(inner, AccessorKind::Get).unwrap_or_else(|| inner.clone())
        } else {
            inner.clone()
        };
        return parse_quote!(Option<&#target>);
    }
    let target = if deref {
        auto_deref_target(field_ty, AccessorKind::Get).unwrap_or_else(|| field_ty.clone())
    } else {
        field_ty.clone()
    };
    parse_quote!(&#target)
}

fn get_mut_return_type(field_ty: &Type, deref: bool) -> Option<Type> {
    if let Some(inner) = option_inner(field_ty) {
        if matches!(inner, Type::Reference(_)) {
            return None;
        }
        let target = if deref {
            auto_deref_target(inner, AccessorKind::GetMut).unwrap_or_else(|| inner.clone())
        } else {
            inner.clone()
        };
        return Some(parse_quote!(Option<&mut #target>));
    }
    let target = if deref {
        auto_deref_target(field_ty, AccessorKind::GetMut).unwrap_or_else(|| field_ty.clone())
    } else {
        field_ty.clone()
    };
    Some(parse_quote!(&mut #target))
}

fn auto_deref_target(ty: &Type, kind: AccessorKind) -> Option<Type> {
    let mut current = ty.clone();
    let mut changed = false;
    for _ in 0..8 {
        let Some(next) = deref_once(&current, kind) else {
            break;
        };
        current = next;
        changed = true;
    }
    changed.then_some(current)
}

fn deref_once(ty: &Type, kind: AccessorKind) -> Option<Type> {
    match ty {
        Type::Reference(reference) if reference.mutability.is_some() => {
            return Some(reference.elem.as_ref().clone());
        }
        Type::Array(array) => {
            let elem = &array.elem;
            return Some(parse_quote!([#elem]));
        }
        Type::Path(_) => {}
        _ => return None,
    }
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    match segment.ident.to_string().as_str() {
        "String" => Some(parse_quote!(str)),
        "PathBuf" => Some(parse_quote!(Path)),
        "OsString" => Some(parse_quote!(OsStr)),
        "Vec" => first_type_argument(segment).map(|inner| parse_quote!([#inner])),
        "Box" => first_type_argument(segment).cloned(),
        "Arc" | "Rc" if kind == AccessorKind::Get => first_type_argument(segment).cloned(),
        "Cow" if kind == AccessorKind::Get => segment.arguments_type_at(0).cloned(),
        _ => None,
    }
}

trait PathArgumentsExt {
    fn arguments_type_at(&self, index: usize) -> Option<&Type>;
}

impl PathArgumentsExt for syn::PathSegment {
    fn arguments_type_at(&self, index: usize) -> Option<&Type> {
        let PathArguments::AngleBracketed(arguments) = &self.arguments else {
            return None;
        };
        arguments
            .args
            .iter()
            .filter_map(|argument| match argument {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .nth(index)
    }
}

fn first_type_argument(segment: &syn::PathSegment) -> Option<&Type> {
    segment.arguments_type_at(0)
}

fn is_reference_to(return_ty: &Type, target: &Type, mutable: bool) -> bool {
    let Type::Reference(reference) = return_ty else {
        return false;
    };
    reference.mutability.is_some() == mutable && reference.elem.as_ref() == target
}

fn crate_manifest(workspace_root: &Path, source: &Path) -> Option<PathBuf> {
    source
        .ancestors()
        .take_while(|path| path.starts_with(workspace_root))
        .map(|path| path.join("Cargo.toml"))
        .find(|path| path.is_file())
}

#[cfg(test)]
fn fix_source(
    source: &str,
    redundant: &BTreeSet<String>,
) -> Result<(String, FixOutcome, Vec<Finding>)> {
    let file = syn::parse_file(source)?;
    let analysis = analyze(source, &file, "test.rs", redundant);
    let mut outcome = FixOutcome::default();
    for finding in &analysis.findings {
        if let Some(reason) = &finding.skip {
            outcome.skipped.push(reason.clone());
        } else {
            outcome.changes.push(finding.method.clone());
        }
    }
    let mut rewriter = SourceRewriter::new(source);
    for edit in analysis.edits {
        rewriter.replace(edit.range, edit.text);
    }
    let changed = !rewriter.is_empty();
    let rewritten = rewriter.finish()?;
    outcome.writes = usize::from(changed);
    Ok((rewritten, outcome, analysis.findings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(source: &str) -> Result<(String, FixOutcome, Vec<Finding>)> {
        fix_source(source, &BTreeSet::new())
    }

    fn count(source: &str) -> usize {
        let file = syn::parse_file(source).expect("valid Rust source");
        analyze(source, &file, "test.rs", &BTreeSet::new())
            .findings
            .len()
    }

    #[test]
    fn all_fields_use_concise_get() -> Result<()> {
        let source = "struct User {\n    name: String,\n    count: u64,\n}\nimpl User {\n    pub fn name(&self) -> &str { &self.name }\n    pub fn count(&self) -> u64 { self.count }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "count"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(get)]\nstruct User {\n    name: String,\n    count: u64,\n}\n"
        );
        Ok(())
    }

    #[test]
    fn concise_get_refines_only_deref_opt_out() -> Result<()> {
        let source = "struct User {\n    name: String,\n    title: String,\n}\nimpl User {\n    pub fn name(&self) -> &str { &self.name }\n    pub fn title(&self) -> &String { &self.title }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "title"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(get)]\nstruct User {\n    name: String,\n    #[field(get(deref = false))]\n    title: String,\n}\n"
        );
        Ok(())
    }

    #[test]
    fn concise_get_uses_uniform_crate_visibility() -> Result<()> {
        let source = "struct User {\n    name: String,\n    count: u64,\n}\nimpl User {\n    pub(crate) fn name(&self) -> &str { &self.name }\n    pub(crate) fn count(&self) -> u64 { self.count }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "count"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(get, vis = \"pub(crate)\")]\nstruct User {\n    name: String,\n    count: u64,\n}\n"
        );
        Ok(())
    }

    #[test]
    fn partial_get_coverage_stays_opt_in() -> Result<()> {
        let source = "struct User {\n    name: String,\n    count: u64,\n}\nimpl User {\n    pub fn name(&self) -> &str { &self.name }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(opt_in, get)]\nstruct User {\n    #[field(get)]\n    name: String,\n    count: u64,\n}\n"
        );
        Ok(())
    }

    #[test]
    fn restricted_visibility_remains_valid_rust() -> Result<()> {
        let source = "struct User {\n    name: String,\n    count: u64,\n}\nimpl User {\n    pub(in crate::inner) fn name(&self) -> &str { &self.name }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name"]);
        assert!(
            fixed.contains("#[field(get, vis = \"pub(in crate::inner)\")]"),
            "{fixed}"
        );
        Ok(())
    }

    #[test]
    fn concise_get_adds_one_mutable_getter() -> Result<()> {
        let source = "struct User {\n    name: String,\n    count: u64,\n}\nimpl User {\n    pub fn name(&self) -> &str { &self.name }\n    pub fn edit_name(&mut self) -> &mut str { &mut self.name }\n    pub fn count(&self) -> u64 { self.count }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "edit_name", "count"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(get)]\nstruct User {\n    #[field(get_mut = edit_name)]\n    name: String,\n    count: u64,\n}\n"
        );
        Ok(())
    }

    #[test]
    fn tuple_struct_full_coverage_is_concise() -> Result<()> {
        let source = "struct Pair(\n    String,\n    u64,\n);\nimpl Pair {\n    pub fn name(&self) -> &str { &self.0 }\n    pub fn count(&self) -> u64 { self.1 }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "count"]);
        assert_eq!(
            fixed,
            "#[derive(fieldwork::Fieldwork)]\n#[fieldwork(get)]\nstruct Pair(\n    #[field(get = name)]\n    String,\n    #[field(get = count)]\n    u64,\n);\n"
        );
        Ok(())
    }

    #[test]
    fn default_and_arbitrary_names_convert() -> Result<()> {
        let source = "struct User {\n    name: String,\n    title: String,\n}\nimpl User { pub fn name(&self) -> &str { &self.name } pub fn label(&self) -> &str { &self.title } }\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "label"]);
        assert!(fixed.contains("#[fieldwork(get)]"));
        assert!(!fixed.contains("#[field(get)]\n    name: String"));
        assert!(fixed.contains("#[field(get = label)]\n    title: String"));
        assert!(!fixed.contains("fn name"));
        assert!(!fixed.contains("fn label"));
        Ok(())
    }

    #[test]
    fn copy_and_string_return_models_convert() -> Result<()> {
        let source = "struct Values {\n    count: u64,\n    name: String,\n}\nimpl Values { pub fn count(&self) -> u64 { self.count } pub fn name(&self) -> &str { &self.name } }\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes.len(), 2);
        assert!(fixed.contains("#[fieldwork(get)]"));
        assert!(!fixed.contains("#[field("));
        Ok(())
    }

    #[test]
    fn copy_and_deref_opt_outs_convert_exact_references() -> Result<()> {
        let source = "struct Values {\n    count: u64,\n    name: String,\n}\nimpl Values { pub fn count(&self) -> &u64 { &self.count } pub fn name(&self) -> &String { &self.name } }\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes.len(), 2);
        assert!(fixed.contains("#[field(get(copy = false))]"));
        assert!(fixed.contains("#[field(get(deref = false))]"));
        Ok(())
    }

    #[test]
    fn move_getter_proves_copy_for_unknown_type() -> Result<()> {
        let source = "struct Values {\n    duration: Duration,\n    name: String,\n}\nimpl Values { pub(crate) fn duration(&self) -> Duration { self.duration } }\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["duration"]);
        assert!(fixed.contains("#[field(get, vis = \"pub(crate)\", copy)]"));
        Ok(())
    }

    #[test]
    fn clone_and_option_reference_getters_stay_manual() -> Result<()> {
        let source = "struct Values {\n    name: String,\n    item: Option<String>,\n}\nimpl Values { pub fn name(&self) -> String { self.name.clone() } pub fn item(&self) -> &Option<String> { &self.item } }\n";
        let (fixed, outcome, findings) = fix(source)?;
        assert_eq!(fixed, source);
        assert_eq!(outcome.writes, 0);
        assert_eq!(findings.len(), 2);
        assert!(
            outcome
                .skipped
                .iter()
                .any(|reason| reason.contains("clone getter"))
        );
        assert!(
            outcome
                .skipped
                .iter()
                .any(|reason| reason.contains("&Option"))
        );
        Ok(())
    }

    #[test]
    fn mutable_getter_is_signature_driven_and_named_exactly() -> Result<()> {
        let source = "struct User {\n    name: String,\n}\nimpl User { pub(crate) fn edit_name(&mut self) -> &mut String { &mut self.name } }\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["edit_name"]);
        assert!(fixed.contains("#[fieldwork(opt_in, get, get_mut)]"));
        assert!(fixed.contains("get_mut = edit_name"));
        assert!(fixed.contains("vis = \"pub(crate)\""));
        assert!(fixed.contains("deref = false"));
        Ok(())
    }

    #[test]
    fn absent_mutable_getter_never_enables_get_mut() -> Result<()> {
        let source = "struct User {\n    name: String,\n}\nimpl User { pub fn name(&self) -> &str { &self.name } }\n";
        let (fixed, _, _) = fix(source)?;
        assert!(!fixed.contains("get_mut"));
        Ok(())
    }

    #[test]
    fn trait_and_cfg_test_getters_are_ignored() {
        assert_eq!(
            count(
                "struct User { name: String } impl Named for User { fn name(&self) -> &str { &self.name } }"
            ),
            0
        );
        assert_eq!(
            count(
                "struct User { name: String } #[cfg(test)] impl User { fn name(&self) -> &str { &self.name } }"
            ),
            0
        );
    }

    #[test]
    fn cross_file_struct_is_reported_but_not_fixed() -> Result<()> {
        let source = "impl User { pub fn name(&self) -> &str { &self.name } }\n";
        let (fixed, outcome, findings) = fix(source)?;
        assert_eq!(fixed, source);
        assert_eq!(findings.len(), 1);
        assert_eq!(outcome.skipped, ["struct declared in another file"]);
        Ok(())
    }

    #[test]
    fn redundant_accessor_is_left_for_delete_pass() -> Result<()> {
        let source = "pub struct User {\n    name: String,\n}\nimpl User { pub fn name(&self) -> &str { &self.name } }\n";
        let redundant = BTreeSet::from(["test.rs::User::name".to_owned()]);
        let (fixed, outcome, _) = fix_source(source, &redundant)?;
        assert_eq!(fixed, source);
        assert!(outcome.skipped[0].contains("redundant_accessors"));
        Ok(())
    }

    #[test]
    fn comments_visibility_and_existing_derive_are_preserved() -> Result<()> {
        let source = "// Struct comment.\n#[derive(Debug)]\nstruct User {\n    name: String,\n}\nimpl User {\n    /// Getter comment.\n    fn name(&self) -> &str { &self.name }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name"]);
        assert!(fixed.contains("// Struct comment.\n#[derive(Debug, fieldwork::Fieldwork)]"));
        assert!(fixed.contains("#[fieldwork(get, vis = \"\")]"));
        assert!(fixed.contains("/// Getter comment.\n    name: String"));
        assert!(!fixed.contains("#[field("));
        assert!(!fixed.contains("fn name"));
        Ok(())
    }

    #[test]
    fn fix_is_idempotent() -> Result<()> {
        let source = "struct User {\n    name: String,\n}\nimpl User { pub fn name(&self) -> &str { &self.name } }\n";
        let (fixed, first, _) = fix(source)?;
        assert_eq!(first.writes, 1);
        let (again, second, _) = fix(&fixed)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn partial_impl_deletion_leaves_no_whitespace_lines() -> Result<()> {
        let source = "struct User {\n    name: String,\n    title: String,\n}\nimpl User {\n    pub fn name(&self) -> &str { &self.name }\n\n    pub fn title(&self) -> &str { &self.title }\n\n    fn new(name: String, title: String) -> Self { Self { name, title } }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name", "title"]);
        assert!(fixed.contains("impl User {\n    fn new"));
        assert!(
            !fixed
                .lines()
                .any(|line| !line.is_empty() && line.trim().is_empty())
        );
        Ok(())
    }

    #[test]
    fn must_use_getter_converts_and_drops_the_attribute() -> Result<()> {
        let source = "struct User {\n    name: String,\n}\nimpl User {\n    /// The name.\n    #[must_use]\n    pub fn name(&self) -> &str { &self.name }\n}\n";
        let (fixed, outcome, _) = fix(source)?;
        assert_eq!(outcome.changes, ["name"]);
        assert!(!fixed.contains("must_use"));
        assert!(fixed.contains("/// The name."));
        assert!(!fixed.contains("fn name"));
        Ok(())
    }

    #[test]
    fn inline_getter_stays_manual() {
        let source = "struct User { name: String }\nimpl User {\n    #[inline]\n    pub fn name(&self) -> &str { &self.name }\n}\n";
        let (fixed, outcome, _) = fix(source).expect("fix");
        assert_eq!(fixed, source);
        assert!(
            outcome
                .skipped
                .iter()
                .any(|reason| reason.contains("non-droppable"))
        );
    }
}
