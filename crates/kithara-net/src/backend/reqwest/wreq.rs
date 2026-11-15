pub(crate) use ::wreq::{Client, ClientBuilder};

use super::metrics::CountConnectionsLayer;
use crate::{metrics::ConnectionMetrics, types::NetOptions};

pub(crate) type BackendError = ::wreq::Error;

/// Build the HTTP `Client` (`client-wreq` backend): `BoringSSL` plus browser
/// TLS/HTTP2 emulation so anti-bot WAFs that fingerprint the `ClientHello`
/// (JA3) see a real browser. Applies pool / cert / compression knobs.
///
/// No client-level `.read_timeout`: the idle/stall timeout is owned by the
/// resilient body (`resumable_body`), whose `sleep(inactivity_timeout)` routes
/// through `kithara_platform::time` and so collapses under `flash`. A
/// wall-clock client timer would double-own the stall and break simulation
/// determinism.
pub(crate) fn build_client(
    options: &NetOptions,
    metrics: &ConnectionMetrics,
) -> Result<Client, BackendError> {
    let base = Client::builder()
        .connector_layer(CountConnectionsLayer::new(metrics.clone()))
        .emulation(super::emulate::emulation(options.impersonate))
        .cookie_store(true)
        .pool_max_idle_per_host(options.pool_max_idle_per_host)
        .pool_idle_timeout(Some(options.pool_idle_timeout))
        .tls_cert_verification(!options.is_insecure);
    super::apply_compression(base, options.compression).build()
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::io::{self, Write};

    use ::wreq::{
        Emulation,
        http2::{PseudoId, PseudoOrder, SettingId, SettingsOrder},
        tls::{
            AlpnProtocol, AlpsProtocol, TlsVersion,
            compress::{CertificateCompressionAlgorithm, Codec},
        },
    };

    use crate::types::ImpersonatePreset;

    fn assert_common(profile: &Emulation) {
        assert!(profile.http1_options.is_none());
        assert!(profile.orig_headers.is_empty());
        let tls = profile.tls_options.as_ref().unwrap();
        assert_eq!(
            tls.alpn_protocols.as_deref(),
            Some([AlpnProtocol::HTTP2, AlpnProtocol::HTTP1].as_slice())
        );
        assert_eq!(tls.max_tls_version, Some(TlsVersion::TLS_1_3));
        assert_eq!(tls.grease_enabled, Some(true));
        assert!(tls.enable_ocsp_stapling);
        assert!(tls.enable_signed_cert_timestamps);
        assert!(!tls.psk_skip_session_ticket);
        assert!(tls.psk_dhe_ke);
        assert!(tls.renegotiation);
        assert!(!tls.random_aes_hw_override);
        assert!(tls.key_shares.is_none());
        assert!(tls.delegated_credentials.is_none());
        assert!(tls.record_size_limit.is_none());
        assert!(tls.extension_permutation.is_none());
        assert!(tls.preserve_tls13_cipher_list.is_none());
        let http2 = profile.http2_options.as_ref().unwrap();
        assert_eq!(http2.enable_push, Some(false));
        let settings = SettingsOrder::builder()
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
            .build();
        assert_eq!(
            format!("{:?}", http2.settings_order),
            format!("{:?}", Some(settings))
        );
    }

    fn assert_headers(profile: &Emulation, expected: &[(&str, &str)]) {
        let actual: Vec<_> = profile
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect();
        assert_eq!(actual, expected);
        assert!(!profile.headers.contains_key("accept-encoding"));
    }

    fn call_codec(codec: Codec, input: &[u8], output: &mut dyn Write) -> io::Result<()> {
        match codec {
            Codec::Pointer(call) => call(input, output),
            Codec::Dynamic(call) => call(input, output),
        }
    }

    fn assert_certificate_codec(profile: &Emulation, algorithm: CertificateCompressionAlgorithm) {
        let compressors = profile
            .tls_options
            .as_ref()
            .unwrap()
            .certificate_compressors
            .as_deref()
            .unwrap();
        assert_eq!(compressors.len(), 1);
        let compressor = compressors[0];
        assert_eq!(compressor.algorithm(), algorithm);
        for input in [
            Vec::new(),
            b"certificate-chain\0with-binary\xff".repeat(128),
        ] {
            let mut encoded = Vec::new();
            call_codec(compressor.compress(), &input, &mut encoded).unwrap();
            assert!(!encoded.is_empty());
            let mut decoded = Vec::new();
            call_codec(compressor.decompress(), &encoded, &mut decoded).unwrap();
            assert_eq!(decoded, input);
            assert!(
                call_codec(compressor.decompress(), &encoded, &mut [].as_mut_slice()).is_err()
                    || input.is_empty()
            );
        }
        assert!(
            call_codec(
                compressor.compress(),
                b"certificate",
                &mut [].as_mut_slice()
            )
            .is_err()
        );
        assert!(call_codec(compressor.decompress(), &[0xff; 32], &mut Vec::new()).is_err());
    }

    #[kithara::test]
    fn safari_profile() {
        let profile: Emulation = super::super::emulate::emulation(ImpersonatePreset::Safari);
        assert_common(&profile);
        assert_headers(
            &profile,
            &[
                ("sec-fetch-dest", "document"),
                (
                    "user-agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
                ),
                (
                    "accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
                ("sec-fetch-site", "none"),
                ("sec-fetch-mode", "navigate"),
                ("accept-language", "en-US,en;q=0.9"),
                ("priority", "u=0, i"),
            ],
        );
        let tls = profile.tls_options.as_ref().unwrap();
        assert_eq!(tls.curves_list.as_deref(), Some("X25519:P-256:P-384:P-521"));
        assert_eq!(
            tls.cipher_list.as_deref(),
            Some(
                "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA:TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA:TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:TLS_RSA_WITH_AES_256_GCM_SHA384:TLS_RSA_WITH_AES_128_GCM_SHA256:TLS_RSA_WITH_AES_256_CBC_SHA:TLS_RSA_WITH_AES_128_CBC_SHA:TLS_ECDHE_ECDSA_WITH_3DES_EDE_CBC_SHA:TLS_ECDHE_RSA_WITH_3DES_EDE_CBC_SHA:TLS_RSA_WITH_3DES_EDE_CBC_SHA"
            )
        );
        assert_eq!(
            tls.sigalgs_list.as_deref(),
            Some(
                "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:ecdsa_sha1:rsa_pss_rsae_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha512:rsa_pkcs1_sha1"
            )
        );
        assert_eq!(tls.min_tls_version, Some(TlsVersion::TLS_1_0));
        assert_eq!(tls.session_ticket, false);
        assert_eq!(tls.alps_use_new_codepoint, false);
        assert_eq!(tls.pre_shared_key, false);
        assert_eq!(tls.enable_ech_grease, false);
        assert_eq!(tls.aes_hw_override, None);
        assert_eq!(tls.permute_extensions, None);
        assert_eq!(tls.alps_protocols.as_deref(), None);
        let http2 = profile.http2_options.as_ref().unwrap();
        assert_eq!(http2.max_concurrent_streams, Some(100));
        assert_eq!(http2.initial_window_size, 2097152);
        assert_eq!(http2.initial_conn_window_size, 10485760);
        assert_eq!(http2.max_header_list_size, None);
        assert_eq!(http2.header_table_size, None);
        assert_eq!(http2.enable_connect_protocol, Some(true));
        assert_eq!(http2.no_rfc7540_priorities, Some(true));
        let order = PseudoOrder::builder()
            .extend([
                PseudoId::Method,
                PseudoId::Scheme,
                PseudoId::Authority,
                PseudoId::Path,
            ])
            .build();
        assert_eq!(
            format!("{:?}", http2.headers_pseudo_order),
            format!("{:?}", Some(order))
        );
        assert_certificate_codec(&profile, CertificateCompressionAlgorithm::ZLIB);
    }

    #[kithara::test]
    fn chrome_profile() {
        let profile: Emulation = super::super::emulate::emulation(ImpersonatePreset::Chrome);
        assert_common(&profile);
        assert_headers(
            &profile,
            &[
                (
                    "sec-ch-ua",
                    "\"Google Chrome\";v=\"137\", \"Chromium\";v=\"137\", \"Not/A)Brand\";v=\"24\"",
                ),
                ("sec-ch-ua-mobile", "?0"),
                ("sec-ch-ua-platform", "\"macOS\""),
                (
                    "user-agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36",
                ),
                ("sec-fetch-dest", "document"),
                ("sec-fetch-mode", "navigate"),
                ("sec-fetch-site", "none"),
                (
                    "accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
                ),
                ("accept-language", "en-US,en;q=0.9"),
                ("priority", "u=0, i"),
            ],
        );
        let tls = profile.tls_options.as_ref().unwrap();
        assert_eq!(
            tls.curves_list.as_deref(),
            Some("X25519MLKEM768:X25519:P-256:P-384")
        );
        assert_eq!(
            tls.cipher_list.as_deref(),
            Some(
                "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:TLS_RSA_WITH_AES_128_GCM_SHA256:TLS_RSA_WITH_AES_256_GCM_SHA384:TLS_RSA_WITH_AES_128_CBC_SHA:TLS_RSA_WITH_AES_256_CBC_SHA"
            )
        );
        assert_eq!(
            tls.sigalgs_list.as_deref(),
            Some(
                "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha512"
            )
        );
        assert_eq!(tls.min_tls_version, Some(TlsVersion::TLS_1_2));
        assert_eq!(tls.session_ticket, true);
        assert_eq!(tls.alps_use_new_codepoint, true);
        assert_eq!(tls.pre_shared_key, true);
        assert_eq!(tls.enable_ech_grease, true);
        assert_eq!(tls.aes_hw_override, Some(true));
        assert_eq!(tls.permute_extensions, Some(true));
        assert_eq!(
            tls.alps_protocols.as_deref(),
            Some([AlpsProtocol::HTTP2].as_slice())
        );
        let http2 = profile.http2_options.as_ref().unwrap();
        assert_eq!(http2.max_concurrent_streams, None);
        assert_eq!(http2.initial_window_size, 6291456);
        assert_eq!(http2.initial_conn_window_size, 15728640);
        assert_eq!(http2.max_header_list_size, Some(262144));
        assert_eq!(http2.header_table_size, Some(65536));
        assert_eq!(http2.enable_connect_protocol, None);
        assert_eq!(http2.no_rfc7540_priorities, None);
        let order = PseudoOrder::builder()
            .extend([
                PseudoId::Method,
                PseudoId::Authority,
                PseudoId::Scheme,
                PseudoId::Path,
            ])
            .build();
        assert_eq!(
            format!("{:?}", http2.headers_pseudo_order),
            format!("{:?}", Some(order))
        );
        assert_certificate_codec(&profile, CertificateCompressionAlgorithm::BROTLI);
    }
}
