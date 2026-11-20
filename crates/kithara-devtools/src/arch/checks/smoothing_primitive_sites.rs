use std::collections::BTreeSet;

use anyhow::Result;
use quote::ToTokens;
use syn::{
    Block, Expr, ExprAssign, ExprBinary, ExprClosure, ExprForLoop, ExprMethodCall, ExprPath, FnArg,
    ImplItemFn, Item, Local, Pat, PatIdent, TraitItemFn, Type, parse_file,
    visit::{self, Visit},
};

use super::{
    Check, Context,
    cancel_root_sites::{attrs_indicate_test, production_sources, scan_source},
};
use crate::common::{exclude::item_attrs, violation::Violation};

pub(crate) mod consts {
    pub(crate) const ID: &str = "smoothing_primitive_sites";

    pub(super) const EXPLANATION: &str = "\
Runtime parameters use `SmoothedParam` from `kithara_dsp::param` (firewheel's
type), composed as `MixDSP` for A-to-B transitions. `SmoothingFilter` is
sanctioned only for the equalizer's biquad gain bank and the facade. A state
that steps toward a scalar target (`s += k * (t - s)`, `s = a * s + b * t`) is
a hand-rolled parameter smoother; move parameter smoothing to the owning config
and primitive. A recurrence whose input comes from a sample slice, iterator or
chunk is signal processing, not parameter smoothing.";

    pub(super) const SMOOTHER: &str =
        "hand-rolled parameter smoother: a state steps toward a scalar target";
}

pub(crate) struct SmoothingPrimitiveSites;

impl Check for SmoothingPrimitiveSites {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.smoothing_primitive_sites;
        let mut violations = Vec::new();
        for source in production_sources(ctx, &cfg.allowed_files, &cfg.exempt_crates)? {
            violations.extend(findings(&source.content, &cfg.patterns).into_iter().map(
                |(line, message)| {
                    Violation::deny(consts::ID, format!("{}:{line}", source.path), message)
                        .with_explanation(consts::EXPLANATION.into())
                },
            ));
        }
        Ok(violations)
    }
}

/// Every (1-based line, message) in `content`: configured primitive names
/// used outside the sanctioned files, and smoother-shaped state updates.
fn findings(content: &str, patterns: &[String]) -> Vec<(usize, String)> {
    let mut found: Vec<(usize, String)> = scan_source(content, patterns)
        .into_iter()
        .map(|(line, pattern)| {
            (
                line,
                format!("`{pattern}` outside the sanctioned gain bank"),
            )
        })
        .collect();
    if let Ok(file) = parse_file(content) {
        let mut functions = Functions::default();
        functions.visit_file(&file);
        found.extend(
            functions
                .lines
                .into_iter()
                .map(|line| (line, consts::SMOOTHER.to_owned())),
        );
    }
    found.sort();
    found
}

/// Walks production functions; each body is classified on its own bindings.
#[derive(Default)]
struct Functions {
    lines: Vec<usize>,
}

impl Functions {
    fn body<'a>(&mut self, inputs: impl Iterator<Item = &'a FnArg>, block: &'a Block) {
        let mut bindings = Bindings::default();
        for input in inputs {
            if let FnArg::Typed(arg) = input
                && is_sequence(&arg.ty)
            {
                idents(&arg.pat, &mut bindings.tainted);
            }
        }
        bindings.collect(block);
        let mut steps = Steps {
            bindings: &bindings,
            lines: &mut self.lines,
        };
        steps.visit_block(block);
    }
}

