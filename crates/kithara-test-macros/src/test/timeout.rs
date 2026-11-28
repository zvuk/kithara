use kithara_test_args::TestArgs;
use syn::{Expr, parse_quote};

pub(crate) fn resolve(args: &TestArgs) -> Expr {
    let budget = args
        .timeout()
        .as_ref()
        .cloned()
        .unwrap_or_else(|| parse_quote!(Quick));
    let flash_budget = args
        .flash_timeout()
        .as_ref()
        .cloned()
        .unwrap_or_else(|| budget.clone());
    let flash = args.flash().unwrap_or(true);
    parse_quote!({
        const __TIMEOUT: ::std::time::Duration =
            ::kithara_test_utils::TestBudget::#budget.timeout();
        const __FLASH_TIMEOUT: ::std::time::Duration =
            ::kithara_test_utils::TestBudget::#flash_budget.timeout();
        ::kithara_test_utils::test::timeout_for_clock(__TIMEOUT, __FLASH_TIMEOUT, #flash)
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_args::TestArgs;
    use quote::ToTokens;

    use super::resolve;

    #[test]
    fn timeout_accepts_every_named_tier() -> syn::Result<()> {
        for tier in ["Quick", "Short", "Medium", "Long", "Heavy", "Network"] {
            let args = syn::parse_str::<TestArgs>(&format!("timeout({tier})"))?;
            assert_eq!(
                args.timeout().as_ref().map(ToString::to_string).as_deref(),
                Some(tier)
            );
            let args = syn::parse_str::<TestArgs>(&format!("flash_timeout({tier})"))?;
            assert_eq!(
                args.flash_timeout()
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref(),
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
            "1000",
            "{ Long }",
            "Long, Short",
        ] {
            assert!(
                syn::parse_str::<TestArgs>(&format!("timeout({expression})")).is_err(),
                "timeout expression must be rejected: {expression}"
            );
            assert!(
                syn::parse_str::<TestArgs>(&format!("flash_timeout({expression})")).is_err(),
                "flash_timeout expression must be rejected: {expression}"
            );
        }
    }

    #[test]
    fn a_missing_timeout_uses_a_constant_quick_budget() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("")?;
        let expanded = resolve(&args).to_token_stream().to_string();
        assert!(expanded.contains("const __TIMEOUT"));
        assert!(expanded.contains("TestBudget :: Quick . timeout ()"));
        Ok(())
    }

    #[test]
    fn clock_mode_selects_the_matching_budget() -> syn::Result<()> {
        let real = syn::parse_str::<TestArgs>("flash(false), timeout(Medium)")?;
        let flash = syn::parse_str::<TestArgs>("timeout(Medium)")?;
        let real = resolve(&real).to_token_stream().to_string();
        assert!(real.contains("TestBudget :: Medium . timeout ()"));
        assert!(real.contains("timeout_for_clock"));
        assert_ne!(real, resolve(&flash).to_token_stream().to_string());
        Ok(())
    }

    #[test]
    fn flash_timeout_can_be_tighter_than_the_real_budget() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("timeout(Heavy), flash_timeout(Quick)")?;
        let expanded = resolve(&args).to_token_stream().to_string();
        assert!(expanded.contains("TestBudget :: Heavy . timeout ()"));
        assert!(expanded.contains("TestBudget :: Quick . timeout ()"));
        assert!(expanded.contains("timeout_for_clock"));
        Ok(())
    }

    #[test]
    fn manual_duration_expressions_are_rejected() {
        for expression in [
            "Duration::from_secs(5)",
            "std::time::Duration::from_millis(2186)",
            "::std::time::Duration::from_secs(5)",
        ] {
            assert!(syn::parse_str::<TestArgs>(&format!("timeout({expression})")).is_err());
        }
    }
}
