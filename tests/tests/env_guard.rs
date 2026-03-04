use std::env;

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(
    native,
    env(NO_PROXY = "127.0.0.1,localhost"),
    timeout(Duration::from_secs(5))
)]
fn no_proxy_env_clears_proxy_variables() {
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        assert!(
            env::var(key).is_err(),
            "{key} must be unset when NO_PROXY is configured",
        );
    }
}

#[kithara::test(
    native,
    env(
        NO_PROXY = "127.0.0.1,localhost",
        HTTP_PROXY = "http://example.test:8080",
    ),
    timeout(Duration::from_secs(5))
)]
fn no_proxy_env_keeps_explicit_proxy_override() {
    assert_eq!(
        env::var("HTTP_PROXY").ok().as_deref(),
        Some("http://example.test:8080"),
        "explicit proxy override must not be removed by NO_PROXY helper",
    );
}
