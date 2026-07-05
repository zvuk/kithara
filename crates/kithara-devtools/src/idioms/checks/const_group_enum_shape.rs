use std::collections::HashMap;

use anyhow::Result;
use syn::{
    BinOp, Expr, ExprBinary, ExprLit, ExprUnary, Item, ItemConst, Lit, LitInt, Type, TypePath,
};

use super::{Check, Context};
use crate::{
    common::{
        parse::parse_file,
        suppress::Suppressions,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
    idioms::config::ConstGroupEnumShapeConfig,
};

pub(crate) const ID: &str = "const_group_enum_shape";

pub(crate) struct ConstGroupEnumShape;

#[derive(Debug, Clone)]
struct ConstEntry {
    name: String,
    ty: String,
    value: i128,
    line: usize,
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Sequential,
    BitFlags,
    SparseDiscriminants,
}

impl Check for ConstGroupEnumShape {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.const_group_enum_shape;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let src = std::fs::read_to_string(&path)?;
            let suppress = Suppressions::parse(&src);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            analyze_file(&rel_str, &file.items, cfg, &suppress, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn analyze_file(
    rel: &str,
    items: &[Item],
    cfg: &ConstGroupEnumShapeConfig,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    let mut entries: Vec<ConstEntry> = Vec::new();
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    analyze_file(rel, inner, cfg, sup, out);
                }
            }
            Item::Const(c) => {
                if let Some(e) = entry(c) {
                    entries.push(e);
                }
            }
            _ => {}
        }
    }
    for (prefix, ty, group) in group_by_prefix(&entries, cfg.min_group_size, cfg.min_prefix_chars) {
        let Some(shape) = classify_shape(&group) else {
            continue;
        };
        let line = group.first().map_or(0, |e| e.line);
        if sup.is_suppressed(line, ID) {
            continue;
        }
        let key = format!("{rel}:{line}:{prefix}{ty}");
        out.push(Violation::warn(
            ID,
            key,
            hint_for(&prefix, &ty, &group, shape),
        ));
    }
}

fn entry(c: &ItemConst) -> Option<ConstEntry> {
    let ty = primitive_int_type(&c.ty)?;
    let value = literal_value(&c.expr)?;
    Some(ConstEntry {
        ty,
        value,
        name: c.ident.to_string(),
        line: c.ident.span().start().line,
    })
}

fn primitive_int_type(ty: &Type) -> Option<String> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return None;
    };
    let seg = path.segments.last()?;
    let name = seg.ident.to_string();
    matches!(
        name.as_str(),
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
    )
    .then_some(name)
}

