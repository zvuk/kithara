use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Attribute, DeriveInput, Expr, Field, GenericParam, Generics, Ident, Meta, Path, Result, Token,
    Type, ext::IdentExt as _, parse::Parser as _, punctuated::Punctuated, visit::Visit as _,
};

use super::{implementation::docs, live::spelled};
use crate::config::field::{Accessor, Declaration, Live, Role, Wrap};

/// What one field contributes to its configuration type.
pub(super) struct Member<'a> {
    pub(super) name: &'a Ident,
    pub(super) ty: &'a Type,
    pub(super) construction: Construction,
    pub(super) accessor: Option<TokenStream>,
    pub(super) owner_accessor: Option<TokenStream>,
    /// Whether the generated `Debug` prints the field.
    pub(super) debugged: bool,
    pub(super) retained: Option<Retained>,
    pub(super) live: Option<LiveField>,
    pub(super) check: Option<Path>,
    /// Whether the field holds a nested configuration.
    pub(super) nested: bool,
    pub(super) attributes: &'a [Attribute],
}

/// How the builder fills the field.
pub(super) enum Construction {
    /// An argument of the builder's `new`, with its docs and bon options.
    Argument(TokenStream),
    /// `builder(skip)`: no setter; `new` initialises the field itself, because
    /// bon has no `skip` for a function argument.
    Initialised(Expr),
}

/// How a live field changes.
pub(super) struct LiveField {
    pub(super) mode: Live,
    /// The field's variant in the change enum.
    pub(super) variant: Ident,
}

pub(super) struct Retained {
    pub(super) declaration: TokenStream,
    pub(super) read: TokenStream,
}

