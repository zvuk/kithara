use proc_macro2::Span;
use syn::{
    Error, Expr, Ident, Token,
    parse::{Parse, ParseStream},
};

#[derive(Default)]
pub(crate) struct TestArgs {
    /// `flash(true|false)`; `None` defaults to `true` at expansion. `true` runs
    /// the lexical body-only time-call rewrite + sets the per-test ambient gate;
    /// `false` sets ambient off and does NOT rewrite (the body runs REAL).
    pub(crate) flash: Option<bool>,
    pub(crate) timeout: Option<Expr>,
    pub(crate) tracing_filter: Option<String>,
    pub(crate) env_vars: Vec<(String, String)>,
    /// Substring patterns for soft-fail: if a panic message contains any of
    /// these (case-insensitive), the test prints a warning instead of failing.
    pub(crate) soft_fail_patterns: Vec<String>,
    pub(crate) is_browser: bool,
    pub(crate) is_loom: bool,
    pub(crate) is_multi_thread: bool,
    pub(crate) is_native_only: bool,
    pub(crate) is_selenium: bool,
    pub(crate) is_serial: bool,
    pub(crate) is_tokio: bool,
    pub(crate) is_wasm_only: bool,
}

impl TestArgs {
    fn check_exclusive(a: bool, a_name: &str, b: bool, b_name: &str) -> syn::Result<()> {
        if a && b {
            Err(Error::new(
                Span::call_site(),
                format!("`{a_name}` and `{b_name}` are mutually exclusive"),
            ))
        } else {
            Ok(())
        }
    }

    fn validate(&mut self) -> syn::Result<()> {
        Self::check_exclusive(self.is_tokio, "tokio", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_wasm_only, "wasm", self.is_native_only, "native")?;
        Self::check_exclusive(self.is_browser, "browser", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_selenium, "selenium", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_selenium, "selenium", self.is_browser, "browser")?;
        Self::check_exclusive(self.is_loom, "loom", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_loom, "loom", self.is_browser, "browser")?;
        Self::check_exclusive(self.is_loom, "loom", self.is_selenium, "selenium")?;
        Self::check_exclusive(self.is_loom, "loom", self.is_multi_thread, "multi_thread")?;
        Self::check_exclusive(self.is_loom, "loom", self.is_tokio, "tokio")?;

        if self.is_loom && !self.soft_fail_patterns.is_empty() {
            return Err(Error::new(
                Span::call_site(),
                "`loom` and `soft_fail` are mutually exclusive",
            ));
        }

        if self.is_selenium {
            self.is_native_only = true;
            self.is_tokio = true;
            self.is_serial = true;
            self.is_multi_thread = true;
        }

        if self.is_loom {
            self.is_native_only = true;
        }

        if self.is_multi_thread && !self.is_tokio {
            return Err(Error::new(
                Span::call_site(),
                "`multi_thread` requires `tokio` (or `selenium` which implies it)",
            ));
        }

        Ok(())
    }
}

pub(crate) fn parse_comma_separated<T>(
    input: ParseStream<'_>,
    mut parse_item: impl FnMut(ParseStream<'_>) -> syn::Result<T>,
) -> syn::Result<Vec<T>> {
    let content;
    syn::parenthesized!(content in input);
    let mut items = Vec::new();
    while !content.is_empty() {
        items.push(parse_item(&content)?);
        if !content.is_empty() {
            content.parse::<Token![,]>()?;
        }
    }
    Ok(items)
}

impl Parse for TestArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            match ident.to_string().as_str() {
                "tokio" => args.is_tokio = true,
                "wasm" => args.is_wasm_only = true,
                "native" => args.is_native_only = true,
                "browser" => args.is_browser = true,
                "loom" => args.is_loom = true,
                "serial" => args.is_serial = true,
                "selenium" => args.is_selenium = true,
                "multi_thread" => args.is_multi_thread = true,
                "timeout" => {
                    let content;
                    syn::parenthesized!(content in input);
                    args.timeout = Some(content.parse()?);
                }
                "flash" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let lit: syn::LitBool = content.parse()?;
                    args.flash = Some(lit.value);
                }
                "env" => {
                    args.env_vars = parse_comma_separated(input, |content| {
                        let key: Ident = content.parse()?;
                        content.parse::<Token![=]>()?;
                        let value: syn::LitStr = content.parse()?;
                        Ok((key.to_string(), value.value()))
                    })?;
                }
                "soft_fail" => {
                    args.soft_fail_patterns = parse_comma_separated(input, |content| {
                        let pattern: syn::LitStr = content.parse()?;
                        Ok(pattern.value())
                    })?;
                    if args.soft_fail_patterns.is_empty() {
                        return Err(Error::new(
                            ident.span(),
                            "soft_fail requires at least one pattern string",
                        ));
                    }
                }
                "tracing" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let value: syn::LitStr = content.parse()?;
                    args.tracing_filter = Some(value.value());
                }
                other => {
                    return Err(Error::new(
                        ident.span(),
                        format!("unknown argument: {other}"),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        args.validate()?;
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::TestArgs;

    #[test]
    fn loom_implies_native() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("loom")?;
        assert!(args.is_loom);
        assert!(args.is_native_only);
        Ok(())
    }

    #[test]
    fn loom_rejects_non_native_execution_modes() {
        for input in [
            "loom, wasm",
            "loom, browser",
            "loom, tokio",
            "loom, multi_thread, tokio",
            "loom, soft_fail(\"timeout\")",
        ] {
            assert!(
                syn::parse_str::<TestArgs>(input).is_err(),
                "accepted {input}"
            );
        }
    }
}
