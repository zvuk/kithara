use kithara_test_args::TestArgs;
use syn::{Expr, Ident, parse_quote};

pub(crate) fn resolve(args: &TestArgs) -> Expr {
    let flash = args.flash().unwrap_or(true);
    let budget = match args.timeout() {
        None => Some(parse_quote!(Quick)),
        Some(Expr::Path(path)) if path.qself.is_none() => path
            .path
            .get_ident()
            .filter(|name| {
                matches!(
                    name.to_string().as_str(),
                    "Quick" | "Short" | "Medium" | "Long"
                )
            })
            .cloned(),
        _ => None,
    };
    if let Some(budget) = budget {
        return named(&budget, flash);
    }
    let manual = args.timeout();
    let miri = named(&parse_quote!(Long), flash);
    parse_quote!({
        #[cfg(miri)]
        { #miri }
        #[cfg(not(miri))]
        { #manual }
    })
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
    use syn::Expr;

    use super::resolve;

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
    fn manual_expressions_are_replaced_only_in_miri() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("timeout(Duration::from_secs(5))")?;
        let expr = resolve(&args);
        let Expr::Block(block) = &expr else {
            panic!("manual timeout must select its mode at compile time");
        };
        assert_eq!(block.block.stmts.len(), 2);
        let expanded = expr.to_token_stream().to_string();
        assert!(expanded.contains("cfg (miri)"));
        assert!(expanded.contains("TestBudget :: Long . timeout"));
        assert!(expanded.contains("cfg (not (miri))"));
        assert!(expanded.contains("Duration :: from_secs (5)"));
        Ok(())
    }
}
