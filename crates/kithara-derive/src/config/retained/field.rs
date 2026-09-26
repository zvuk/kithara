use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Expr, Field, GenericParam, Generics, Lit, LitStr, Meta, Result, Token, Type, parenthesized,
    parse::Parser as _, punctuated::Punctuated, visit::Visit as _,
};

use super::implementation::attributes;

enum Role {
    Value,
    Projection(Box<(Type, Expr)>),
    Nested,
    Skip,
}

pub(super) struct Expanded {
    pub(super) declaration: TokenStream,
    pub(super) read: TokenStream,
    pub(super) update: Option<Update>,
}

pub(super) struct Update {
    pub(super) declaration: TokenStream,
    pub(super) field: TokenStream,
    pub(super) lower: TokenStream,
}

pub(super) fn expand(
    field: &mut Field,
    generics: &Generics,
    owner: &syn::Ident,
    snapshot: bool,
) -> Result<Option<Expanded>> {
    let mut role = None;
    let mut update = false;
    let mut sdk = false;
    let mut preserved: Vec<syn::Attribute> = Vec::new();
    let mut forwarded: Vec<syn::Path> = Vec::new();
    for attr in &field.attrs {
        if !attr.path().is_ident("config") {
            preserved.push(attr.clone());
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("builder")
                || meta.path.is_ident("field")
                || meta.path.is_ident("patch")
            {
                if forwarded.contains(&meta.path) {
                    return Err(meta.error("duplicate config field attribute group"));
                }
                let content;
                parenthesized!(content in meta.input);
                let arguments: TokenStream = content.parse()?;
                if arguments.is_empty() {
                    return Err(meta.error("config field attribute group cannot be empty"));
                }
                Punctuated::<Meta, Token![,]>::parse_terminated.parse2(arguments.clone())?;
                let path = &meta.path;
                preserved.push(syn::parse_quote!(#[#path(#arguments)]));
                forwarded.push(meta.path.clone());
                return Ok(());
            }
            if meta.path.is_ident("update") {
                if update {
                    return Err(meta.error("duplicate config field option"));
                }
                update = true;
                return Ok(());
            }
            if meta.path.is_ident("sdk") {
                if sdk {
                    return Err(meta.error("duplicate config field option"));
                }
                sdk = true;
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
                return Ok(());
            }
            if role.is_some() {
                return Err(meta.error("select exactly one config field role"));
            }
            role = Some(if meta.path.is_ident("value") {
                if meta.input.peek(syn::token::Paren) {
                    let content;
                    parenthesized!(content in meta.input);
                    let ty = content.parse()?;
                    content.parse::<syn::Token![,]>()?;
                    let expression = content.parse()?;
                    if !content.is_empty() {
                        return Err(content.error("unexpected projection tokens"));
                    }
                    Role::Projection(Box::new((ty, expression)))
                } else {
                    Role::Value
                }
            } else if meta.path.is_ident("nested") {
                Role::Nested
            } else if meta.path.is_ident("skip") {
                let reason: LitStr = meta.value()?.parse()?;
                if reason.value().trim().is_empty() {
                    return Err(meta.error("config exclusion requires a reason"));
                }
                Role::Skip
            } else {
                return Err(meta.error(
                    "expected value, value(Type, expression), nested, skip = reason, or update",
                ));
            });
            Ok(())
        })?;
    }
    for path in &forwarded {
        if field.attrs.iter().any(|attr| attr.path() == path) {
            return Err(syn::Error::new_spanned(
                path,
                "choose either a native field attribute or its config group",
            ));
        }
    }
    let role = role.ok_or_else(|| {
        syn::Error::new_spanned(
            &*field,
            "classify each config field as value, nested, or skip = reason",
        )
    })?;
    validate_role(field, &role, update, sdk, snapshot, &preserved)?;
    field.attrs = preserved;
    if !snapshot {
        return Ok(None);
    }
    let name = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(&*field, "config requires named fields"))?;
    let original_type = &field.ty;
    let (ty, expression): (Type, Expr) = match role {
        Role::Skip => return Ok(None),
        Role::Value => (
            original_type.clone(),
            syn::parse_quote!(::core::clone::Clone::clone(&self.#name)),
        ),
        Role::Nested => (
            syn::parse_quote!(<#original_type as ::kithara_config::Config>::Values),
            syn::parse_quote!(::kithara_config::Config::values(&self.#name)),
        ),
        Role::Projection(projection) => *projection,
    };
    let mut usage = GenericUse {
        generics,
        found: false,
    };
    usage.visit_type(&ty);
    if usage.found {
        return Err(syn::Error::new_spanned(
            ty,
            "snapshot types cannot depend on resource generics; use value(OwnedType, expression)",
        ));
    }
    let gates = attributes(&field.attrs, false)?;
    let surface = attributes(&field.attrs, true)?;
    let update = update
        .then(|| update_tokens(field, owner, name, original_type, &surface, &gates))
        .transpose()?;
    Ok(Some(Expanded {
        declaration: quote! { #(#surface)* pub #name: #ty },
        read: quote! { #(#gates)* #name: #expression },
        update,
    }))
}

fn validate_role(
    field: &Field,
    role: &Role,
    update: bool,
    sdk: bool,
    snapshot: bool,
    preserved: &[syn::Attribute],
) -> Result<()> {
    if sdk && !matches!(role, Role::Value | Role::Projection(_)) {
        return Err(syn::Error::new_spanned(
            field,
            "SDK exposure requires a value or projected value field",
        ));
    }
    if update && !matches!(role, Role::Value) {
        return Err(syn::Error::new_spanned(
            field,
            "runtime update currently requires a retained value field",
        ));
    }
    if update && !snapshot {
        return Err(syn::Error::new_spanned(
            field,
            "construction inputs cannot declare retained runtime updates",
        ));
    }
    if !snapshot && matches!(role, Role::Projection(_)) {
        return Err(syn::Error::new_spanned(
            field,
            "construction inputs do not produce projected values",
        ));
    }
    if update {
        for attribute in preserved {
            if patch_skips(attribute)? {
                return Err(syn::Error::new_spanned(
                    field,
                    "runtime update cannot use patch(skip): the generated Patch field is absent",
                ));
            }
        }
    }
    Ok(())
}

fn patch_skips(attribute: &syn::Attribute) -> Result<bool> {
    if !attribute.path().is_ident("patch") {
        return Ok(false);
    }
    let options = attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    Ok(options.iter().any(|option| option.path().is_ident("skip")))
}

fn update_tokens(
    field: &Field,
    owner: &syn::Ident,
    name: &syn::Ident,
    ty: &Type,
    surface: &[syn::Attribute],
    gates: &[syn::Attribute],
) -> Result<Update> {
    let enum_name = format_ident!("{}{}Update", owner, upper_camel(name));
    let optional = option_inner(ty);
    let payload = optional.unwrap_or(ty);
    let default = builder_default(field)?;
    let clear = optional.map(|_| quote! { Clear, });
    let reset = default.as_ref().map(|_| quote! { Reset, });
    let set = if optional.is_some() {
        quote! { patch.#name = ::core::option::Option::Some(::core::option::Option::Some(value)); }
    } else {
        quote! { patch.#name = ::core::option::Option::Some(value); }
    };
    let clear_lower = optional.map(|_| {
        quote! { #enum_name::Clear => { patch.#name = ::core::option::Option::Some(::core::option::Option::None); } }
    });
    let reset_lower = default.map(|default| {
        quote! { #enum_name::Reset => { patch.#name = ::core::option::Option::Some(#default); } }
    });
    Ok(Update {
        declaration: quote! {
            #(#surface)*
            #[derive(::core::default::Default)]
            #[non_exhaustive]
            pub enum #enum_name {
                #[default]
                Unchanged,
                Set { value: #payload },
                #clear
                #reset
            }
        },
        field: quote! { #(#surface)* pub #name: #enum_name },
        lower: quote! {
            #(#gates)*
            match update.#name {
                #enum_name::Unchanged => {}
                #enum_name::Set { value } => { #set }
                #clear_lower
                #reset_lower
            }
        },
    })
}

fn builder_default(field: &Field) -> Result<Option<Expr>> {
    let mut default = None;
    for attribute in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("builder"))
    {
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("default") {
                return Ok(());
            }
            if default.is_some() {
                return Err(meta.error("duplicate builder default"));
            }
            default = Some(if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse()?
            } else {
                syn::parse_quote!(::core::default::Default::default())
            });
            Ok(())
        })?;
    }
    Ok(default)
}

fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    match arguments.args.first()? {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn upper_camel(ident: &syn::Ident) -> String {
    ident
        .to_string()
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(char::to_uppercase)
                .into_iter()
                .flatten()
                .chain(chars)
                .collect::<String>()
        })
        .collect()
}

struct GenericUse<'a> {
    generics: &'a Generics,
    found: bool,
}

impl<'ast> syn::visit::Visit<'ast> for GenericUse<'_> {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.found |= self
            .generics
            .params
            .iter()
            .any(|parameter| match parameter {
                GenericParam::Type(parameter) => parameter.ident == *ident,
                GenericParam::Const(parameter) => parameter.ident == *ident,
                GenericParam::Lifetime(parameter) => parameter.lifetime.ident == *ident,
            });
    }
}
