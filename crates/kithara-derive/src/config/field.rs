use proc_macro2::TokenStream;
use syn::{
    Attribute, Expr, Lit, LitStr, Meta, Path, Result, Type, ext::IdentExt as _,
    meta::ParseNestedMeta, parenthesized, parse::Parser as _,
};

#[derive(Clone)]
pub(super) enum Role {
    Value,
    Projection(Box<(Type, Expr)>),
    Nested,
    Skip,
}

/// `wrap(default = expr, with = path)`: the builder takes the projected wire
/// value and wraps it, defaulting to the wrapped `default`.
#[derive(Clone)]
pub(super) struct Wrap {
    pub(super) default: Expr,
    pub(super) with: Path,
}

/// How a live field's change is executed: on the shared path of its owner's
/// executor, or by a method of the owner named after the field.
#[derive(Clone, Copy)]
pub(super) enum Live {
    Shared,
    Owner,
}

#[derive(Clone, Copy)]
pub(super) enum Accessor {
    Ref,
    Copy,
    Skip,
}

/// The shared grammar for a type's field defaults and a field's overrides.
#[derive(Clone, Default)]
pub(super) struct Declaration {
    pub(super) role: Option<Role>,
    pub(super) live: Option<Live>,
    /// `fn(T) -> Result<T, E>` the field's value passes.
    pub(super) check: Option<Path>,
    pub(super) sdk: bool,
    pub(super) builder: Option<TokenStream>,
    pub(super) accessor: Option<Accessor>,
    pub(super) debug_skipped: bool,
    pub(super) wrap: Option<Wrap>,
    pub(super) patch: Option<TokenStream>,
}

impl Declaration {
    pub(super) fn parse(attributes: &[Attribute]) -> Result<Self> {
        let mut declaration = Self::default();
        for attr in attributes
            .iter()
            .filter(|attr| attr.path().is_ident("config"))
        {
            attr.parse_nested_meta(|meta| declaration.option(&meta))?;
        }
        Ok(declaration)
    }

    pub(super) fn group(arguments: TokenStream) -> Result<Self> {
        let mut declaration = Self::default();
        syn::meta::parser(|meta| declaration.option(&meta)).parse2(arguments)?;
        Ok(declaration)
    }

    pub(super) fn inherit(mut self, defaults: &Self) -> Self {
        self.role = self.role.or_else(|| defaults.role.clone());
        self.live = self.live.or(defaults.live);
        self.check = self.check.or_else(|| defaults.check.clone());
        self.sdk |= defaults.sdk;
        let construction_override = self.builder.is_some() || self.wrap.is_some();
        if !construction_override {
            self.builder = defaults.builder.clone();
            self.wrap = defaults.wrap.clone();
        }
        self.accessor = self.accessor.or(defaults.accessor);
        self.debug_skipped |= defaults.debug_skipped;
        self.patch = self.patch.or_else(|| defaults.patch.clone());
        self
    }

