use proc_macro2::{Group, Span, TokenStream, TokenTree};
use quote::{ToTokens, format_ident, quote};
use syn::{Attribute, DeriveInput, Ident, Result, Type};

use super::{
    control::{control, exec},
    field::{LiveField, Member},
    implementation::docs,
};
use crate::config::field::Live;

/// `CheckedConfig` whenever the struct declares a check error or a live field,
/// then the change enum, `LiveConfig`, `<Name>Control` and `<Name>Exec` when it
/// has a live field.
pub(super) fn expand(
    item: &DeriveInput,
    members: &[Member<'_>],
    error: Option<&Type>,
) -> Result<Option<TokenStream>> {
    let fields: Vec<(&Member<'_>, &LiveField)> = members
        .iter()
        .filter_map(|member| member.live.as_ref().map(|live| (member, live)))
        .collect();
    if error.is_none() && fields.is_empty() {
        return Ok(None);
    }
    let checked = checked(item, members, error);
    let live = if fields.is_empty() {
        None
    } else {
        let change = live(item, &fields);
        let control = control(item, members)?;
        let exec = exec(item, &fields)?;
        Some(quote! { #change #control #exec })
    };
    Ok(Some(quote! { #checked #live }))
}

/// The `cfg` attributes of a field, which every statement generated from it
/// carries.
pub(super) fn cfgs(attributes: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("cfg"))
}

/// The change enum's name, spanned at the derive so generated items read as
/// the macro's own.
pub(super) fn change_name(item: &DeriveInput) -> Ident {
    format_ident!("{}Change", item.ident, span = Span::call_site())
}

/// A field type with `Self` spelled as the struct and its generics, for items
/// in which `Self` names the change enum, an owner or the snapshot.
pub(super) fn spelled(ty: &Type, item: &DeriveInput) -> TokenStream {
    let name = &item.ident;
    let (_, ty_generics, _) = item.generics.split_for_impl();
    respell(ty.to_token_stream(), &quote!(#name #ty_generics))
}

fn respell(tokens: TokenStream, config: &TokenStream) -> TokenStream {
    tokens
        .into_iter()
        .flat_map(|token| match token {
            TokenTree::Ident(ident) if ident == "Self" => config.clone(),
            TokenTree::Group(group) => {
                let mut respelled = Group::new(group.delimiter(), respell(group.stream(), config));
                respelled.set_span(group.span());
                TokenTree::Group(respelled).into()
            }
            other => other.into(),
        })
        .collect()
}

fn checked(item: &DeriveInput, members: &[Member<'_>], error: Option<&Type>) -> TokenStream {
    let name = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    let error = error.map_or_else(
        || quote!(::core::convert::Infallible),
        |error| quote!(#error),
    );
    let steps: Vec<TokenStream> = members
        .iter()
        .filter_map(|member| {
            let field = member.name;
            let cfgs = cfgs(member.attributes);
            if member.nested {
                Some(quote! {
                    #(#cfgs)*
                    self.#field = ::kithara_config::CheckedConfig::validated(self.#field)?;
                })
            } else {
                member.check.as_ref().map(|check| {
                    quote! {
                        #(#cfgs)*
                        self.#field = #check(self.#field)?;
                    }
                })
            }
        })
        .collect();
    let receiver = if steps.is_empty() {
        quote!(self)
    } else {
        quote!(mut self)
    };
    quote! {
        #[automatically_derived]
        impl #impl_generics ::kithara_config::CheckedConfig for #name #ty_generics #where_clause {
            type Error = #error;

            fn validated(#receiver) -> ::core::result::Result<Self, Self::Error> {
                #(#steps)*
                ::core::result::Result::Ok(self)
            }
        }
    }
}

/// The change enum and `LiveConfig`. Their bindings carry a prefix no caller
/// names, so neither a field check path nor a constant in scope meets them.
fn live(item: &DeriveInput, fields: &[(&Member<'_>, &LiveField)]) -> TokenStream {
    let name = &item.ident;
    let visibility = &item.vis;
    let change = change_name(item);
    let received = format_ident!("__kithara_change");
    let value = format_ident!("__kithara_value");
    let mut variants: Vec<TokenStream> = Vec::new();
    let mut checks: Vec<TokenStream> = Vec::new();
    let mut applies: Vec<TokenStream> = Vec::new();
    let mut nested: Vec<TokenStream> = Vec::new();
    for (member, live) in fields {
        let variant = &live.variant;
        let field = member.name;
        let ty = spelled(member.ty, item);
        let payload = if member.nested {
            quote!(<#ty as ::kithara_config::LiveConfig>::Change)
        } else {
            quote!(#ty)
        };
        let surface = docs(member.attributes);
        let cfgs: Vec<&Attribute> = cfgs(member.attributes).collect();
        variants.push(quote! { #(#surface)* #variant(#payload) });
        if member.nested {
            checks.push(quote! {
                #(#cfgs)*
                #change::#variant(#value) => ::core::result::Result::Ok(#change::#variant(
                    <#ty as ::kithara_config::LiveConfig>::check(#value)?
                ))
            });
            applies.push(quote! {
                #(#cfgs)*
                #change::#variant(#value) => ::kithara_config::LiveConfig::apply_change(&mut self.#field, #value)
            });
            nested.push(quote! {
                #(#cfgs)*
                #[automatically_derived]
                impl ::core::convert::From<#payload> for #change {
                    fn from(#value: #payload) -> Self {
                        Self::#variant(#value)
                    }
                }
                #(#cfgs)*
                const _: () = ::core::assert!(
                    !<#ty as ::kithara_config::LiveConfig>::OWNER_FIELDS,
                    "a nested live config cannot have live(owner) fields"
                );
            });
        } else {
            let checked = member
                .check
                .as_ref()
                .map_or_else(|| quote!(#value), |check| quote!(#check(#value)?));
            checks.push(quote! {
                #(#cfgs)*
                #change::#variant(#value) => ::core::result::Result::Ok(#change::#variant(#checked))
            });
            applies.push(quote! {
                #(#cfgs)*
                #change::#variant(#value) => self.#field = #value
            });
        }
    }
    let owner_fields = fields
        .iter()
        .any(|(_, live)| matches!(live.mode, Live::Owner));
    let subject = format!(" One change of one live field of [`{name}`].");
    quote! {
        #[doc = #subject]
        #[derive(::core::clone::Clone, ::core::marker::Copy, ::core::fmt::Debug)]
        #visibility enum #change {
            #(#variants,)*
        }

        #(#nested)*

        #[automatically_derived]
        impl ::kithara_config::LiveConfig for #name {
            type Change = #change;

            const OWNER_FIELDS: bool = #owner_fields;

            fn apply_change(&mut self, #received: Self::Change) {
                match #received {
                    #(#applies,)*
                }
            }

            fn check(
                #received: Self::Change,
            ) -> ::core::result::Result<Self::Change, <Self as ::kithara_config::CheckedConfig>::Error> {
                match #received {
                    #(#checks,)*
                }
            }
        }
    }
}
