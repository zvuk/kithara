use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Attribute, DeriveInput, Ident, Result, ext::IdentExt as _};

use super::{
    field::{LiveField, Member},
    implementation::docs,
    live::{cfgs, change_name, spelled},
};
use crate::config::field::Live;

/// `<Name>Control`: on any owner that configures the struct, a getter of each
/// field with an accessor, a `Nested` getter of each nested live field and a
/// setter of each live value field.
pub(super) fn control(item: &DeriveInput, members: &[Member<'_>]) -> Result<TokenStream> {
    let name = &item.ident;
    let visibility = &item.vis;
    let change = change_name(item);
    let control = format_ident!("{}Control", name, span = Span::call_site());
    let configure = quote!(::kithara_config::Configure<#change>);
    let value = format_ident!("__kithara_value");
    let config = format_ident!("__kithara_config");
    let mut taken: Vec<(String, &Ident)> = Vec::new();
    let mut methods: Vec<TokenStream> = Vec::new();
    for member in members {
        let field = member.name;
        let ty = spelled(member.ty, item);
        let surface = docs(member.attributes);
        let cfgs: Vec<&Attribute> = cfgs(member.attributes).collect();
        let mut getter = field.clone();
        getter.set_span(Span::call_site());
        if member.nested && member.live.is_some() {
            claim(&mut taken, &getter, field)?;
            methods.push(quote! {
                #(#cfgs)*
                #(#surface)*
                fn #getter(&self) -> ::kithara_config::Nested<&Self, fn(&#name) -> #ty> {
                    ::kithara_config::__private::nested(self, |#config: &#name| #config.#field)
                }
            });
        } else if member.accessor.is_some() {
            claim(&mut taken, &getter, field)?;
            methods.push(quote! {
                #(#cfgs)*
                #(#surface)*
                fn #getter(&self) -> #ty {
                    <Self as #configure>::settings(self).#field
                }
            });
        }
        let Some(live) = member.live.as_ref().filter(|_| !member.nested) else {
            continue;
        };
        let setter = format_ident!("set_{}", field, span = Span::call_site());
        claim(&mut taken, &setter, field)?;
        let variant = &live.variant;
        let doc = format!(" Hands the owner a change of `{field}` for the nearest moment.");
        methods.push(quote! {
            #(#cfgs)*
            #[doc = #doc]
            ///
            /// # Errors
            ///
            /// Returns the owner's refusal.
            fn #setter(
                &self,
                #value: #ty,
            ) -> ::core::result::Result<<Self as #configure>::Output, <Self as #configure>::Error> {
                <Self as #configure>::configure(
                    self,
                    #change::#variant(#value),
                    ::core::default::Default::default(),
                )
            }
        });
    }
    let subject = format!(" Getters and setters of [`{name}`] on any owner that configures it.");
    Ok(quote! {
        #[doc = #subject]
        #visibility trait #control: ::kithara_config::Configure<#change, Config = #name> {
            #(#methods)*
        }

        #[automatically_derived]
        impl<__KitharaConfigOwner> #control for __KitharaConfigOwner
        where
            __KitharaConfigOwner: ::kithara_config::Configure<#change, Config = #name> + ?Sized,
        {
        }
    })
}

/// `<Name>Exec`: how an owner executes a change, each `live(owner)` field
/// through its own method and every other live field through `exec_live`.
/// The provided `exec` binds prefixed names, so a constant in scope never
/// turns them into patterns.
pub(super) fn exec(
    item: &DeriveInput,
    fields: &[(&Member<'_>, &LiveField)],
) -> Result<TokenStream> {
    let name = &item.ident;
    let visibility = &item.vis;
    let change = change_name(item);
    let exec = format_ident!("{}Exec", name, span = Span::call_site());
    let cx = Ident::new("__KitharaCx", Span::call_site());
    let received = format_ident!("__kithara_change");
    let moment = format_ident!("__kithara_at");
    let context = format_ident!("__kithara_cx");
    let value = format_ident!("__kithara_value");
    let exec_live = Ident::new("exec_live", Span::call_site());
    let mut taken: Vec<(String, &Ident)> = Vec::new();
    let mut methods: Vec<TokenStream> = Vec::new();
    let mut arms: Vec<TokenStream> = Vec::new();
    let mut shared = false;
    for (member, live) in fields {
        let variant = &live.variant;
        let field = member.name;
        let ty = spelled(member.ty, item);
        let cfgs: Vec<&Attribute> = cfgs(member.attributes).collect();
        if matches!(live.mode, Live::Owner) {
            let method = format_ident!("exec_{}", field, span = Span::call_site());
            claim(&mut taken, &method, field)?;
            let doc = format!(" Executes a change of `{field}` at `at`.");
            methods.push(quote! {
                #(#cfgs)*
                #[doc = #doc]
                fn #method(&mut self, value: #ty, at: Self::At, cx: &mut #cx) -> Self::Output;
            });
            arms.push(quote! {
                #(#cfgs)*
                #change::#variant(#value) => self.#method(#value, #moment, #context)
            });
        } else {
            if !shared {
                claim(&mut taken, &exec_live, field)?;
            }
            shared = true;
            arms.push(quote! {
                #(#cfgs)*
                #change::#variant(_) => self.#exec_live(#received, #moment, #context)
            });
        }
    }
    let exec_live = shared.then(|| {
        quote! {
            /// Executes a change of a live field that is not `live(owner)`.
            fn #exec_live(&mut self, change: #change, at: Self::At, cx: &mut #cx) -> Self::Output;
        }
    });
    let subject = format!(" How an owner executes each change of [`{name}`].");
    Ok(quote! {
        #[doc = #subject]
        #visibility trait #exec<#cx: ?Sized> {
            /// When a change executes.
            type At;
            /// What executing a change yields.
            type Output;

            /// Executes one change through the method of its field.
            fn exec(&mut self, #received: #change, #moment: Self::At, #context: &mut #cx) -> Self::Output {
                match #received {
                    #(#arms,)*
                }
            }

            #(#methods)*

            #exec_live
        }
    })
}

/// Records that `field` generates the method `method`, refusing a method an
/// earlier field already generates.
fn claim<'a>(taken: &mut Vec<(String, &'a Ident)>, method: &Ident, field: &'a Ident) -> Result<()> {
    let method = method.unraw().to_string();
    if let Some((_, first)) = taken.iter().find(|(other, _)| *other == method) {
        let message = format!("fields `{first}` and `{field}` both generate the method `{method}`");
        return Err(syn::Error::new_spanned(field, message));
    }
    taken.push((method, field));
    Ok(())
}