fn literal_value(expr: &Expr) -> Option<i128> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(li), ..
        }) => parse_int(li),
        Expr::Unary(ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => literal_value(expr).map(|v| -v),
        Expr::Binary(ExprBinary {
            left, op, right, ..
        }) => {
            let l = literal_value(left)?;
            let r = literal_value(right)?;
            match op {
                BinOp::Shl(_) => l.checked_shl(u32::try_from(r).ok()?),
                BinOp::Shr(_) => l.checked_shr(u32::try_from(r).ok()?),
                BinOp::BitOr(_) => Some(l | r),
                BinOp::BitAnd(_) => Some(l & r),
                BinOp::BitXor(_) => Some(l ^ r),
                BinOp::Add(_) => l.checked_add(r),
                BinOp::Sub(_) => l.checked_sub(r),
                BinOp::Mul(_) => l.checked_mul(r),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_int(li: &LitInt) -> Option<i128> {
    li.base10_parse::<i128>().ok()
}

fn group_by_prefix(
    entries: &[ConstEntry],
    min_group: usize,
    min_prefix: usize,
) -> Vec<(String, String, Vec<ConstEntry>)> {
    let mut by_type: HashMap<&str, Vec<ConstEntry>> = HashMap::new();
    for e in entries {
        by_type.entry(e.ty.as_str()).or_default().push(e.clone());
    }
    let mut groups = Vec::new();
    for (ty, list) in by_type {
        if list.len() < min_group {
            continue;
        }
        let prefix = longest_common_prefix(list.iter().map(|e| e.name.as_str()));
        if prefix.len() < min_prefix {
            continue;
        }
        groups.push((prefix, ty.to_string(), list));
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

fn longest_common_prefix<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.collect();
    if names.is_empty() {
        return String::new();
    }
    let first = names[0];
    let mut end = first.len();
    for n in &names[1..] {
        end = end.min(n.len());
        while end > 0 && !first.is_char_boundary(end) {
            end -= 1;
        }
        while end > 0 && first[..end] != n[..end] {
            end -= 1;
        }
    }
    first[..end].to_string()
}

fn classify_shape(group: &[ConstEntry]) -> Option<Shape> {
    let mut values: Vec<i128> = group.iter().map(|e| e.value).collect();
    values.sort_unstable();
    if values.windows(2).any(|w| w[0] == w[1]) {
        return None;
    }
    let lo = *values.first()?;
    let hi = *values.last()?;
    let n = i128::try_from(values.len()).ok()?;
    if hi - lo + 1 == n
        && values
            .iter()
            .enumerate()
            .all(|(i, v)| i128::try_from(i).is_ok_and(|i| *v == lo + i))
    {
        return Some(Shape::Sequential);
    }
    if values
        .iter()
        .all(|v| *v >= 0 && (*v == 0 || (v & (v - 1) == 0)))
    {
        return Some(Shape::BitFlags);
    }
    Some(Shape::SparseDiscriminants)
}

fn hint_for(prefix: &str, ty: &str, group: &[ConstEntry], shape: Shape) -> String {
    let names: Vec<&str> = group.iter().map(|e| e.name.as_str()).collect();
    let stripped: Vec<&str> = names.iter().map(|n| n.trim_start_matches(prefix)).collect();
    let mut camel: String = String::new();
    if let Some(first) = prefix.chars().next() {
        camel.push(first.to_ascii_uppercase());
        camel.push_str(
            &prefix
                .chars()
                .skip(1)
                .collect::<String>()
                .trim_end_matches('_')
                .to_lowercase(),
        );
    }
    if camel.is_empty() {
        camel.push_str("Group");
    }
    match shape {
        Shape::Sequential => format!(
            "group of {} `{ty}` constants sharing prefix `{prefix}` is structurally an enum. \
             Replace with `#[repr({ty})] #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)] \
             enum {camel} {{ {} }}` so the compiler enforces exhaustiveness.",
            group.len(),
            stripped.join(", "),
        ),
        Shape::BitFlags => format!(
            "group of {} `{ty}` constants with power-of-2 values sharing prefix `{prefix}` is \
             a bitfield. Replace with `bitflags::bitflags! {{ struct {camel}: {ty} {{ {} }} }}` \
             for type-safe set operations.",
            group.len(),
            group
                .iter()
                .map(|e| format!(
                    "const {} = 0x{:x};",
                    e.name.trim_start_matches(prefix),
                    e.value.cast_unsigned()
                ))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Shape::SparseDiscriminants => format!(
            "group of {} `{ty}` constants sharing prefix `{prefix}` looks FFI-mirrored. \
             Replace with `#[repr({ty})] enum {camel} {{ {} }}` (preserves ABI; explicit \
             discriminants).",
            group.len(),
            group
                .iter()
                .map(|e| format!(
                    "{} = 0x{:x}",
                    e.name.trim_start_matches(prefix),
                    e.value.cast_unsigned()
                ))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes(src: &str) -> Vec<&'static str> {
        let cfg = ConstGroupEnumShapeConfig::default();
        let suppress = Suppressions::parse(src);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file.items, &cfg, &suppress, &mut out);
        out.iter()
            .map(|v| classify_from_message(&v.message))
            .collect()
    }

    fn classify_from_message(msg: &str) -> &'static str {
        if msg.contains("structurally an enum") {
            "sequential"
        } else if msg.contains("bitfield") {
            "bitflags"
        } else if msg.contains("FFI-mirrored") {
            "sparse_discriminants"
        } else {
            "unknown"
        }
    }

    #[test]
    fn sequential_group_flagged_as_enum() {
        let src = "pub const STATE_IDLE: u8 = 0; \
                   pub const STATE_RUNNING: u8 = 1; \
                   pub const STATE_DONE: u8 = 2;";
        assert_eq!(shapes(src), vec!["sequential"]);
    }

    #[test]
    fn powers_of_two_flagged_as_bitflags() {
        let src = "pub const FLAG_A: u32 = 1; \
                   pub const FLAG_B: u32 = 2; \
                   pub const FLAG_C: u32 = 4; \
                   pub const FLAG_D: u32 = 8;";
        assert_eq!(shapes(src), vec!["bitflags"]);
    }

    #[test]
    fn ffi_hex_constants_flagged_as_sparse_enum() {
        let src = "pub const kAudioFormatLinearPCM: u32 = 0x6c70636d; \
                   pub const kAudioFormatAC3: u32 = 0x61632d33; \
                   pub const kAudioFormatMPEG4AAC: u32 = 0x61616320;";
        assert_eq!(shapes(src), vec!["sparse_discriminants"]);
    }

    #[test]
    fn duplicate_values_not_flagged() {
        let src = "pub const A: u8 = 1; pub const B: u8 = 1; pub const C: u8 = 2;";
        assert!(shapes(src).is_empty());
    }

    #[test]
    fn group_below_min_size_not_flagged() {
        let src = "pub const A: u8 = 0; pub const B: u8 = 1;";
        assert!(shapes(src).is_empty());
    }

    #[test]
    fn no_common_prefix_not_flagged() {
        let src = "pub const ALPHA: u8 = 0; pub const BETA: u8 = 1; pub const GAMMA: u8 = 2;";
        assert!(shapes(src).is_empty());
    }

    #[test]
    fn private_consts_also_caught() {
        let src = "const STATE_IDLE: u8 = 0; \
                   const STATE_RUNNING: u8 = 1; \
                   const STATE_DONE: u8 = 2;";
        assert_eq!(shapes(src), vec!["sequential"]);
    }
}
