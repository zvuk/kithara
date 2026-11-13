use proc_macro2::Span;
use syn::{
    Error, Expr, Ident, Token,
    parse::{Parse, ParseStream},
};

bitflags::bitflags! {
    /// Execution modes and explicit boolean settings of a test attribute.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct TestFlags: u16 {
        const BROWSER = 1 << 0;
        const LOOM = 1 << 1;
        const MIRI = 1 << 2;
        const MULTI_THREAD = 1 << 3;
        const NATIVE_ONLY = 1 << 4;
        const SELENIUM = 1 << 5;
        const SERIAL = 1 << 6;
        const TOKIO = 1 << 7;
        const WASM_ONLY = 1 << 8;
        const FLASH_EXPLICIT = 1 << 9;
        const FLASH_ENABLED = 1 << 10;
    }
}

/// Arguments accepted by the shared Kithara test attribute.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(get, set)]
pub struct TestArgs {
    #[field(get(copy))]
    flags: TestFlags,
    hang_timeout_secs: Option<u64>,
    #[field(get(option_borrow_inner = false))]
    timeout: Option<Expr>,
    tracing_filter: Option<String>,
    soft_fail_patterns: Vec<String>,
}

impl TestArgs {
    /// The explicit flash setting; absence keeps the expansion's default.
    #[must_use]
    pub fn flash(&self) -> Option<bool> {
        self.flags
            .contains(TestFlags::FLASH_EXPLICIT)
            .then_some(self.flags.contains(TestFlags::FLASH_ENABLED))
    }

    fn check_exclusive(
        &self,
        a: TestFlags,
        a_name: &str,
        b: TestFlags,
        b_name: &str,
    ) -> syn::Result<()> {
        if self.flags.contains(a | b) {
            Err(Error::new(
                Span::call_site(),
                format!("`{a_name}` and `{b_name}` are mutually exclusive"),
            ))
        } else {
            Ok(())
        }
    }

    fn validate_exclusive_modes(&self) -> syn::Result<()> {
        self.check_exclusive(TestFlags::TOKIO, "tokio", TestFlags::WASM_ONLY, "wasm")?;
        self.check_exclusive(
            TestFlags::WASM_ONLY,
            "wasm",
            TestFlags::NATIVE_ONLY,
            "native",
        )?;
        self.check_exclusive(TestFlags::BROWSER, "browser", TestFlags::WASM_ONLY, "wasm")?;
        self.check_exclusive(
            TestFlags::SELENIUM,
            "selenium",
            TestFlags::WASM_ONLY,
            "wasm",
        )?;
        self.check_exclusive(
            TestFlags::SELENIUM,
            "selenium",
            TestFlags::BROWSER,
            "browser",
        )?;
        self.check_exclusive(TestFlags::LOOM, "loom", TestFlags::WASM_ONLY, "wasm")?;
        self.check_exclusive(TestFlags::LOOM, "loom", TestFlags::BROWSER, "browser")?;
        self.check_exclusive(TestFlags::LOOM, "loom", TestFlags::SELENIUM, "selenium")?;
        self.check_exclusive(
            TestFlags::LOOM,
            "loom",
            TestFlags::MULTI_THREAD,
            "multi_thread",
        )?;
        self.check_exclusive(TestFlags::LOOM, "loom", TestFlags::TOKIO, "tokio")?;

        Ok(())
    }

    fn validate(&mut self) -> syn::Result<()> {
        self.validate_exclusive_modes()?;

        if self.flags.contains(TestFlags::LOOM) && !self.soft_fail_patterns.is_empty() {
            return Err(Error::new(
                Span::call_site(),
                "`loom` and `soft_fail` are mutually exclusive",
            ));
        }

        if self.flags.contains(TestFlags::SELENIUM) {
            self.flags.insert(
                TestFlags::NATIVE_ONLY
                    | TestFlags::TOKIO
                    | TestFlags::SERIAL
                    | TestFlags::MULTI_THREAD,
            );
        }

        if self.flags.contains(TestFlags::LOOM) {
            self.flags.insert(TestFlags::NATIVE_ONLY);
        }

        if self.flags.contains(TestFlags::MULTI_THREAD) && !self.flags.contains(TestFlags::TOKIO) {
            return Err(Error::new(
                Span::call_site(),
                "`multi_thread` requires `tokio` (or `selenium` which implies it)",
            ));
        }

        Ok(())
    }
}

fn parse_comma_separated<T>(
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
                "tokio" => args.flags.insert(TestFlags::TOKIO),
                "wasm" => args.flags.insert(TestFlags::WASM_ONLY),
                "native" => args.flags.insert(TestFlags::NATIVE_ONLY),
                "browser" => args.flags.insert(TestFlags::BROWSER),
                "loom" => args.flags.insert(TestFlags::LOOM),
                "miri" => args.flags.insert(TestFlags::MIRI),
                "serial" => args.flags.insert(TestFlags::SERIAL),
                "selenium" => args.flags.insert(TestFlags::SELENIUM),
                "multi_thread" => args.flags.insert(TestFlags::MULTI_THREAD),
                "timeout" => {
                    let content;
                    syn::parenthesized!(content in input);
                    args.set_timeout(Some(content.parse()?));
                }
                "flash" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let lit: syn::LitBool = content.parse()?;
                    args.flags.insert(TestFlags::FLASH_EXPLICIT);
                    args.flags.set(TestFlags::FLASH_ENABLED, lit.value);
                }
                "hang_timeout_secs" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let secs: syn::LitInt = content.parse()?;
                    args.set_hang_timeout_secs(Some(secs.base10_parse()?));
                }
                "soft_fail" => {
                    let patterns = parse_comma_separated(input, |content| {
                        let pattern: syn::LitStr = content.parse()?;
                        Ok(pattern.value())
                    })?;
                    args.set_soft_fail_patterns(patterns);
                    if args.soft_fail_patterns().is_empty() {
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
                    args.set_tracing_filter(Some(value.value()));
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
    use super::{TestArgs, TestFlags};

    #[test]
    fn flash_preserves_default_false_true_and_last_assignment() -> syn::Result<()> {
        for (source, expected) in [
            ("", None),
            ("flash(false)", Some(false)),
            ("flash(true)", Some(true)),
            ("flash(false), miri, flash(true)", Some(true)),
            ("flash(true), tokio, flash(false)", Some(false)),
        ] {
            let args = syn::parse_str::<TestArgs>(source)?;
            assert_eq!(args.flash(), expected, "{source}");
            assert_eq!(
                args.flags().contains(TestFlags::FLASH_EXPLICIT),
                expected.is_some(),
                "{source}"
            );
        }
        Ok(())
    }

    #[test]
    fn mode_bits_remain_independent_and_selenium_keeps_its_implications() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("miri, tokio")?;
        assert_eq!(args.flags(), TestFlags::MIRI | TestFlags::TOKIO);
        let args = syn::parse_str::<TestArgs>("selenium")?;
        assert_eq!(
            args.flags(),
            TestFlags::SELENIUM
                | TestFlags::NATIVE_ONLY
                | TestFlags::TOKIO
                | TestFlags::SERIAL
                | TestFlags::MULTI_THREAD
        );
        assert_eq!(args.flash(), None);
        Ok(())
    }

    #[test]
    fn loom_implies_native() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("loom")?;
        assert!(args.flags().contains(TestFlags::LOOM));
        assert!(args.flags().contains(TestFlags::NATIVE_ONLY));
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
