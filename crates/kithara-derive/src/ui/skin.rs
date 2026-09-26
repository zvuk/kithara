use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, parse_macro_input};

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn derive(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "SkinWalk can only be derived for structs with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "SkinWalk requires named fields",
        ));
    };

    let mut frame_walks = TokenStream2::new();
    let mut role_walks = TokenStream2::new();
    for field in &fields.named {
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| Error::new_spanned(field, "SkinWalk requires named fields"))?;
        let mut skip_frames = false;
        let mut skip_roles = false;
        for attribute in field
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("skin"))
        {
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip_frames") {
                    skip_frames = true;
                    Ok(())
                } else if meta.path.is_ident("skip_roles") {
                    skip_roles = true;
                    Ok(())
                } else {
                    Err(meta.error("expected `skip_frames` or `skip_roles`"))
                }
            })?;
        }
        if !skip_frames {
            frame_walks.extend(quote! {
                crate::doc::skin::blanket::Frames::each_frame(&mut self.#ident, visit);
            });
        }
        if !skip_roles {
            role_walks.extend(quote! {
                crate::doc::skin::blanket::Roles::each_role(&mut self.#ident, visit);
            });
        }
    }

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics crate::doc::skin::blanket::Frames for #name #ty_generics #where_clause {
            fn each_frame(
                &mut self,
                visit: &mut dyn FnMut(&mut crate::doc::skin::primitives::FrameSkin),
            ) {
                #frame_walks
            }
        }

        impl #impl_generics crate::doc::skin::blanket::Roles for #name #ty_generics #where_clause {
            fn each_role(
                &mut self,
                visit: &mut dyn FnMut(&mut crate::doc::skin::primitives::TextRoleSkin),
            ) {
                #role_walks
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use quote::quote;
    use syn::{DeriveInput, parse_quote};

    use super::derive;

    #[kithara::test(native, flash(false))]
    fn walks_fields_in_declaration_order_and_skips_each_domain_explicitly() -> syn::Result<()> {
        let input: DeriveInput = parse_quote! {
            struct Section {
                frame: FrameSkin,
                #[skin(skip_roles)]
                frame_only: FrameSkin,
                #[skin(skip_frames)]
                role_only: TextRoleSkin,
            }
        };

        let output = derive(&input)?.to_string();
        let frame = quote!(self.frame).to_string();
        let frame_only = quote!(self.frame_only).to_string();
        let role_only = quote!(self.role_only).to_string();
        assert!(
            output
                .find(&frame)
                .is_some_and(|first| output[first..].contains(&frame_only))
        );
        assert_eq!(output.matches(&frame_only).count(), 1);
        assert_eq!(output.matches(&role_only).count(), 1);
        Ok(())
    }

    #[kithara::test(native, flash(false))]
    fn refuses_non_structs() {
        let input: DeriveInput = parse_quote! { enum Section { A } };
        assert!(derive(&input).is_err());
    }
}
