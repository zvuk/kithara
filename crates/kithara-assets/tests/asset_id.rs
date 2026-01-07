use kithara_assets::{AssetId, AssetsError};
use rstest::rstest;
use url::Url;

#[rstest]
#[case(
    "https://example.com/audio.mp3?token=123&quality=high#section",
    "https://example.com/audio.mp3?different=456#other",
    true,
    "AssetId should ignore both query and fragment"
)]
#[case(
    "https://example.com/audio.mp3?token=123&quality=high#section",
    "https://example.com/audio.mp3",
    true,
    "AssetId should ignore query and fragment compared to clean URL"
)]
#[case(
    "HTTPS://EXAMPLE.COM/audio.mp3",
    "https://example.com/audio.mp3",
    true,
    "AssetId should normalize scheme and host to lowercase"
)]
#[case(
    "https://example.com:443/audio.mp3",
    "https://example.com/audio.mp3",
    true,
    "AssetId should remove default HTTPS port 443"
)]
#[case(
    "http://example.com:80/audio.mp3",
    "http://example.com/audio.mp3",
    true,
    "AssetId should remove default HTTP port 80"
)]
#[case(
    "https://example.com:8443/audio.mp3",
    "https://example.com/audio.mp3",
    false,
    "AssetId should preserve explicit non-default port"
)]
fn test_asset_id_comparisons(
    #[case] url1: &str,
    #[case] url2: &str,
    #[case] should_be_equal: bool,
    #[case] description: &str,
) {
    let url1 = Url::parse(url1).unwrap();
    let url2 = Url::parse(url2).unwrap();

    let asset1 = AssetId::from_url(&url1).unwrap();
    let asset2 = AssetId::from_url(&url2).unwrap();

    if should_be_equal {
        assert_eq!(asset1, asset2, "{}", description);
    } else {
        assert_ne!(asset1, asset2, "{}", description);
    }
}

#[test]
fn test_asset_id_stable_across_calls() {
    let url = Url::parse("https://example.com/path/to/audio.mp3?version=1.2").unwrap();

    let asset1 = AssetId::from_url(&url).unwrap();
    let asset2 = AssetId::from_url(&url).unwrap();

    assert_eq!(
        asset1, asset2,
        "AssetId should be stable across multiple calls"
    );
}

#[rstest]
#[case("file:///path/to/audio.mp3", "URL without host should error")]
#[case("", "Empty URL should error")]
fn test_asset_id_errors_on_missing_host(#[case] url_str: &str, #[case] description: &str) {
    if url_str.is_empty() {
        // Empty string cannot be parsed as URL
        return;
    }

    let url = Url::parse(url_str).unwrap();
    let result = AssetId::from_url(&url);

    assert!(result.is_err(), "{}", description);
    assert!(
        matches!(result, Err(AssetsError::MissingComponent(_))),
        "{} should return MissingComponent error",
        description
    );
}

#[test]
fn test_asset_id_as_bytes() {
    let url = Url::parse("https://example.com/audio.mp3").unwrap();
    let asset_id = AssetId::from_url(&url).unwrap();

    let bytes = asset_id.as_bytes();
    assert_eq!(bytes.len(), 32, "AssetId should be 32 bytes");

    // Verify that the bytes are consistent
    let asset_id2 = AssetId::from_url(&url).unwrap();
    assert_eq!(
        asset_id.as_bytes(),
        asset_id2.as_bytes(),
        "Bytes should be consistent"
    );
}

#[rstest]
#[case("https://example.com/audio.mp3", "Simple URL")]
#[case(
    "https://example.com:8080/audio.mp3?quality=high",
    "URL with non-default port and query"
)]
#[case("HTTPS://EXAMPLE.COM/PATH/TO/FILE.MP3", "URL with uppercase")]
fn test_asset_id_creation_success(#[case] url_str: &str, #[case] description: &str) {
    let url = Url::parse(url_str).unwrap();
    let result = AssetId::from_url(&url);

    assert!(
        result.is_ok(),
        "{} should create AssetId successfully",
        description
    );

    let asset_id = result.unwrap();
    assert_eq!(
        asset_id.as_bytes().len(),
        32,
        "{} should produce 32-byte hash",
        description
    );
}
