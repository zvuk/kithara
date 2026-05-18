use syn::{
    Error, Expr, Ident, LitStr, Token, Type,
    parse::{Parse, ParseStream},
};

pub(crate) struct WatchdogArgs {
    pub(crate) ctx: Option<Type>,
    pub(crate) dump_dir: Option<Expr>,
    pub(crate) name: Option<LitStr>,
    pub(crate) timeout: Option<Expr>,
}

fn unknown_attr_err(ident: &Ident) -> Error {
    Error::new(
        ident.span(),
        format!("unknown attribute `{ident}`, expected `name`, `timeout`, `ctx`, or `dump_dir`"),
    )
}

fn parse_field(ident: &Ident, input: ParseStream<'_>, out: &mut WatchdogArgs) -> syn::Result<()> {
    match ident.to_string().as_str() {
        "name" => out.name = Some(input.parse::<LitStr>()?),
        "timeout" => out.timeout = Some(input.parse::<Expr>()?),
        "ctx" => out.ctx = Some(input.parse::<Type>()?),
        "dump_dir" => out.dump_dir = Some(input.parse::<Expr>()?),
        _ => return Err(unknown_attr_err(ident)),
    }
    Ok(())
}

impl Parse for WatchdogArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut out = Self {
            ctx: None,
            dump_dir: None,
            name: None,
            timeout: None,
        };

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            parse_field(&ident, input, &mut out)?;
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(out)
    }
}