impl<'ast> Visit<'ast> for Functions {
    fn visit_item(&mut self, item: &'ast Item) {
        if attrs_indicate_test(item_attrs(item)) {
            return;
        }
        if let Item::Fn(function) = item {
            self.body(function.sig.inputs.iter(), &function.block);
        } else {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        if !attrs_indicate_test(&function.attrs) {
            self.body(function.sig.inputs.iter(), &function.block);
        }
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        if let Some(block) = &function.default
            && !attrs_indicate_test(&function.attrs)
        {
            self.body(function.sig.inputs.iter(), block);
        }
    }
}

/// Names bound inside one function body. `elements` are bound per step by a
/// loop or closure over a sequence; `tainted` adds sequence parameters and
/// every local computed from a sequence; `fed` are states some assignment
/// takes from a sequence, so the state follows the signal. A closure passed
/// to a method (an iterator adaptor) binds elements; any other closure binds
/// parameters like a function.
#[derive(Default)]
struct Bindings<'ast> {
    elements: BTreeSet<String>,
    tainted: BTreeSet<String>,
    fed: BTreeSet<String>,
    locals: Vec<(BTreeSet<String>, &'ast Expr)>,
    assignments: Vec<(&'ast Expr, &'ast Expr)>,
}

impl<'ast> Bindings<'ast> {
    fn collect(&mut self, block: &'ast Block) {
        self.visit_block(block);
        self.tainted.extend(self.elements.iter().cloned());
        let mut grew = true;
        while grew {
            grew = false;
            for (names, init) in &self.locals {
                if !names.is_subset(&self.tainted)
                    && reads_sequence(init, None, None, &self.tainted)
                {
                    self.tainted.extend(names.iter().cloned());
                    grew = true;
                }
            }
        }
        for &(state, update) in &self.assignments {
            let place = key(state);
            if reads_sequence(update, Some(&place), self.owner(state), &self.tainted) {
                self.fed.insert(place);
            }
        }
    }

    /// The element a field or index state belongs to (`v` in `v.gain`):
    /// reads of that element's other fields are per-item parameters, not
    /// samples.
    fn owner<'e>(&self, state: &'e Expr) -> Option<&'e syn::Ident> {
        let mut base = state;
        loop {
            base = match strip(base) {
                Expr::Field(field) => &field.base,
                Expr::Index(index) => &index.expr,
                Expr::Path(path) if !std::ptr::eq(base, state) => {
                    return path
                        .path
                        .get_ident()
                        .filter(|name| self.elements.contains(&name.to_string()));
                }
                _ => return None,
            };
        }
    }

    fn is_element(&self, place: &Expr) -> bool {
        matches!(strip(place), Expr::Path(path) if single_ident(path).is_some_and(|name| self.elements.contains(&name)))
    }
}

impl<'ast> Visit<'ast> for Bindings<'ast> {
    fn visit_expr_for_loop(&mut self, looped: &'ast ExprForLoop) {
        if !is_range(&looped.expr) {
            idents(&looped.pat, &mut self.elements);
        }
        visit::visit_expr_for_loop(self, looped);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        for argument in &call.args {
            if let Expr::Closure(closure) = strip(argument) {
                for input in &closure.inputs {
                    idents(input, &mut self.elements);
                }
            }
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_closure(&mut self, closure: &'ast ExprClosure) {
        for input in &closure.inputs {
            if let Pat::Type(typed) = input
                && is_sequence(&typed.ty)
            {
                idents(&typed.pat, &mut self.tainted);
            }
        }
        visit::visit_expr_closure(self, closure);
    }

    fn visit_expr_assign(&mut self, assign: &'ast ExprAssign) {
        self.assignments.push((&assign.left, &assign.right));
        visit::visit_expr_assign(self, assign);
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if let Some(init) = &local.init {
            let mut names = BTreeSet::new();
            idents(&local.pat, &mut names);
            self.locals.push((names, &init.expr));
        }
        visit::visit_local(self, local);
    }
}

/// Finds state updates shaped like a parameter smoother.
struct Steps<'a> {
    bindings: &'a Bindings<'a>,
    lines: &'a mut Vec<usize>,
}

impl Steps<'_> {
    fn check(&mut self, state: &Expr, update: &Expr, compound: bool) {
        let place = key(state);
        if self.bindings.is_element(state) || self.bindings.fed.contains(&place) {
            return;
        }
        let shaped = moves_by_difference(update, &place) || (!compound && blends(update, &place));
        if shaped
            && !reads_sequence(
                update,
                Some(&place),
                self.bindings.owner(state),
                &self.bindings.tainted,
            )
        {
            self.lines.push(span_line(state));
        }
    }
}

impl<'ast> Visit<'ast> for Steps<'_> {
    fn visit_expr_assign(&mut self, assign: &'ast ExprAssign) {
        self.check(&assign.left, &assign.right, false);
        visit::visit_expr_assign(self, assign);
    }

    fn visit_expr_binary(&mut self, binary: &'ast ExprBinary) {
        if matches!(
            binary.op,
            syn::BinOp::AddAssign(_) | syn::BinOp::SubAssign(_)
        ) {
            self.check(&binary.left, &binary.right, true);
        }
        visit::visit_expr_binary(self, binary);
    }
}

