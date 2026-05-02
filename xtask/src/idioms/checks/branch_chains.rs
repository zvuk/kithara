//! Long `if/else if/else` chains hide a classifier that's better expressed as a
//! `match` (homogeneous arms) or a tag-enum + `match` (heterogeneous arms).
//!
//! Tiers:
//!
//! - **B1 — homogeneous**: every arm tests the same canonical expression with
//!   the same operator (or `matches!` on the same receiver). Even short chains
//!   here are strong match-conversion candidates.
//! - **B2 — heterogeneous**: arms test unrelated predicates. Hint: extract
//!   predicates into named bindings, or build a tag enum.
//! - **B3 — long**: any chain past `general_arms` triggers a generic
//!   cognitive-load signal.
//!
//! Modern CPUs penalise long mispredicted chains (~20 cycles per miss) and the
//! optimiser cannot turn unrelated predicates into a jump table. Even purely
//! cold chains hurt readability.
//!
//! Out of scope:
//! - chains where every condition is a `cfg!()` macro call (compile-time)
//! - `if let` chains (configurable via `count_if_let`; default off)

use std::collections::BTreeSet;

use anyhow::Result;
use syn::{
    BinOp, Expr, ExprBinary, ExprIf, ExprMacro,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        parse::{canonical_subject, parse_file},
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
    idioms::config::BranchChainsConfig,
};

pub(crate) const ID: &str = "branch_chains";

pub(crate) struct BranchChains;

impl Check for BranchChains {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.branch_chains;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let mut v = ChainVisitor {
                cfg,
                rel: &rel,
                skip: BTreeSet::new(),
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct ChainVisitor<'a> {
    cfg: &'a BranchChainsConfig,
    rel: &'a str,
    /// `(line, column)` of `if` tokens already consumed as part of an outer
    /// chain. Prevents double-reporting nested `else if` arms.
    skip: BTreeSet<(usize, usize)>,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for ChainVisitor<'_> {
    fn visit_expr_if(&mut self, e: &'ast ExprIf) {
        let key = span_key(e);
        if !self.skip.contains(&key) {
            let chain = collect_chain(e);
            for inner in &chain[1..] {
                self.skip.insert(span_key(inner));
            }
            self.process_chain(&chain);
        }
        visit::visit_expr_if(self, e);
    }
}

impl ChainVisitor<'_> {
    fn process_chain(&mut self, chain: &[&ExprIf]) {
        let arms = chain.len();
        if arms < 2 {
            return;
        }

        // skip cfg!()-only chains (compile-time switches)
        if chain.iter().all(|c| is_cfg_macro(&c.cond)) {
            return;
        }

        // if-let handling (configurable)
        let has_if_let = chain.iter().any(|c| matches!(*c.cond, Expr::Let(_)));
        if has_if_let && !self.cfg.count_if_let {
            return;
        }

        let kind = classify_chain(chain);
        let warn = match kind {
            ChainKind::Homogeneous { .. } | ChainKind::MatchesMacro { .. } => {
                arms >= self.cfg.homogeneous_arms
            }
            ChainKind::Heterogeneous => arms >= self.cfg.heterogeneous_arms,
        } || arms >= self.cfg.general_arms;

        if !warn {
            return;
        }

        let (line, col) = span_key(chain[0]);
        let key = format!("{}:{}:{}", self.rel, line, col);
        let msg = match &kind {
            ChainKind::Homogeneous { lhs, op } => format!(
                "B1: {arms}-arm chain on `{lhs}` ({op}) — convert to `match {lhs} {{ ... }}`"
            ),
            ChainKind::MatchesMacro { receiver } => format!(
                "B1: {arms}-arm chain of `matches!({receiver}, ...)` — collapse into `match {receiver} {{ ... }}`"
            ),
            ChainKind::Heterogeneous if arms >= self.cfg.heterogeneous_arms => format!(
                "B2: {arms}-arm chain across heterogeneous conditions — \
                 consider extracting predicates or a tag-enum + match"
            ),
            ChainKind::Heterogeneous => format!(
                "B3: {arms}-arm chain — splits into many independent branches; \
                 consider restructuring (extract predicates, decision table)"
            ),
        };
        self.out.push(Violation::warn(ID, key, msg));
    }
}