pub(super) fn expand<'a>(
    field: &'a Field,
    owner: &DeriveInput,
    snapshot: bool,
    defaults: &Declaration,
) -> Result<Member<'a>> {
    let Declaration {
        role,
        live,
        check,
        sdk,
        mut builder,
        accessor,
        debug_skipped,
        wrap,
        patch: _,
    } = Declaration::parse(&field.attrs)?.inherit(defaults);
    let role = role
        .or_else(|| (!snapshot).then_some(Role::Skip))
        .ok_or_else(|| syn::Error::new_spanned(field, "missing config field role"))?;
    validate_role(field, &role, sdk, snapshot)?;
    if !snapshot && (live.is_some() || check.is_some()) {
        return Err(syn::Error::new_spanned(
            field,
            "construction inputs cannot declare live fields or checks",
        ));
    }
    validate_live(field, &role, live, check.is_some())?;
    let live = live
        .map(|mode| change_variant(field).map(|variant| LiveField { mode, variant }))
        .transpose()?;
    let nested = matches!(role, Role::Nested);
    let name = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(field, "config requires named fields"))?;
    let ty = &field.ty;
    let surface = docs(&field.attrs);
    if let Some(wrap) = wrap {
        if builder.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "wrap replaces the builder group",
            ));
        }
        let Role::Projection(projection) = &role else {
            return Err(syn::Error::new_spanned(
                field,
                "wrap requires value(Type, expression)",
            ));
        };
        let Wrap { default, with } = wrap;
        let wire = &projection.0;
        builder = Some(quote!(default = #with(#default), with = |value: #wire| #with(value)));
    }
    let construction = construction(name, ty, &surface, builder.as_ref())?;
    let accessor = accessor.filter(|mode| !matches!(mode, Accessor::Skip));
    let owner_accessor = accessor.map(|mode| {
        let copy = matches!(mode, Accessor::Copy);
        let ty = spelled(ty, owner);
        let output = if copy { quote!(#ty) } else { quote!(&#ty) };
        quote! {
            #(#surface)*
            fn #name(&self) -> #output {
                self.config().#name()
            }
        }
    });
    let accessor = accessor.map(|mode| {
        let copy = matches!(mode, Accessor::Copy);
        let (output, body) = if copy {
            (quote!(#ty), quote!(self.#name))
        } else {
            (quote!(&#ty), quote!(&self.#name))
        };
        let visibility = &owner.vis;
        quote! {
            #(#surface)*
            #visibility fn #name(&self) -> #output {
                #body
            }
        }
    });
    let retained = if snapshot {
        retained(field, owner, name, role)?
    } else {
        None
    };
    Ok(Member {
        name,
        ty,
        construction,
        accessor,
        owner_accessor,
        debugged: !debug_skipped,
        retained,
        live,
        check,
        nested,
        attributes: &field.attrs,
    })
}

/// Reads a `builder(...)` group: `skip` or `skip = expr` keeps the field out
/// of the builder, anything else is bon's option list for the argument.
fn construction(
    name: &Ident,
    ty: &Type,
    surface: &[&Attribute],
    builder: Option<&TokenStream>,
) -> Result<Construction> {
    let Some(builder) = builder else {
        return Ok(Construction::Argument(quote! { #(#surface)* #name: #ty }));
    };
    let options = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(builder.clone())?;
    let Some(skip) = options.iter().find(|option| option.path().is_ident("skip")) else {
        return Ok(Construction::Argument(
            quote! { #(#surface)* #[builder(#builder)] #name: #ty },
        ));
    };
    if options.len() > 1 {
        return Err(syn::Error::new_spanned(
            skip,
            "builder(skip) takes no other builder options",
        ));
    }
    Ok(Construction::Initialised(match skip {
        Meta::Path(_) => syn::parse_quote!(::core::default::Default::default()),
        Meta::NameValue(skip) => skip.value.clone(),
        Meta::List(_) => {
            return Err(syn::Error::new_spanned(
                skip,
                "expected builder(skip) or builder(skip = expression)",
            ));
        }
    }))
}

fn retained(
    field: &Field,
    owner: &DeriveInput,
    name: &Ident,
    role: Role,
) -> Result<Option<Retained>> {
    let original_type = &field.ty;
    let (ty, expression): (Type, Expr) = match role {
        Role::Skip => return Ok(None),
        Role::Value => (
            syn::parse2(spelled(original_type, owner))?,
            syn::parse_quote!(::core::clone::Clone::clone(&self.#name)),
        ),
        Role::Nested => (
            syn::parse_quote!(<#original_type as ::kithara_config::Config>::Values),
            syn::parse_quote!(::kithara_config::Config::values(&self.#name)),
        ),
        Role::Projection(projection) => *projection,
    };
    if names_a_parameter(&ty, &owner.generics) {
        return Err(syn::Error::new_spanned(
            ty,
            "snapshot types cannot depend on resource generics; use value(OwnedType, expression)",
        ));
    }
    let surface = docs(&field.attrs);
    Ok(Some(Retained {
        declaration: quote! { #(#surface)* pub #name: #ty },
        read: quote! { #name: #expression },
    }))
}

fn validate_role(field: &Field, role: &Role, sdk: bool, snapshot: bool) -> Result<()> {
    let refuse = |message: &str| Err(syn::Error::new_spanned(field, message));
    if sdk && !matches!(role, Role::Value | Role::Projection(_)) {
        return refuse("SDK exposure requires a value or projected value field");
    }
    if !snapshot && matches!(role, Role::Projection(_)) {
        return refuse("construction inputs do not produce projected values");
    }
    Ok(())
}

fn validate_live(field: &Field, role: &Role, live: Option<Live>, checked: bool) -> Result<()> {
    let refuse = |message: &str| Err(syn::Error::new_spanned(field, message));
    if checked && !matches!(role, Role::Value) {
        return refuse("check requires a value field");
    }
    match (live, role) {
        (Some(_), Role::Skip | Role::Projection(_)) => {
            return refuse("live requires a value or nested field");
        }
        (Some(Live::Owner), Role::Nested) => return refuse("live(owner) requires a value field"),
        _ => {}
    }
    Ok(())
}

/// The change enum's variant for `field`: its name in upper camel case, without
/// a raw identifier's prefix.
fn change_variant(field: &Field) -> Result<Ident> {
    let refusal = || {
        syn::Error::new_spanned(
            field,
            "a live field needs a name that forms a change variant",
        )
    };
    let name = field.ident.as_ref().ok_or_else(refusal)?;
    syn::parse_str(&upper_camel(&name.unraw())).map_err(|_| refusal())
}

fn upper_camel(ident: &Ident) -> String {
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

/// Whether `ty` names one of the type's generic parameters.
pub(super) fn names_a_parameter(ty: &Type, generics: &Generics) -> bool {
    let mut usage = GenericUse {
        generics,
        found: false,
    };
    usage.visit_type(ty);
    usage.found
}

struct GenericUse<'a> {
    generics: &'a Generics,
    found: bool,
}

impl<'ast> syn::visit::Visit<'ast> for GenericUse<'_> {
    fn visit_ident(&mut self, ident: &'ast Ident) {
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