/// `k * (target - state)` (either difference order) anywhere in `update`.
fn moves_by_difference(update: &Expr, place: &str) -> bool {
    let mut products = Vec::new();
    collect_products(update, &mut products);
    products.iter().flatten().any(|factor| {
        difference(factor).is_some_and(|(left, right)| {
            (key(left) == place && !contains(right, place))
                || (key(right) == place && !contains(left, place))
        })
    })
}

/// `a * state + b * target`: one additive term scales the state, another
/// weights a non-literal value that does not read the state.
fn blends(update: &Expr, place: &str) -> bool {
    let terms = terms(update);
    terms.len() > 1
        && terms
            .iter()
            .any(|factors| factors.len() > 1 && factors.iter().any(|factor| key(factor) == place))
        && terms.iter().any(|factors| {
            factors.len() > 1
                && !factors.iter().any(|factor| contains(factor, place))
                && !factors
                    .iter()
                    .all(|factor| matches!(strip(factor), Expr::Lit(_)))
        })
}

fn collect_products<'a>(expr: &'a Expr, out: &mut Vec<Vec<&'a Expr>>) {
    struct Products<'a, 'o> {
        out: &'o mut Vec<Vec<&'a Expr>>,
    }
    impl<'a> Visit<'a> for Products<'a, '_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Some((left, right)) = product(expr) {
                let mut both = factors(left);
                both.extend(factors(right));
                self.out.push(both);
            } else if let Some((left, right, _)) = fused(expr) {
                self.out.push(vec![left, right]);
            }
            visit::visit_expr(self, expr);
        }
    }
    Products { out }.visit_expr(expr);
}

fn terms(expr: &Expr) -> Vec<Vec<&Expr>> {
    if let Some((left, right)) = sum(expr) {
        let mut all = terms(left);
        all.extend(terms(right));
        all
    } else if let Some((left, right, addend)) = fused(expr) {
        let mut all = vec![vec![left, right]];
        all.extend(terms(addend));
        all
    } else {
        vec![factors(expr)]
    }
}

fn factors(expr: &Expr) -> Vec<&Expr> {
    product(expr).map_or_else(
        || vec![strip(expr)],
        |(left, right)| {
            let mut all = factors(left);
            all.extend(factors(right));
            all
        },
    )
}

fn product(expr: &Expr) -> Option<(&Expr, &Expr)> {
    operation(expr, "mul", |op| matches!(op, syn::BinOp::Mul(_)))
}

fn sum(expr: &Expr) -> Option<(&Expr, &Expr)> {
    operation(expr, "add", |op| matches!(op, syn::BinOp::Add(_))).or_else(|| difference(expr))
}

fn difference(expr: &Expr) -> Option<(&Expr, &Expr)> {
    operation(expr, "sub", |op| matches!(op, syn::BinOp::Sub(_)))
}

/// A binary operator or its `std::ops` method form (`a.mul(b)`).
fn operation<'a>(
    expr: &'a Expr,
    method: &str,
    operator: impl Fn(&syn::BinOp) -> bool,
) -> Option<(&'a Expr, &'a Expr)> {
    match strip(expr) {
        Expr::Binary(binary) if operator(&binary.op) => Some((&binary.left, &binary.right)),
        Expr::MethodCall(call) if call.method == method && call.args.len() == 1 => {
            call.args.first().map(|arg| (&*call.receiver, arg))
        }
        _ => None,
    }
}

/// `a.mul_add(b, c)` and its precision variants: `a * b + c`.
fn fused(expr: &Expr) -> Option<(&Expr, &Expr, &Expr)> {
    match strip(expr) {
        Expr::MethodCall(call)
            if call.method.to_string().starts_with("mul_add") && call.args.len() == 2 =>
        {
            let mut args = call.args.iter();
            Some((&*call.receiver, args.next()?, args.next()?))
        }
        _ => None,
    }
}

