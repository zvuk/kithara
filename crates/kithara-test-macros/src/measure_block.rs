use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Expr, Token, parse::Parse};

struct Input {
    label: Expr,
    _comma: Token![,],
    expression: Expr,
}

impl Parse for Input {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            label: input.parse()?,
            _comma: input.parse()?,
            expression: input.parse()?,
        })
    }
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    match syn::parse::<Input>(input) {
        Ok(input) => expand_parsed(input).into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand_parsed(input: Input) -> TokenStream2 {
    let Input {
        label, expression, ..
    } = input;
    quote! {{
        #[cfg(feature = "perf")]
        {
            hotpath::measure_block!(#label, #expression)
        }
        #[cfg(not(feature = "perf"))]
        {
            #expression
        }
    }}
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::{Input, expand_parsed};

    #[test]
    fn gates_the_measured_and_plain_expression() {
        let input = syn::parse2::<Input>(quote!("audio.read", read())).expect("valid input");
        let expanded = expand_parsed(input).to_string();

        assert!(expanded.contains("feature = \"perf\""));
        assert!(expanded.contains("hotpath :: measure_block"));
        assert_eq!(expanded.matches("read ()").count(), 2);
    }
}
