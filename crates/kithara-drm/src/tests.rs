mod decrypt {
    use aes::Aes128;
    use cbc::{
        Encryptor,
        cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
    };
    use kithara_test_utils::kithara;

    use crate::{DecryptContext, aes128_cbc_process_chunk, decrypt::AES_BLOCK_SIZE};

    fn encrypt_aes128_cbc(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
        let padded_len = plaintext.len() + (AES_BLOCK_SIZE - plaintext.len() % AES_BLOCK_SIZE);
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = encryptor
            .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
            .expect("encrypt_padded failed");
        ct.to_vec()
    }

    fn repeating_bytes(len: usize) -> Vec<u8> {
        (0u8..=u8::MAX).cycle().take(len).collect()
    }

    /// Roundtrip: encrypt → decrypt single chunk.
    #[kithara::test(wasm)]
    #[case::hello(b"Hello, DRM world! This is a test of AES-128-CBC.".as_slice(), [0x42u8; 16], [0x13u8; 16])]
    #[case::exact_block(&[0x55u8; 16], [0xAAu8; 16], [0xBBu8; 16])]
    fn test_single_chunk_roundtrip(
        #[case] plaintext: &[u8],
        #[case] key: [u8; 16],
        #[case] iv: [u8; 16],
    ) {
        let ciphertext = encrypt_aes128_cbc(plaintext, &key, &iv);
        let mut ctx = DecryptContext::new(key, iv);

        let mut output = vec![0u8; ciphertext.len()];
        let written = aes128_cbc_process_chunk(&ciphertext, &mut output, &mut ctx, true).unwrap();

        assert_eq!(&output[..written], plaintext);
    }

    #[kithara::test]
    fn test_single_chunk_roundtrip_large() {
        let key = [0x01u8; 16];
        let iv = [0x02u8; 16];
        let plaintext = repeating_bytes(1000);

        let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);
        let mut ctx = DecryptContext::new(key, iv);

        let mut output = vec![0u8; ciphertext.len()];
        let written = aes128_cbc_process_chunk(&ciphertext, &mut output, &mut ctx, true).unwrap();

        assert_eq!(written, plaintext.len());
        assert_eq!(&output[..written], &plaintext[..]);
    }

    #[kithara::test]
    fn test_empty_input() {
        let mut ctx = DecryptContext::new([0u8; 16], [0u8; 16]);
        let mut output = [0u8; 16];
        let written = aes128_cbc_process_chunk(&[], &mut output, &mut ctx, true).unwrap();
        assert_eq!(written, 0);
    }

    #[kithara::test]
    fn test_unaligned_input_fails() {
        let mut ctx = DecryptContext::new([0u8; 16], [0u8; 16]);
        let input = [0u8; 15];
        let mut output = [0u8; 15];
        let result = aes128_cbc_process_chunk(&input, &mut output, &mut ctx, false);
        assert!(result.is_err());
    }

    /// Multi-chunk CBC IV chaining.
    #[kithara::test(wasm)]
    #[case::small_2_chunks(48, 32)]
    #[case::large_4_chunks(256, 64)]
    #[case::uneven_3_chunks(160, 48)]
    fn test_multi_chunk_cbc_chaining(#[case] plaintext_len: usize, #[case] chunk_size: usize) {
        let key = [0x77u8; 16];
        let iv = [0x33u8; 16];

        let plaintext = repeating_bytes(plaintext_len);
        let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);

        let mut ctx = DecryptContext::new(key, iv);
        let mut decrypted = Vec::new();

        let total = ciphertext.len();
        let mut offset = 0;
        while offset < total {
            let end = (offset + chunk_size).min(total);
            let is_last = end == total;
            let chunk = &ciphertext[offset..end];
            let mut output = vec![0u8; chunk.len()];
            let written = aes128_cbc_process_chunk(chunk, &mut output, &mut ctx, is_last).unwrap();
            decrypted.extend_from_slice(&output[..written]);
            offset = end;
        }

        assert_eq!(decrypted, plaintext);
    }
}

mod registry {
    use std::{collections::HashMap, sync::Arc};

    use bytes::Bytes;
    use kithara_test_utils::kithara;
    use url::Url;

    use crate::{DomainMatcher, KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};

    fn noop_processor() -> KeyProcessor {
        Arc::new(Ok)
    }

    fn reverse_processor() -> KeyProcessor {
        Arc::new(|key| {
            let mut v = key.to_vec();
            v.reverse();
            Ok(Bytes::from(v))
        })
    }

