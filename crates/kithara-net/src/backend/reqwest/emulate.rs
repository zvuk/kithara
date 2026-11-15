// Extracted and adapted by Kithara from Apache-2.0 wreq-util 3.0.0-rc.14.

use wreq::{
    Emulation, Group,
    header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, USER_AGENT},
    http2::{
        Http2Options, PseudoId, PseudoOrder, SettingId, SettingsOrder, StreamDependency, StreamId,
    },
    tls::{AlpsProtocol, TlsOptions, TlsVersion},
};

use super::certificate;
use crate::types::ImpersonatePreset;

mod consts {
    pub(super) const SAFARI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
    pub(super) const SAFARI_CURVES: &str = "X25519:P-256:P-384:P-521";

    pub(super) const SAFARI_CIPHERS: &str = concat!(
        "TLS_AES_128_GCM_SHA256:",
        "TLS_AES_256_GCM_SHA384:",
        "TLS_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:",
        "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:",
        "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
        "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
        "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA:",
        "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA:",
        "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
        "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:",
        "TLS_RSA_WITH_AES_256_GCM_SHA384:",
        "TLS_RSA_WITH_AES_128_GCM_SHA256:",
        "TLS_RSA_WITH_AES_256_CBC_SHA:",
        "TLS_RSA_WITH_AES_128_CBC_SHA:",
        "TLS_ECDHE_ECDSA_WITH_3DES_EDE_CBC_SHA:",
        "TLS_ECDHE_RSA_WITH_3DES_EDE_CBC_SHA:",
        "TLS_RSA_WITH_3DES_EDE_CBC_SHA",
    );

    pub(super) const SAFARI_SIGALGS: &str = concat!(
        "ecdsa_secp256r1_sha256:",
        "rsa_pss_rsae_sha256:",
        "rsa_pkcs1_sha256:",
        "ecdsa_secp384r1_sha384:",
        "ecdsa_sha1:",
        "rsa_pss_rsae_sha384:",
        "rsa_pss_rsae_sha384:",
        "rsa_pkcs1_sha384:",
        "rsa_pss_rsae_sha512:",
        "rsa_pkcs1_sha512:",
        "rsa_pkcs1_sha1",
    );

    pub(super) const CHROME_SEC_CH_UA: &str =
        r#""Google Chrome";v="137", "Chromium";v="137", "Not/A)Brand";v="24""#;

    pub(super) const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";
    pub(super) const CHROME_CURVES: &str = "X25519MLKEM768:X25519:P-256:P-384";

    pub(super) const CHROME_CIPHERS: &str = concat!(
        "TLS_AES_128_GCM_SHA256:",
        "TLS_AES_256_GCM_SHA384:",
        "TLS_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:",
        "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
        "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
        "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
        "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:",
        "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
        "TLS_RSA_WITH_AES_128_GCM_SHA256:",
        "TLS_RSA_WITH_AES_256_GCM_SHA384:",
        "TLS_RSA_WITH_AES_128_CBC_SHA:",
        "TLS_RSA_WITH_AES_256_CBC_SHA",
    );

    pub(super) const CHROME_SIGALGS: &str = concat!(
        "ecdsa_secp256r1_sha256:",
        "rsa_pss_rsae_sha256:",
        "rsa_pkcs1_sha256:",
        "ecdsa_secp384r1_sha384:",
        "rsa_pss_rsae_sha384:",
        "rsa_pkcs1_sha384:",
        "rsa_pss_rsae_sha512:",
        "rsa_pkcs1_sha512",
    );
}

pub(super) fn emulation(preset: ImpersonatePreset) -> Emulation {
    match preset {
        ImpersonatePreset::Safari => safari18(),
        ImpersonatePreset::Chrome => chrome137(),
    }
}

fn safari18() -> Emulation {
    Emulation::builder()
        .tls_options(safari_tls())
        .http2_options(safari_http2())
        .headers(safari_headers())
        .build(Group::new("safari18"))
}