/// Whether `expr` reads a sequence: a sequence parameter, an element, an
/// index into either, or a local derived from them. `place` (the state itself)
/// and `owner` (the element the state belongs to) are not sequence reads.
fn reads_sequence(
    expr: &Expr,
    place: Option<&str>,
    owner: Option<&syn::Ident>,
    tainted: &BTreeSet<String>,
) -> bool {
    struct Reads<'a> {
        place: Option<&'a str>,
        owner: Option<&'a syn::Ident>,
        tainted: &'a BTreeSet<String>,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Reads<'_> {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if self.found || self.place.is_some_and(|place| key(expr) == place) {
                return;
            }
            visit::visit_expr(self, expr);
        }
        fn visit_expr_path(&mut self, path: &'ast ExprPath) {
            if let Some(name) = path.path.get_ident()
                && self.owner != Some(name)
                && self.tainted.contains(&name.to_string())
            {
                self.found = true;
            }
        }
    }
    let mut reads = Reads {
        place,
        owner,
        tainted,
        found: false,
    };
    reads.visit_expr(expr);
    reads.found
}

fn contains(expr: &Expr, place: &str) -> bool {
    struct Contains<'a> {
        place: &'a str,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Contains<'_> {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if key(expr) == self.place {
                self.found = true;
            } else if !self.found {
                visit::visit_expr(self, expr);
            }
        }
    }
    let mut contains = Contains {
        place,
        found: false,
    };
    contains.visit_expr(expr);
    contains.found
}

fn idents(pat: &Pat, out: &mut BTreeSet<String>) {
    struct Idents<'a> {
        out: &'a mut BTreeSet<String>,
    }
    impl<'ast> Visit<'ast> for Idents<'_> {
        fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
            self.out.insert(pat.ident.to_string());
            visit::visit_pat_ident(self, pat);
        }
    }
    Idents { out }.visit_pat(pat);
}

/// Slices, arrays and `impl Trait` (iterators), behind any references.
fn is_sequence(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => is_sequence(&reference.elem),
        Type::Paren(paren) => is_sequence(&paren.elem),
        Type::Group(group) => is_sequence(&group.elem),
        Type::Slice(_) | Type::Array(_) | Type::ImplTrait(_) => true,
        _ => false,
    }
}

/// `a..b` and adaptor chains on it (`(0..n).rev()`) count steps, not samples.
fn is_range(expr: &Expr) -> bool {
    match strip(expr) {
        Expr::Range(_) => true,
        Expr::MethodCall(call) => is_range(&call.receiver),
        _ => false,
    }
}

fn single_ident(path: &ExprPath) -> Option<String> {
    path.path.get_ident().map(ToString::to_string)
}

/// Comparison key: the place behind parentheses, derefs and borrows.
fn key(expr: &Expr) -> String {
    strip(expr).to_token_stream().to_string()
}

fn strip(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => strip(&paren.expr),
        Expr::Group(group) => strip(&group.expr),
        Expr::Reference(reference) => strip(&reference.expr),
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Deref(_)) => strip(&unary.expr),
        _ => expr,
    }
}

fn span_line(expr: &Expr) -> usize {
    syn::spanned::Spanned::span(expr).start().line
}

#[cfg(test)]
mod tests {
    use super::{consts, findings};

    fn patterns() -> Vec<String> {
        ["SmoothingFilter", "smoothing_coeff"]
            .map(String::from)
            .to_vec()
    }

    const PRIMITIVE: &str = "`SmoothingFilter` outside the sanctioned gain bank";

