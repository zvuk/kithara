use std::fs;

use anyhow::Result;
use syn::{
    Expr, ExprMethodCall, Fields, ImplItem, ItemImpl, Lit, Path, RangeLimits, Token, Type, UnOp,
    punctuated::Punctuated,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        parse::{collect_scopes, self_ty_name},
        violation::Violation,
        walker::relative_to,
    },
    idioms::config::DerivableSeverity,
};

pub(crate) struct DerivableRanged;

impl Check for DerivableRanged {
    fn id(&self) -> &'static str {
        "derivable_ranged"
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let config = &ctx.config.thresholds.derivable_ranged;
        if !config.enabled {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let source = fs::read_to_string(path)?;
            let relative = relative_to(ctx.workspace_root, path).to_string_lossy();
            for (name, line) in check_source(&source) {
                let key = format!("{relative}:{line}:0");
                let message = format!(
                    "bounded newtype {name}: replace manual bounds with #[derive(Ranged)]; choose clamp only for a knob"
                );
                out.push(match config.severity {
                    DerivableSeverity::Deny => Violation::deny(self.id(), key, message),
                    DerivableSeverity::Warn => Violation::warn(self.id(), key, message),
                });
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn check_source(source: &str) -> Vec<(String, usize)> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for scope in collect_scopes(&file) {
        for item in scope.structs {
            let line = item.ident.span().start().line;
            let Fields::Unnamed(fields) = &item.fields else {
                continue;
            };
            if fields.unnamed.len() != 1 {
                continue;
            }
            let Some(field) = fields.unnamed.first() else {
                continue;
            };
            let Some(number) = numeric_type(&field.ty) else {
                continue;
            };
            let derived = item.attrs.iter().any(|attr| {
                attr.path().is_ident("derive")
                    && attr
                        .parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
                        .is_ok_and(|paths| {
                            paths.iter().any(|path| {
                                path.segments
                                    .last()
                                    .is_some_and(|segment| segment.ident == "Ranged")
                            })
                        })
            });
            if derived {
                continue;
            }
            let name = item.ident.to_string();
            if scope.impls.iter().any(|implementation| {
                implementation.trait_.is_none()
                    && self_ty_name(&implementation.self_ty).as_deref() == Some(name.as_str())
                    && bounded_impl(implementation, &number, &name)
            }) {
                out.push((name, line));
            }
        }
    }
    out
}

fn numeric_type(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    let name = path.path.get_ident()?.to_string();
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
            | "f32"
            | "f64"
    )
    .then_some(name)
}

fn bounded_impl(implementation: &ItemImpl, number: &str, name: &str) -> bool {
    let bounds: Vec<String> = implementation
        .items
        .iter()
        .filter_map(|item| {
            let ImplItem::Const(constant) = item else {
                return None;
            };
            let Type::Path(ty) = &constant.ty else {
                return None;
            };
            let name = constant.ident.to_string();
            ((ty.path.is_ident(number) || ty.path.is_ident("Self"))
                && (name.starts_with("MIN") || name.starts_with("MAX")))
            .then_some(name)
        })
        .collect();
    if bounds.iter().any(|name| name.starts_with("MIN"))
        && bounds.iter().any(|name| name.starts_with("MAX"))
    {
        return true;
    }
    implementation.items.iter().any(|item| {
        let ImplItem::Fn(function) = item else {
            return false;
        };
        let method = function.sig.ident.to_string();
        if !(matches!(method.as_str(), "new" | "checked" | "try_from")
            || method.starts_with("from_"))
        {
            return false;
        }
        let mut scan = RangeScan {
            name,
            bounds: &bounds,
            found: false,
        };
        scan.visit_block(&function.block);
        scan.found
    })
}

struct RangeScan<'a> {
    bounds: &'a [String],
    name: &'a str,
    found: bool,
}

impl RangeScan<'_> {
    fn bound(&self, expression: &Expr) -> Option<bool> {
        match expression {
            Expr::Lit(literal) => matches!(literal.lit, Lit::Int(_) | Lit::Float(_)).then_some(false),
            Expr::Unary(unary) if matches!(unary.op, UnOp::Neg(_)) => {
                matches!(&*unary.expr, Expr::Lit(literal) if matches!(literal.lit, Lit::Int(_) | Lit::Float(_))).then_some(false)
            }
            Expr::Path(path) if path.path.segments.len() == 2 => {
                let mut segments = path.path.segments.iter();
                let owner = segments.next().map(|segment| segment.ident.to_string());
                let constant = segments.next().map(|segment| segment.ident.to_string());
                ((owner.as_deref() == Some("Self") || owner.as_deref() == Some(self.name))
                    && constant.is_some_and(|name| self.bounds.contains(&name))).then_some(true)
            }
            Expr::Paren(paren) => self.bound(&paren.expr),
            Expr::Group(group) => self.bound(&group.expr),
            _ => None,
        }
    }

    fn pair(&self, first: &Expr, second: &Expr) -> bool {
        self.bound(first)
            .zip(self.bound(second))
            .is_some_and(|(a, b)| a == b)
    }

    fn range(&self, expression: &Expr) -> bool {
        match expression {
            Expr::Range(range) => range
                .start
                .as_deref()
                .zip(range.end.as_deref())
                .is_some_and(|(start, end)| {
                    matches!(range.limits, RangeLimits::Closed(_)) && self.pair(start, end)
                }),
            Expr::Paren(paren) => self.range(&paren.expr),
            Expr::Group(group) => self.range(&group.expr),
            _ => false,
        }
    }
}

