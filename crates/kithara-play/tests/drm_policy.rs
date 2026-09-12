use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_drm::{
    KeyProcessor, KeyProcessorRegistry, KeyRequest, KeyRequestFactory, KeyRequestResolver,
};
use kithara_play::policy::{DomainKeyPolicy, DomainKeyRule};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;
use url::Url;

fn url(value: &str) -> Url {
    Url::parse(value).expect("valid test URL")
}

fn key_factory(
    dynamic_headers: HashMap<String, String>,
    processor: KeyProcessor,
) -> KeyRequestFactory {
    Arc::new(move || KeyRequest::new(dynamic_headers.clone(), Arc::clone(&processor)))
}

fn identity_factory() -> KeyRequestFactory {
    key_factory(HashMap::new(), Arc::new(Ok))
}

fn reverse_processor() -> KeyProcessor {
    Arc::new(|key| Ok(Bytes::from(key.iter().rev().copied().collect::<Vec<_>>())))
}

fn reverse_factory() -> KeyRequestFactory {
    key_factory(HashMap::new(), reverse_processor())
}

fn xor_factory(mask: u8) -> KeyRequestFactory {
    key_factory(
        HashMap::new(),
        Arc::new(move |key| {
            Ok(Bytes::from(
                key.iter().map(|byte| byte ^ mask).collect::<Vec<_>>(),
            ))
        }),
    )
}

#[kithara::test]
fn drm_policy_routes_processors_by_exact_wildcard_and_all_domains() {
    let policy = DomainKeyPolicy::new([
        DomainKeyRule::new(["KEYS.EXAMPLE.COM"], reverse_factory()),
        DomainKeyRule::new(["*.Example.COM"], xor_factory(0xaa)),
        DomainKeyRule::new(["*"], identity_factory()),
    ]);
    let mut registry = KeyProcessorRegistry::new();
    registry.register(Arc::new(policy));

    let exact = registry
        .prepare(&url("https://keys.example.com/key.bin"))
        .expect("exact request");
    let wildcard = registry
        .prepare(&url("https://edge.keys.example.com/key.bin"))
        .expect("wildcard request");
    let bare = registry
        .prepare(&url("https://example.com/key.bin"))
        .expect("all request");
    let suffix_collision = registry
        .prepare(&url("https://notexample.com/key.bin"))
        .expect("all request");

    assert_eq!(
        (exact.processor)(Bytes::from_static(b"abcd")).expect("exact processor"),
        Bytes::from_static(b"dcba")
    );
    assert_eq!(
        (wildcard.processor)(Bytes::from_static(&[0x00, 0x55])).expect("wildcard processor"),
        Bytes::from_static(&[0xaa, 0xff])
    );
    assert_eq!(
        (bare.processor)(Bytes::from_static(b"plain")).expect("all processor"),
        Bytes::from_static(b"plain")
    );
    assert_eq!(
        (suffix_collision.processor)(Bytes::from_static(b"plain")).expect("all processor"),
        Bytes::from_static(b"plain")
    );
}

#[kithara::test]
fn prepared_drm_request_merges_headers_rewrites_url_and_redacts_secrets() {
    let static_headers = HashMap::from([
        ("X-App".to_string(), "top-secret".to_string()),
        ("X-Seed".to_string(), "static".to_string()),
    ]);
    let dynamic_headers = HashMap::from([("X-Seed".to_string(), "dynamic".to_string())]);
    let query_params = HashMap::from([("auth".to_string(), "query-secret".to_string())]);
    let policy = DomainKeyPolicy::new([DomainKeyRule::for_domains(
        ["keys.example.com"],
        key_factory(dynamic_headers, reverse_processor()),
    )
    .headers(static_headers)
    .query_params(query_params)
    .build()]);

    let request = policy
        .prepare(&url("https://keys.example.com/key.bin?existing=1"))
        .expect("prepared request");
    let pairs: HashMap<_, _> = request.url.query_pairs().into_owned().collect();

    assert_eq!(pairs.get("existing").map(String::as_str), Some("1"));
    assert_eq!(pairs.get("auth").map(String::as_str), Some("query-secret"));
    assert_eq!(
        request.headers.get("X-App").map(String::as_str),
        Some("top-secret")
    );
    assert_eq!(
        request.headers.get("X-Seed").map(String::as_str),
        Some("dynamic")
    );
    assert_eq!(
        (request.processor)(Bytes::from_static(b"abcd")).expect("processor"),
        Bytes::from_static(b"dcba")
    );

    let debug = format!("{policy:?}");
    assert!(!debug.contains("top-secret"));
    assert!(!debug.contains("query-secret"));
    assert!(!debug.contains("dynamic"));
}

#[kithara::test]
fn resource_headers_follow_the_same_first_matching_rule() {
    let policy = DomainKeyPolicy::new([
        DomainKeyRule::for_domains(["*.example.com"], identity_factory())
            .headers(HashMap::from([(
                "X-Provider".to_string(),
                "specific".to_string(),
            )]))
            .build(),
        DomainKeyRule::for_domains(["*"], identity_factory())
            .headers(HashMap::from([(
                "X-Provider".to_string(),
                "fallback".to_string(),
            )]))
            .build(),
    ]);

    let specific = policy
        .resource_headers(&url("https://cdn.example.com/master.m3u8"))
        .expect("specific headers");
    let fallback = policy
        .resource_headers(&url("https://example.com/master.m3u8"))
        .expect("fallback headers");

    assert_eq!(specific.get("X-Provider"), Some("specific"));
    assert_eq!(fallback.get("X-Provider"), Some("fallback"));
}
