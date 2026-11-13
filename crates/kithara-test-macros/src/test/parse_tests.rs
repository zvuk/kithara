use kithara_test_args::{TestArgs, TestFlags};

#[test]
fn soft_fail_lists_preserve_order_and_reject_empty_or_malformed_input() -> syn::Result<()> {
    let args = syn::parse_str::<TestArgs>(r#"soft_fail("timeout", "cancelled",)"#)?;
    assert_eq!(args.soft_fail_patterns(), ["timeout", "cancelled"]);
    for source in ["soft_fail()", "soft_fail(1)", r#"soft_fail("a" "b")"#] {
        assert!(syn::parse_str::<TestArgs>(source).is_err(), "{source}");
    }
    Ok(())
}

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
