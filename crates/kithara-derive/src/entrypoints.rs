macro_rules! ui_derives {
    () => {
        /// Implements the document-owned size contract for a built-in UI control.
        #[cfg(feature = "control")]
        #[proc_macro_derive(Control, attributes(control))]
        pub fn control(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            ui::control::expand(input)
        }

        /// Implements a draw-only UI painter by forwarding structural arguments.
        #[cfg(feature = "control-painter")]
        #[proc_macro_derive(ControlPainter, attributes(control_painter))]
        pub fn control_painter(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            ui::painter::expand(input)
        }
    };
}

macro_rules! pool_derives {
    () => {
        /// Implements typed construction and key dispatch for a buffer-pool schema.
        ///
        /// The struct keeps its private `PoolSlot<K>` fields; expansion adds a
        /// module-private `builder(OverallBudget)` with one required `PoolConfig`
        /// setter per field and one `HasPool<K>` implementation per field.
        #[cfg(feature = "pool")]
        #[proc_macro_derive(Pool)]
        pub fn pool(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            pool::expand(input)
        }
    };
}

#[cfg(feature = "event")]
macro_rules! event_derives {
    () => {
        /// Implements the marker trait for a concrete event struct or enum.
        #[proc_macro_derive(Event)]
        pub fn event(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            let input = syn::parse_macro_input!(input as syn::DeriveInput);
            event::derive(&input)
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }

        /// Implements a consumer set of concrete event types.
        #[proc_macro_derive(EventSet)]
        pub fn event_set(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            let input = syn::parse_macro_input!(input as syn::DeriveInput);
            event::derive_set(&input)
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }
    };
}

#[cfg(any(feature = "enum-str", feature = "variants"))]
macro_rules! vocabulary_derives {
    () => {
        /// Exposes the declared values of a unit enum in declaration order.
        #[cfg(feature = "variants")]
        #[proc_macro_derive(Variants)]
        pub fn variants(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            vocabulary::variants::expand(input)
        }

        /// Exposes an exhaustive string vocabulary for an enum.
        #[cfg(feature = "enum-str")]
        #[proc_macro_derive(EnumStr, attributes(enum_str))]
        pub fn enum_str(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            vocabulary::enum_str::expand(input)
        }
    };
}
