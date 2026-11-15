use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    ops::RangeInclusive,
};

use anyhow::{Context as _, Result};
use syn::{
    BinOp, Expr, FnArg, ImplItem, ItemFn, ItemImpl, Pat, ReturnType, Signature, Type,
    visit::{self, Visit},
};

use super::{
    Check, Context,
    cancel_root_sites::{attrs_indicate_test, configured_pattern_sites},
};
use crate::common::violation::Violation;

pub(crate) mod consts {
    pub(crate) const ID: &str = "smoothing_primitive_sites";

    pub(super) const EXPLANATION: &str = "\
Runtime parameters use `SmoothedParam` from `kithara_dsp::param` (firewheel's
type), composed as `MixDSP` for A-to-B transitions. `SmoothingFilter` is
sanctioned only for the equalizer's biquad gain bank and the facade; move
parameter smoothing to the owning config and primitive.";
}

pub(crate) struct SmoothingPrimitiveSites;

impl Check for SmoothingPrimitiveSites {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.smoothing_primitive_sites;
        let patterns = cfg
            .patterns
            .iter()
            .chain(&cfg.recurrence_patterns)
            .cloned()
            .collect::<Vec<_>>();
        let sites =
            configured_pattern_sites(ctx, &patterns, &cfg.allowed_files, &cfg.exempt_crates)?;
        let mut regions = BTreeMap::<String, Vec<RangeInclusive<usize>>>::new();
        let mut findings = Vec::new();
        for site in sites {
            if cfg.recurrence_patterns.contains(&site.pattern) {
                let (path, line) = site
                    .location
                    .rsplit_once(':')
                    .context("pattern site location")?;
                let line = line.parse::<usize>()?;
                let ranges = match regions.entry(path.to_owned()) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let source = fs::read_to_string(ctx.workspace_root.join(path))?;
                        let file = syn::parse_file(&source)?;
                        let mut parameters = ParameterRegions::default();
                        parameters.visit_file(&file);
                        entry.insert(parameters.ranges)
                    }
                };
                if !ranges.iter().any(|range| range.contains(&line)) {
                    continue;
                }
            }
            findings.push(
                Violation::deny(
                    consts::ID,
                    site.location,
                    format!(
                        "hand-rolled parameter smoother `{}` outside the sanctioned gain bank",
                        site.pattern
                    ),
                )
                .with_explanation(consts::EXPLANATION.into()),
            );
        }
        Ok(findings)
    }
}

#[derive(Default)]
struct ParameterRegions {
    ranges: Vec<RangeInclusive<usize>>,
}

impl<'ast> Visit<'ast> for ParameterRegions {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if attrs_indicate_test(&item.attrs) {
            return;
        }
        if parameter_recurrence(&item.sig, &item.block) {
            self.ranges.push(
                item.sig.fn_token.span.start().line
                    ..=item.block.brace_token.span.close().end().line,
            );
        }
        visit::visit_item_fn(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if attrs_indicate_test(&item.attrs) {
            return;
        }
        if item.items.iter().any(|item| matches!(item, ImplItem::Fn(method) if !attrs_indicate_test(&method.attrs) && parameter_recurrence(&method.sig, &method.block))) {
            self.ranges.push(item.impl_token.span.start().line..=item.brace_token.span.close().end().line);
        }
        visit::visit_item_impl(self, item);
    }
}

fn float_type(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path.path.is_ident("f32") || path.path.is_ident("f64"),
        Type::Reference(reference) => float_type(&reference.elem),
        _ => false,
    }
}

fn buffer_type(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => buffer_type(&reference.elem),
        Type::Slice(slice) => float_type(&slice.elem) || buffer_type(&slice.elem),
        Type::Array(array) => float_type(&array.elem) || buffer_type(&array.elem),
        Type::Path(path) => path.path.segments.iter().any(|segment| {
            matches!(&segment.arguments, syn::PathArguments::AngleBracketed(arguments) if arguments.args.iter().any(|argument| matches!(argument, syn::GenericArgument::Type(ty) if buffer_type(ty))))
        }),
        _ => false,
    }
}

fn bindings(pattern: &Pat, out: &mut BTreeSet<String>) {
    match pattern {
        Pat::Ident(ident) => {
            out.insert(ident.ident.to_string());
        }
        Pat::Tuple(tuple) => {
            for element in &tuple.elems {
                bindings(element, out);
            }
        }
        Pat::Reference(reference) => bindings(&reference.pat, out),
        _ => {}
    }
}