fn collect_chain(e: &ExprIf) -> Vec<&ExprIf> {
    let mut out = vec![e];
    let mut cur = e;
    while let Some((_, else_expr)) = &cur.else_branch {
        if let Expr::If(next) = else_expr.as_ref() {
            out.push(next);
            cur = next;
        } else {
            break;
        }
    }
    out
}

fn span_key(e: &ExprIf) -> (usize, usize) {
    let s = e.if_token.span().start();
    (s.line, s.column)
}

fn is_cfg_macro(cond: &Expr) -> bool {
    matches!(cond, Expr::Macro(ExprMacro { mac, .. }) if mac.path.is_ident("cfg"))
}

#[derive(Debug)]
enum ChainKind {
    Homogeneous { lhs: String, op: &'static str },
    MatchesMacro { receiver: String },
    Heterogeneous,
}

fn classify_chain(chain: &[&ExprIf]) -> ChainKind {
    // Try matches!() macro first.
    let mut matches_receivers: Vec<String> = Vec::with_capacity(chain.len());
    for c in chain {
        if let Some(rcv) = matches_macro_receiver(&c.cond) {
            matches_receivers.push(rcv);
        } else {
            matches_receivers.clear();
            break;
        }
    }
    if !matches_receivers.is_empty() && matches_receivers.iter().all(|r| r == &matches_receivers[0])
    {
        return ChainKind::MatchesMacro {
            receiver: matches_receivers.swap_remove(0),
        };
    }

    // Try homogeneous binary chain.
    let mut sample: Option<(String, &'static str)> = None;
    for c in chain {
        let Some((lhs, op)) = extract_binary(&c.cond) else {
            return ChainKind::Heterogeneous;
        };
        match &sample {
            None => sample = Some((lhs, op)),
            Some((s_lhs, s_op)) if s_lhs == &lhs && *s_op == op => {}
            Some(_) => return ChainKind::Heterogeneous,
        }
    }
    match sample {
        Some((lhs, op)) => ChainKind::Homogeneous { lhs, op },
        None => ChainKind::Heterogeneous,
    }
}

fn extract_binary(cond: &Expr) -> Option<(String, &'static str)> {
    let Expr::Binary(ExprBinary {
        left, op, right, ..
    }) = cond
    else {
        return None;
    };
    let op_str = bin_op_str(op)?;
    // Pick the side that looks like a "subject" (path/field access) rather than
    // a literal, so `x == 1` and `1 == x` (rare) collapse onto the same key.
    let lhs = canonical_subject(left).or_else(|| canonical_subject(right))?;
    Some((lhs, op_str))
}

fn bin_op_str(op: &BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Eq(_) => "==",
        BinOp::Ne(_) => "!=",
        BinOp::Lt(_) => "<",
        BinOp::Le(_) => "<=",
        BinOp::Gt(_) => ">",
        BinOp::Ge(_) => ">=",
        _ => return None,
    })
}

fn matches_macro_receiver(cond: &Expr) -> Option<String> {
    let Expr::Macro(m) = cond else { return None };
    if !m.mac.path.is_ident("matches") {
        return None;
    }
    // First token of `matches!(EXPR, PATTERN)` is the subject expression.
    // Parse macro tokens as Punctuated::<Expr, Comma> via syn::parse2 — but
    // `matches!` may carry guard `if ...`, so we parse only the receiver
    // expression up to the first comma at the top level.
    let tokens = m.mac.tokens.clone();
    let parsed: Result<RcvAndRest, _> = syn::parse2(tokens);
    parsed
        .ok()
        .map(|r| canonical_subject(&r.receiver).unwrap_or_default())
}

struct RcvAndRest {
    receiver: Expr,
}

impl syn::parse::Parse for RcvAndRest {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let receiver: Expr = input.parse()?;
        // discard remainder (pattern, guard, …)
        let _: proc_macro2::TokenStream = input.parse()?;
        Ok(Self { receiver })
    }
}
