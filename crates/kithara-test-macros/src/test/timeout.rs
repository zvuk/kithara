use kithara_test_args::TestArgs;
use syn::{Expr, Ident, parse_quote};

pub(crate) fn resolve(args: &TestArgs) -> Expr {
    let flash = args.flash().unwrap_or(true);
    let budget = args
        .timeout()
        .as_ref()
        .cloned()
        .unwrap_or_else(|| parse_quote!(Quick));
    named(&budget, flash)
}

fn named(budget: &Ident, flash: bool) -> Expr {
    let flash: Expr = if flash {
        parse_quote!(::kithara_test_utils::kithara_platform::flash::ENABLED)
    } else {
        parse_quote!(false)
    };
    parse_quote!({
        const __TIMEOUT: ::std::time::Duration =
            ::kithara_test_utils::TestBudget::#budget.timeout(#flash);
        __TIMEOUT
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_args::TestArgs;
    use quote::ToTokens;

    use super::resolve;

    #[test]
    fn timeout_accepts_every_named_tier() -> syn::Result<()> {
        for tier in ["Quick", "Short", "Medium", "Long", "Network"] {
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
            "1000",
            "{ Long }",
            "Long, Short",
        ] {
            assert!(
                syn::parse_str::<TestArgs>(&format!("timeout({expression})")).is_err(),
                "timeout expression must be rejected: {expression}"
            );
        }
    }

    #[test]
    fn a_missing_timeout_uses_a_constant_quick_budget() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("")?;
        let expanded = resolve(&args).to_token_stream().to_string();
        assert!(expanded.contains("const __TIMEOUT"));
        assert!(expanded.contains("TestBudget :: Quick . timeout"));
        assert!(expanded.contains("kithara_platform :: flash :: ENABLED"));
        Ok(())
    }

    #[test]
    fn named_budgets_keep_the_explicit_clock_and_compiled_feature() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("flash(false), timeout(Medium)")?;
        let expanded = resolve(&args).to_token_stream().to_string();
        assert!(expanded.contains("TestBudget :: Medium . timeout"));
        assert!(expanded.contains("timeout (false)"));
        assert!(!expanded.contains("flash :: ENABLED"));
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