struct BufferReads<'a> {
    buffers: &'a BTreeSet<String>,
    old_state: Option<&'a Expr>,
    found: bool,
}

impl<'ast> Visit<'ast> for BufferReads<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.old_state == Some(expression) {
            return;
        }
        if let Expr::Path(path) = expression
            && path
                .path
                .get_ident()
                .is_some_and(|name| self.buffers.contains(&name.to_string()))
        {
            self.found = true;
        }
        visit::visit_expr(self, expression);
    }
}

fn reads_buffer(expression: &Expr, buffers: &BTreeSet<String>, old_state: Option<&Expr>) -> bool {
    let mut reads = BufferReads {
        buffers,
        old_state,
        found: false,
    };
    reads.visit_expr(expression);
    reads.found
}

fn stored_value(expression: &Expr) -> bool {
    matches!(expression, Expr::Field(_) | Expr::Index(_))
        || matches!(expression, Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Deref(_)))
}

#[derive(Default)]
struct Multiplication(bool);

impl<'ast> Visit<'ast> for Multiplication {
    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        self.0 |= matches!(expression.op, BinOp::Mul(_) | BinOp::MulAssign(_));
        visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
        self.0 |= matches!(
            expression.method.to_string().as_str(),
            "mul_add" | "mul_add_precise"
        );
        visit::visit_expr_method_call(self, expression);
    }
}

fn multiplication(expression: &Expr) -> bool {
    let mut math = Multiplication::default();
    math.visit_expr(expression);
    math.0
}

struct StateReads<'a> {
    state: &'a Expr,
    found: bool,
}

impl<'ast> Visit<'ast> for StateReads<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        self.found |= expression == self.state;
        visit::visit_expr(self, expression);
    }
}

#[derive(Default)]
struct Recurrence {
    buffers: BTreeSet<String>,
    multiply: bool,
    combine: bool,
    parameter_update: bool,
}

impl<'ast> Visit<'ast> for Recurrence {
    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        let previous = self.buffers.clone();
        if reads_buffer(&expression.expr, &self.buffers, None) {
            bindings(&expression.pat, &mut self.buffers);
        }
        visit::visit_expr_for_loop(self, expression);
        self.buffers = previous;
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if local
            .init
            .as_ref()
            .is_some_and(|init| reads_buffer(&init.expr, &self.buffers, None))
        {
            bindings(&local.pat, &mut self.buffers);
        }
        visit::visit_local(self, local);
    }

    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        self.multiply |= matches!(expression.op, BinOp::Mul(_) | BinOp::MulAssign(_));
        self.combine |= matches!(
            expression.op,
            BinOp::Add(_) | BinOp::Sub(_) | BinOp::AddAssign(_) | BinOp::SubAssign(_)
        );
        if stored_value(&expression.left)
            && (matches!(expression.op, BinOp::MulAssign(_))
                || (matches!(expression.op, BinOp::AddAssign(_) | BinOp::SubAssign(_))
                    && multiplication(&expression.right)))
            && !reads_buffer(&expression.right, &self.buffers, Some(&expression.left))
        {
            self.parameter_update = true;
        }
        visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_assign(&mut self, expression: &'ast syn::ExprAssign) {
        let mut reads = StateReads {
            state: &expression.left,
            found: false,
        };
        reads.visit_expr(&expression.right);
        if stored_value(&expression.left)
            && reads.found
            && multiplication(&expression.right)
            && !reads_buffer(&expression.right, &self.buffers, Some(&expression.left))
        {
            self.parameter_update = true;
        }
        visit::visit_expr_assign(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
        if matches!(
            expression.method.to_string().as_str(),
            "mul_add" | "mul_add_precise"
        ) {
            self.multiply = true;
            self.combine = true;
        }
        visit::visit_expr_method_call(self, expression);
    }
}

fn parameter_recurrence(signature: &Signature, body: &syn::Block) -> bool {
    let mut recurrence = Recurrence::default();
    let mut scalars = 0;
    for input in &signature.inputs {
        if let FnArg::Typed(input) = input {
            scalars += usize::from(float_type(&input.ty));
            if buffer_type(&input.ty) {
                bindings(&input.pat, &mut recurrence.buffers);
            }
        }
    }
    recurrence.visit_block(body);
    let scalar_result = matches!(&signature.output, ReturnType::Type(_, ty) if float_type(ty));
    recurrence.parameter_update
        || recurrence.multiply && recurrence.combine && scalar_result && scalars >= 2
}