fn safari_tls() -> TlsOptions {
    TlsOptions::builder()
        .session_ticket(false)
        .grease_enabled(true)
        .enable_ocsp_stapling(true)
        .enable_signed_cert_timestamps(true)
        .preserve_tls13_cipher_list(None::<bool>)
        .curves_list(consts::SAFARI_CURVES)
        .sigalgs_list(consts::SAFARI_SIGALGS)
        .cipher_list(consts::SAFARI_CIPHERS)
        .min_tls_version(TlsVersion::TLS_1_0)
        .max_tls_version(TlsVersion::TLS_1_3)
        .certificate_compressors(certificate::consts::SAFARI_CERTIFICATE_COMPRESSORS)
        .build()
}

fn safari_http2() -> Http2Options {
    Http2Options::builder()
        .max_concurrent_streams(100)
        .initial_window_size(2097152)
        .initial_connection_window_size(10485760)
        .enable_push(false)
        .enable_connect_protocol(true)
        .no_rfc7540_priorities(true)
        .headers_pseudo_order(
            PseudoOrder::builder()
                .extend([
                    PseudoId::Method,
                    PseudoId::Scheme,
                    PseudoId::Authority,
                    PseudoId::Path,
                ])
                .build(),
        )
        .settings_order(
            SettingsOrder::builder()
                .extend([
                    SettingId::HeaderTableSize,
                    SettingId::EnablePush,
                    SettingId::MaxConcurrentStreams,
                    SettingId::InitialWindowSize,
                    SettingId::MaxFrameSize,
                    SettingId::MaxHeaderListSize,
                    SettingId::EnableConnectProtocol,
                    SettingId::NoRfc7540Priorities,
                ])
                .build(),
        )
        .build()
}

fn safari_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(consts::SAFARI_USER_AGENT),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    );
    headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    headers.insert("priority", HeaderValue::from_static("u=0, i"));
    headers
}

fn chrome137() -> Emulation {
    Emulation::builder()
        .tls_options(chrome_tls())
        .http2_options(chrome_http2())
        .headers(chrome_headers())
        .build(Group::new("v132"))
}

fn chrome_tls() -> TlsOptions {
    TlsOptions::builder()
        .grease_enabled(true)
        .enable_ocsp_stapling(true)
        .enable_signed_cert_timestamps(true)
        .curves_list(consts::CHROME_CURVES)
        .sigalgs_list(consts::CHROME_SIGALGS)
        .cipher_list(consts::CHROME_CIPHERS)
        .min_tls_version(TlsVersion::TLS_1_2)
        .max_tls_version(TlsVersion::TLS_1_3)
        .permute_extensions(true)
        .pre_shared_key(true)
        .enable_ech_grease(true)
        .alps_protocols([AlpsProtocol::HTTP2])
        .alps_use_new_codepoint(true)
        .aes_hw_override(true)
        .certificate_compressors(certificate::consts::CHROME_CERTIFICATE_COMPRESSORS)
        .build()
}

fn chrome_http2() -> Http2Options {
    Http2Options::builder()
        .initial_window_size(6291456)
        .initial_connection_window_size(15728640)
        .max_header_list_size(262144)
        .header_table_size(65536)
        .headers_stream_dependency(StreamDependency::new(StreamId::zero(), 219, true))
        .headers_pseudo_order(
            PseudoOrder::builder()
                .extend([
                    PseudoId::Method,
                    PseudoId::Authority,
                    PseudoId::Scheme,
                    PseudoId::Path,
                ])
                .build(),
        )
        .settings_order(
            SettingsOrder::builder()
                .extend([
                    SettingId::HeaderTableSize,
                    SettingId::EnablePush,
                    SettingId::MaxConcurrentStreams,
                    SettingId::InitialWindowSize,
                    SettingId::MaxFrameSize,
                    SettingId::MaxHeaderListSize,
                    SettingId::EnableConnectProtocol,
                    SettingId::NoRfc7540Priorities,
                ])
                .build(),
        )
        .enable_push(false)
        .build()
}

fn chrome_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "sec-ch-ua",
        HeaderValue::from_static(consts::CHROME_SEC_CH_UA),
    );
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static("\"macOS\""));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(consts::CHROME_USER_AGENT),
    );
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
    );
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    headers.insert("priority", HeaderValue::from_static("u=0, i"));
    headers
}