impl<'ast> Visit<'ast> for RangeScan<'_> {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.found |= (node.method == "clamp"
            && node.args.len() == 2
            && node
                .args
                .first()
                .zip(node.args.last())
                .is_some_and(|(a, b)| self.pair(a, b)))
            || (node.method == "contains" && node.args.len() == 1 && self.range(&node.receiver));
        visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::check_source;

    /// The shape `Tempo` had before the derive: a bound pair plus a
    /// `contains` check in the constructor.
    #[test]
    fn a_bound_pair_with_a_checking_constructor_is_reported() {
        let findings = check_source(
            r#"
            pub struct Tempo(f64);

            impl Tempo {
                pub const MAX_BEATS_PER_MINUTE: f64 = 1_000.0;
                pub const MIN_BEATS_PER_MINUTE: f64 = 1.0;

                pub fn new(value: f64) -> Result<Self, Error> {
                    if (Self::MIN_BEATS_PER_MINUTE..=Self::MAX_BEATS_PER_MINUTE).contains(&value) {
                        Ok(Self(value))
                    } else {
                        Err(Error)
                    }
                }
            }
            "#,
        );

        assert_eq!(
            findings.len(),
            1,
            "the bounded newtype is reported: {findings:?}"
        );
    }

    /// A unit struct holds no value, so its associated constants never make
    /// it a bounded newtype.
    #[test]
    fn a_constant_group_is_not_reported() {
        let findings = check_source(
            r#"
            struct Consts;

            impl Consts {
                const MIN_GAIN: f32 = -24.0;
                const MAX_GAIN: f32 = 6.0;
            }
            "#,
        );

        assert!(
            findings.is_empty(),
            "a unit struct holds no value: {findings:?}"
        );
    }

    #[test]
    fn a_newtype_over_a_non_numeric_type_is_not_reported() {
        let findings = check_source(
            r#"
            pub struct Name(String);

            impl Name {
                pub const MIN_LEN: usize = 1;
                pub const MAX_LEN: usize = 64;
            }
            "#,
        );

        assert!(
            findings.is_empty(),
            "the field is not a number: {findings:?}"
        );
    }

    #[test]
    fn a_type_that_already_derives_ranged_is_not_reported() {
        let findings = check_source(
            r#"
            #[derive(Ranged)]
            #[ranged(min = 0, max = 100, default = 100)]
            pub struct Percent(u8);
            "#,
        );

        assert!(findings.is_empty(), "the derive is the fix: {findings:?}");
    }
    #[test]
    fn constructor_detection_obeys_the_declared_shapes() {
        for (body, expected) in [
            (
                "impl Value { fn new(v: f32) -> Self { Self(v.clamp(-2.0, 2.0)) } }",
                1,
            ),
            (
                "impl Value { fn checked(v: f32) -> bool { (-2.0..=2.0).contains(&v) } }",
                1,
            ),
            (
                "impl Value { fn from_gain(v: f32) -> Self { Self(v.clamp(0.0, 2.0)) } }",
                1,
            ),
            (
                "impl Value { fn try_from(v: f32) -> bool { (0.0..=2.0).contains(&v) } }",
                1,
            ),
            (
                "impl Value { const MIN: Self = Self(0.0); const MAX: Self = Self(2.0); }",
                1,
            ),
            (
                "impl Value { fn new(v: f32, hi: f32) -> Self { Self(v.clamp(0.0, hi)) } }",
                0,
            ),
            (
                "impl Value { fn new(v: f32) -> Self { Self(v.max(0.001)) } }",
                0,
            ),
            (
                "impl Value { fn new(v: f32) -> bool { (0.0..2.0).contains(&v) } }",
                0,
            ),
            (
                "impl Value { const MIN: f32 = 0.0; fn new(v: f32) -> Self { Self(v.clamp(Self::MIN, 2.0)) } }",
                0,
            ),
            (
                "impl Value { const MIN: usize = 0; const MAX: usize = 2; }",
                0,
            ),
            (
                "impl Value { fn scale(v: f32) -> f32 { v.clamp(0.0, 2.0) } }",
                0,
            ),
            (
                "impl Other for Value { fn new(v: f32) -> Self { Self(v.clamp(0.0, 2.0)) } }",
                0,
            ),
            (
                "#[cfg(test)] impl Value { const MIN: f32 = 0.0; const MAX: f32 = 2.0; }",
                1,
            ),
        ] {
            let source = format!("struct Value(f32); {body}");
            assert_eq!(check_source(&source).len(), expected, "{source}");
        }
    }
}