    #[kithara::test]
    fn exact_match() {
        let m = DomainMatcher::parse("zvuk.com");
        assert!(m.matches("zvuk.com"));
        assert!(m.matches("ZVUK.COM"));
        assert!(!m.matches("cdn.zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[kithara::test]
    fn wildcard_match() {
        let m = DomainMatcher::parse("*.zvuk.com");
        assert!(m.matches("cdn.zvuk.com"));
        assert!(m.matches("edge.cdn.zvuk.com"));
        assert!(!m.matches("zvuk.com"));
        assert!(!m.matches("nozvuk.com"));
    }

    #[kithara::test]
    fn wildcard_case_insensitive() {
        let m = DomainMatcher::parse("*.ZVQ.ME");
        assert!(m.matches("cdn-edge.zvq.me"));
        assert!(m.matches("CDN-EDGE.ZVQ.ME"));
    }

    #[kithara::test]
    fn parse_wildcard_all() {
        assert!(matches!(DomainMatcher::parse("*"), DomainMatcher::All));
    }

    #[kithara::test]
    fn wildcard_all_matches_any_host() {
        let m = DomainMatcher::parse("*");
        assert!(m.matches("zvuk.com"));
        assert!(m.matches("cdn.zvq.me"));
        assert!(m.matches("a.b.c.d.example.org"));
        assert!(m.matches("LOCALHOST"));
    }

    #[kithara::test]
    fn registry_find_with_wildcard_all() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(["*"], noop_processor()));

        for host in ["cdn.zvuk.com", "silvercomet.top", "example.org"] {
            let url = Url::parse(&format!("https://{host}/key.bin")).expect("test URL is valid");
            assert!(reg.find(&url).is_some());
        }
    }

    #[kithara::test]
    fn registry_specific_rule_overrides_wildcard_all_when_registered_first() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(["*.zvuk.com"], reverse_processor()));
        reg.add(KeyProcessorRule::new(["*"], noop_processor()));

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("test URL is valid");
        let rule = reg.find(&url).expect("matched");
        let result = rule.processor()(Bytes::from_static(b"abcd")).expect("ok");
        assert_eq!(&result[..], b"dcba");
    }

    #[kithara::test]
    fn rule_builder_sets_headers_and_query_params() {
        let mut headers = HashMap::new();
        headers.insert("X-Encrypted-Key".to_string(), "seed123".to_string());
        let mut params = HashMap::new();
        params.insert("token".to_string(), "abc".to_string());

        let rule = KeyProcessorRule::new(["*.zvuk.com"], noop_processor())
            .with_headers(headers.clone())
            .with_query_params(params.clone());

        assert_eq!(rule.headers.as_ref().expect("headers"), &headers);
        assert_eq!(rule.query_params.as_ref().expect("params"), &params);
    }

    #[kithara::test]
    fn registry_find_first_match() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(
            ["*.zvuk.com", "*.zvq.me"],
            noop_processor(),
        ));
        reg.add(KeyProcessorRule::new(["other.com"], reverse_processor()));

        let url = Url::parse("https://cdn-edge.zvq.me/keys/track.key").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://other.com/key.bin").expect("url");
        assert!(reg.find(&url).is_some());

        let url = Url::parse("https://unknown.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[kithara::test]
    fn registry_returns_per_rule_headers() {
        let mut reg = KeyProcessorRegistry::new();
        let mut headers = HashMap::new();
        headers.insert("X-Encrypted-Key".to_string(), "seed123".to_string());
        reg.add(
            KeyProcessorRule::new(["*.zvuk.com"], noop_processor()).with_headers(headers.clone()),
        );
        reg.add(KeyProcessorRule::new(["silvercomet.top"], noop_processor()));

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert_eq!(
            rule.headers.as_ref().expect("headers")["X-Encrypted-Key"],
            "seed123"
        );

        let url = Url::parse("https://silvercomet.top/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert!(rule.headers.is_none());
    }

    #[kithara::test]
    fn registry_returns_per_rule_query_params() {
        let mut reg = KeyProcessorRegistry::new();
        let mut params = HashMap::new();
        params.insert("token".to_string(), "xyz".to_string());
        reg.add(
            KeyProcessorRule::new(["*.zvuk.com"], noop_processor())
                .with_query_params(params.clone()),
        );
        reg.add(KeyProcessorRule::new(["silvercomet.top"], noop_processor()));

        let url = Url::parse("https://cdn.zvuk.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert_eq!(rule.query_params.as_ref().expect("params")["token"], "xyz");

        let url = Url::parse("https://silvercomet.top/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        assert!(rule.query_params.is_none());
    }

    #[kithara::test]
    fn registry_processor_transforms_key() {
        let mut reg = KeyProcessorRegistry::new();
        reg.add(KeyProcessorRule::new(["example.com"], reverse_processor()));

        let url = Url::parse("https://example.com/key.bin").expect("url");
        let rule = reg.find(&url).expect("matched");
        let result = rule.processor()(Bytes::from_static(b"abcd")).expect("ok");
        assert_eq!(&result[..], b"dcba");
    }

    #[kithara::test]
    fn no_match_returns_none() {
        let reg = KeyProcessorRegistry::new();
        let url = Url::parse("https://example.com/key.bin").expect("url");
        assert!(reg.find(&url).is_none());
    }

    #[kithara::test]
    fn empty_registry() {
        let reg = KeyProcessorRegistry::new();
        assert!(reg.is_empty());
    }
}