    /// Each row: a source, and the messages its production code must yield.
    #[test]
    fn smoother_shape_not_name_decides() {
        let rows: [(&str, &str, &[&str]); 20] = [
            (
                "scalar smoother named one_pole",
                "fn one_pole(state: &mut f32, target: f32, k: f32) {\n    *state = k.mul_add(*state, (1.0 - k) * target);\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "scalar smoother with a neutral name",
                "struct Glide { value: f32, target: f32, rate: f32 }\nimpl Glide {\n    fn next(&mut self) -> f32 {\n        self.value += self.rate * (self.target - self.value);\n        self.value\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "weighted blend toward an argument",
                "fn ease(s: &mut f32, t: f32, a: f32) {\n    *s = a * *s + (1.0 - a) * t;\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "parameter smoother inside a per-sample loop",
                "impl Voice {\n    fn render(&mut self, out: &mut [f32]) {\n        for sample in out.iter_mut() {\n            self.gain += self.rate * (self.target - self.gain);\n            *sample *= self.gain;\n        }\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "per-channel parameter smoother over indexed fields",
                "impl Bank {\n    fn step(&mut self) {\n        for ch in 0..2 {\n            self.gain[ch] += self.rate * (self.target[ch] - self.gain[ch]);\n        }\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "per-voice parameter smoother over element fields",
                "fn glide(voices: &mut [Voice]) {\n    for v in voices.iter_mut() {\n        v.gain += v.rate * (v.target - v.gain);\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "let-bound closure toward a scalar target",
                "fn ramp(s: &mut f32, k: f32, t: f32) {\n    let mut step = |t: f32| *s += k * (t - *s);\n    step(t);\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "weighted EWMA with a per-sample coefficient",
                "impl Ewma {\n    fn add_sample(&mut self, weight: f64, val: f64) {\n        let a = self.alpha.powf(weight.max(0.0));\n        self.last = val.mul_add(1.0 - a, a * self.last);\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "time-constant smoother toward a target",
                "impl Follow {\n    fn tick(&mut self, dt: f64) {\n        let k = (-dt / self.tau).exp();\n        self.value += (1.0 - k) * (self.target - self.value);\n    }\n}\n",
                &[consts::SMOOTHER],
            ),
            (
                "SmoothingFilter outside the facade",
                "fn gain() -> SmoothingFilter {\n    SmoothingFilter::new(1.0)\n}\n",
                &[PRIMITIVE, PRIMITIVE],
            ),
            (
                "recurrence over a slice iterator named one_pole",
                "fn one_pole_low_pass(k: f32, state: &mut f32, samples: &mut [f32]) {\n    for sample in samples.iter_mut() {\n        *state = k.mul_add(*state, (1.0 - k) * *sample);\n        *sample = *state;\n    }\n}\n",
                &[],
            ),
            (
                "recurrence over an indexed buffer",
                "fn comb(y: &mut f32, x: &[f32], g: f32) {\n    for i in 0..x.len() {\n        *y = g * *y + x[i];\n    }\n}\n",
                &[],
            ),
            (
                "recurrence over loaded tiles",
                "fn tiles(previous: &mut V, rows: impl Iterator<Item = [f32; 4]>, a: V, b: V) {\n    for row in rows {\n        let input = V::from_slice(&row);\n        *previous = previous.mul_add(a, input.mul(b));\n    }\n}\n",
                &[],
            ),
            (
                "envelope fed by the buffer",
                "fn release(r: f32, gain: &mut f32, targets: &mut [f32]) {\n    for target in targets.iter_mut() {\n        *gain = (1.0 - *gain).mul_add(-r, 1.0);\n        if *target < *gain {\n            *gain = *target;\n        }\n        *target = *gain;\n    }\n}\n",
                &[],
            ),
            (
                "digit accumulator",
                "fn bits(n: u32) -> u32 {\n    let mut acc = 0;\n    for bit in 0..n {\n        let carry = bit & 1;\n        acc = acc * 2 + carry;\n    }\n    acc\n}\n",
                &[],
            ),
            (
                "module declaration, re-export and call",
                "mod one_pole;\npub use one_pole::OnePoleLowPass;\nfn damp(x: &mut [f32]) {\n    one_pole_low_pass(0.2, &mut [0.0], x);\n}\n",
                &[],
            ),
            (
                "accumulator without a target",
                "struct Osc { phase: f32, step: f32, rate: f32 }\nimpl Osc {\n    fn advance(&mut self) {\n        self.phase += self.step * self.rate;\n    }\n}\n",
                &[],
            ),
            (
                "per-sample affine map",
                "fn gain(samples: &mut [f32], a: f32, b: f32) {\n    for s in samples {\n        *s = a * *s + b;\n    }\n}\n",
                &[],
            ),
            (
                "test code is not production",
                "#[cfg(test)]\nmod tests {\n    fn glide(s: &mut f32, t: f32) {\n        *s += 0.1 * (t - *s);\n    }\n}\n",
                &[],
            ),
            (
                "comment is not code",
                "// SmoothingFilter and s += k * (t - s) in a comment\nfn noop() {}\n",
                &[],
            ),
        ];
        let failures: Vec<String> = rows
            .into_iter()
            .filter_map(|(name, source, expected)| {
                let messages: Vec<String> = findings(source, &patterns())
                    .into_iter()
                    .map(|(_, message)| message)
                    .collect();
                (messages != expected).then(|| format!("{name}: {messages:?}"))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
