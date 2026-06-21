use proc_macro2::TokenStream as TokenStream2;
use syn::{
    Error, Expr, Ident, Token,
    parse::{Parse, ParseStream, Parser},
    punctuated::Punctuated,
};

/// Parsed `#[kithara::probe(...)]` arguments: parameter idents, computed
/// `name = expr` values, and the `caller` / `probe_return` flags. See the
/// crate `CONTEXT.md` "`#[kithara::probe(...)]` arguments" for the syntax.
#[derive(Default, Debug)]
pub(crate) struct ProbeFilter {
    pub args: Option<Vec<Ident>>,
    pub computed: Vec<(Ident, Expr)>,
    pub caller: bool,
    pub probe_return: bool,
}

/// One entry inside `#[kithara::probe(...)]`. Either a bare ident
/// (parameter name or keyword flag) or `name = expr` (computed value).
///
/// `expr` is boxed: `syn::Expr` is large (~256 bytes) and would
/// otherwise dominate the enum size for every entry.
enum ProbeArg {
    Plain(Ident),
    Computed { name: Ident, expr: Box<Expr> },
}

impl Parse for ProbeArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        if input.peek(Token![=]) {
            let _eq: Token![=] = input.parse()?;
            let expr: Expr = input.parse()?;
            Ok(Self::Computed {
                name,
                expr: Box::new(expr),
            })
        } else {
            Ok(Self::Plain(name))
        }
    }
}

pub(crate) fn parse_filter(attr: TokenStream2) -> syn::Result<ProbeFilter> {
    if attr.is_empty() {
        return Ok(ProbeFilter::default());
    }
    let parser = Punctuated::<ProbeArg, Token![,]>::parse_terminated;
    let parsed = parser.parse2(attr)?;
    let mut filter = ProbeFilter::default();
    let mut args: Vec<Ident> = Vec::new();
    for entry in parsed {
        match entry {
            ProbeArg::Plain(ident) => {
                if ident == "probe_return" {
                    filter.probe_return = true;
                } else if ident == "caller" {
                    filter.caller = true;
                } else {
                    args.push(ident);
                }
            }
            ProbeArg::Computed { name, expr } => {
                if name == "probe_return" || name == "caller" {
                    return Err(Error::new_spanned(
                        &name,
                        format!(
                            "#[kithara::probe(...)] `{name}` is a reserved keyword \
                             and cannot be used as the name of a `name = expr` entry"
                        ),
                    ));
                }
                filter.computed.push((name, *expr));
            }
        }
    }
    if !args.is_empty() {
        filter.args = Some(args);
    }
    Ok(filter)
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::parse_filter;

    #[test]
    fn empty_attr_yields_default_filter() {
        let f = parse_filter(quote!()).unwrap();
        assert!(f.args.is_none());
        assert!(f.computed.is_empty());
        assert!(!f.probe_return);
        assert!(!f.caller);
    }

    #[test]
    fn plain_idents_only() {
        let f = parse_filter(quote!(variant, budget)).unwrap();
        let args = f.args.expect("plain idents present");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].to_string(), "variant");
        assert_eq!(args[1].to_string(), "budget");
        assert!(f.computed.is_empty());
    }

    #[test]
    fn keyword_flags_recognised() {
        let f = parse_filter(quote!(probe_return)).unwrap();
        assert!(f.probe_return);
        assert!(!f.caller);
        assert!(f.args.is_none());

        let f = parse_filter(quote!(caller, variant)).unwrap();
        assert!(f.caller);
        assert_eq!(f.args.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn computed_name_eq_expr_recognised() {
        let f = parse_filter(quote!(variant, queue_len = self.queue.len())).unwrap();
        assert_eq!(f.args.as_ref().unwrap().len(), 1);
        assert_eq!(f.computed.len(), 1);
        assert_eq!(f.computed[0].0.to_string(), "queue_len");
    }

    #[test]
    fn multiple_computed_mixed_with_plain() {
        let f = parse_filter(quote!(
            variant,
            queue_len = self.queue.len(),
            init_state = self.init.state as u32
        ))
        .unwrap();
        let args = f.args.expect("plain idents present");
        assert_eq!(args.len(), 1);
        assert_eq!(f.computed.len(), 2);
        assert_eq!(f.computed[0].0.to_string(), "queue_len");
        assert_eq!(f.computed[1].0.to_string(), "init_state");
    }

    #[test]
    fn computed_keyword_name_is_rejected() {
        for keyword in ["probe_return", "caller"] {
            let input: proc_macro2::TokenStream = format!("{keyword} = self.foo")
                .parse()
                .expect("valid tokens");
            let err = parse_filter(input).expect_err("reserved keyword");
            let msg = err.to_string();
            assert!(
                msg.contains(keyword),
                "case {keyword}: missing keyword in {msg}"
            );
            assert!(
                msg.contains("reserved keyword"),
                "case {keyword}: missing 'reserved keyword' in {msg}"
            );
        }
    }
}
