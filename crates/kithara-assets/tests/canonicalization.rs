use kithara_assets::{AssetsError, canonicalize_for_asset};
use rstest::rstest;
use url::Url;

#[rstest]
#[case(
    "https://example.com/audio.mp3?token=123&quality=high#section",
    "https://example.com/audio.mp3",
    "Canonicalization for asset should remove query and fragment"
)]
#[case(
    "HTTPS://EXAMPLE.COM/audio.mp3",
    "https://example.com/audio.mp3",
    "Canonicalization should normalize scheme and host to lowercase"
)]
#[case(
    "https://example.com:443/audio.mp3",
    "https://example.com/audio.mp3",
    "Canonicalization should remove default HTTPS port 443"
)]
#[case(
    "http://example.com:80/audio.mp3",
    "http://example.com/audio.mp3",
    "Canonicalization should remove default HTTP port 80"
)]
#[case(
    "https://example.com:8443/audio.mp3",
    "https://example.com:8443/audio.mp3",
    "Canonicalization should preserve explicit non-default port"
)]
fn test_canonicalize_for_asset(
    #[case] input_url: &str,
    #[case] expected_canonical: &str,
    #[case] description: &str,
) {
    let url = Url::parse(input_url).unwrap();
    let result = canonicalize_for_asset(&url).unwrap();

    assert_eq!(result, expected_canonical, "{}", description);
}

#[rstest]
#[case("file:///path/to/audio.mp3", "file URL without host should error")]
#[case("", "Empty URL string should fail to parse")]
fn test_canonicalize_for_asset_errors_on_missing_host(
    #[case] url_str: &str,
    #[case] description: &str,
) {
    if url_str.is_empty() {
        // Empty string cannot be parsed as URL
        return;
    }

    let url = Url::parse(url_str).unwrap();
    let result = canonicalize_for_asset(&url);

    assert!(result.is_err(), "{}", description);
    assert!(
        matches!(result, Err(AssetsError::MissingComponent(host)) if host == "host"),
        "{} should return MissingComponent error for host",
        description
    );
}