    fn option(&mut self, meta: &ParseNestedMeta<'_>) -> Result<()> {
        let name = meta
            .path
            .get_ident()
            .ok_or_else(|| meta.error("expected a config field option"))?;
        match name.to_string().as_str() {
            "builder" => {
                if self.builder.replace(group(meta)?).is_some() {
                    return Err(meta.error("duplicate config field attribute group"));
                }
            }
            "patch" => {
                if self.patch.replace(group(meta)?).is_some() {
                    return Err(meta.error("duplicate config field attribute group"));
                }
            }
            "get" => {
                let mode = syn::Ident::parse_any.parse2(group(meta)?)?;
                let accessor = match mode.to_string().as_str() {
                    "ref" => Accessor::Ref,
                    "copy" => Accessor::Copy,
                    "skip" => Accessor::Skip,
                    _ => return Err(meta.error("expected get(ref), get(copy), or get(skip)")),
                };
                if self.accessor.replace(accessor).is_some() {
                    return Err(meta.error("duplicate config getter"));
                }
            }
            "debug" => {
                let mode: Path = syn::parse2(group(meta)?)?;
                if !mode.is_ident("skip") {
                    return Err(meta.error("expected debug(skip)"));
                }
                if self.debug_skipped {
                    return Err(meta.error("duplicate config field attribute group"));
                }
                self.debug_skipped = true;
            }
            "wrap" => {
                if self
                    .wrap
                    .replace(parse_wrap(&meta.path, group(meta)?)?)
                    .is_some()
                {
                    return Err(meta.error("duplicate config field wrap"));
                }
            }
            "live" => {
                let live = if meta.input.peek(syn::token::Paren) {
                    let mode: Path = syn::parse2(group(meta)?)?;
                    if !mode.is_ident("owner") {
                        return Err(meta.error("expected live or live(owner)"));
                    }
                    Live::Owner
                } else {
                    Live::Shared
                };
                if self.live.replace(live).is_some() {
                    return Err(meta.error("duplicate config field option"));
                }
            }
            "check" => {
                if self.check.replace(meta.value()?.parse()?).is_some() {
                    return Err(meta.error("duplicate config field option"));
                }
            }
            "sdk" => {
                if self.sdk {
                    return Err(meta.error("duplicate config field option"));
                }
                self.sdk = true;
                parse_sdk(meta)?;
            }
            _ => {
                if self.role.is_some() {
                    return Err(meta.error("select exactly one config field role"));
                }
                self.role = Some(parse_role(meta)?);
            }
        }
        Ok(())
    }
}

/// Nonempty arguments of one configuration option group.
pub(super) fn group(meta: &ParseNestedMeta<'_>) -> Result<TokenStream> {
    let content;
    parenthesized!(content in meta.input);
    let tokens: TokenStream = content.parse()?;
    if tokens.is_empty() {
        return Err(meta.error("empty config group"));
    }
    Ok(tokens)
}

fn parse_sdk(meta: &ParseNestedMeta<'_>) -> Result<()> {
    if !meta.input.peek(syn::token::Paren) {
        return Ok(());
    }
    let content;
    parenthesized!(content in meta.input);
    let maximum: Meta = content.parse()?;
    let Meta::NameValue(maximum) = maximum else {
        return Err(content.error("expected sdk(max = positive integer)"));
    };
    if !maximum.path.is_ident("max")
        || !content.is_empty()
        || !matches!(&maximum.value, Expr::Lit(expr) if matches!(&expr.lit, Lit::Int(value) if value.base10_parse::<u32>().is_ok_and(|value| value > 0)))
    {
        return Err(content.error("expected sdk(max = positive integer)"));
    }
    Ok(())
}

fn parse_role(meta: &ParseNestedMeta<'_>) -> Result<Role> {
    if meta.path.is_ident("value") {
        if !meta.input.peek(syn::token::Paren) {
            return Ok(Role::Value);
        }
        let content;
        parenthesized!(content in meta.input);
        let ty = content.parse()?;
        content.parse::<syn::Token![,]>()?;
        let expression = content.parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected projection tokens"));
        }
        Ok(Role::Projection(Box::new((ty, expression))))
    } else if meta.path.is_ident("nested") {
        Ok(Role::Nested)
    } else if meta.path.is_ident("skip") {
        let reason: LitStr = meta.value()?.parse()?;
        if reason.value().trim().is_empty() {
            return Err(meta.error("config exclusion requires a reason"));
        }
        Ok(Role::Skip)
    } else {
        Err(meta.error("expected value, value(Type, expression), nested, or skip = reason"))
    }
}

fn parse_wrap(group: &Path, arguments: TokenStream) -> Result<Wrap> {
    let mut default = None;
    let mut with = None;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("default") {
            if default.replace(meta.value()?.parse()?).is_some() {
                return Err(meta.error("duplicate wrap default"));
            }
        } else if meta.path.is_ident("with") {
            if with.replace(meta.value()?.parse()?).is_some() {
                return Err(meta.error("duplicate wrap constructor"));
            }
        } else {
            return Err(meta.error("expected default = expression or with = path"));
        }
        Ok(())
    })
    .parse2(arguments)?;
    let (Some(default), Some(with)) = (default, with) else {
        return Err(syn::Error::new_spanned(
            group,
            "wrap requires default = expression and with = constructor",
        ));
    };
    Ok(Wrap { default, with })
}
