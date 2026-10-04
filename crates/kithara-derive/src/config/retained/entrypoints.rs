macro_rules! config_derives {
    () => {
        /// `#[derive(Config)]` — the builder, accessors, retained snapshot, field checks
        /// and live field changes of a configuration struct, all declared through
        /// `#[config(...)]`.
        #[proc_macro_derive(Config, attributes(config))]
        pub fn config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            config::retained::expand(input.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }

        /// Implements `ConfigOwner` by borrowing the named retained configuration field.
        #[proc_macro_derive(ConfigOwner, attributes(config_owner, config_owner_mut))]
        pub fn config_owner(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            config::owner::expand(input.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }
    };
}

pub(crate) use config_derives;
