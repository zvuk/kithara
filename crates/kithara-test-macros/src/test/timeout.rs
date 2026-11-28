use kithara_test_args::TestArgs;
use syn::{Expr, parse_quote};

pub(crate) fn resolve(args: &TestArgs) -> Expr {
    let budget = args
        .timeout()
        .as_ref()
        .cloned()
        .unwrap_or_else(|| parse_quote!(Quick));
    let flash = args.flash().unwrap_or(true);
    parse_quote!({
        const __TIMEOUT: ::std::time::Duration =
            ::kithara_test_utils::TestBudget::#budget.timeout(false);
        ::kithara_test_utils::test::timeout_for_clock(
            __TIMEOUT,
            ::kithara_test_utils::TestBudget::#budget.timeout(true),
            #flash,
        )
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_args::TestArgs;
    use quote::ToTokens;

    use super::resolve;

    #[test]
    fn timeout_accepts_every_named_tier() -> syn::Result<()> {
        for tier in ["Quick", "Long", "Heavy", "Network"] {
            let args = syn::parse_str::<TestArgs>(&format!("timeout({tier})"))?;
            assert_eq!(
                args.timeout().as_ref().map(ToString::to_string).as_deref(),
                Some(tier)
            );
        }
        Ok(())
    }

    #[test]
    fn timeout_rejects_non_tier_expressions() {
        for expression in [
            "Duration::from_secs(5)",
            "std::time::Duration::from_millis(2186)",
            "::std::time::Duration::from_secs(5)",
            "SOME_TIMEOUT",
            "TestBudget::Long",
            "Short",
            "Medium",
            "1000",
            "{ Long }",
            "Long, Heavy",
        ] {
            assert!(
                syn::parse_str::<TestArgs>(&format!("timeout({expression})")).is_err(),
                "timeout expression must be rejected: {expression}"
            );
        }
    }

    #[test]
    fn a_flash_timeout_argument_is_rejected() {
        assert!(syn::parse_str::<TestArgs>("flash_timeout(Quick)").is_err());
    }

    #[test]
    fn a_missing_timeout_uses_a_constant_quick_budget() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("")?;
        let expanded = resolve(&args).to_token_stream().to_string();
        assert!(expanded.contains("const __TIMEOUT"));
        assert!(expanded.contains("TestBudget :: Quick . timeout (false)"));
        assert!(expanded.contains("TestBudget :: Quick . timeout (true)"));
        Ok(())
    }

    #[test]
    fn clock_mode_selects_the_matching_budget() -> syn::Result<()> {
        let real = syn::parse_str::<TestArgs>("flash(false), timeout(Heavy)")?;
        let flash = syn::parse_str::<TestArgs>("timeout(Heavy)")?;
        let real = resolve(&real).to_token_stream().to_string();
        assert!(real.contains("TestBudget :: Heavy . timeout (false)"));
        assert!(real.contains("TestBudget :: Heavy . timeout (true)"));
        assert!(real.contains("timeout_for_clock"));
        assert_ne!(real, resolve(&flash).to_token_stream().to_string());
        Ok(())
    }
}
