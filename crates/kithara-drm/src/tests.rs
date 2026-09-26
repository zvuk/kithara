mod decrypt {
    use aes::Aes128;
    use cbc::{
        Encryptor,
        cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
    };
    use kithara_test_utils::kithara;

    use crate::{DecryptContext, aes128_cbc_process_chunk, consts};

    fn encrypt_aes128_cbc(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
        let padded_len =
            plaintext.len() + (consts::AES_BLOCK_SIZE - plaintext.len() % consts::AES_BLOCK_SIZE);
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
    use std::collections::HashMap;

    use bytes::Bytes;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;
    use url::Url;

    use crate::{KeyProcessor, KeyProcessorRegistry, KeyRequestResolver, PreparedKeyRequest};

    #[derive(Clone, Copy, Debug)]
    enum ProcessorKind {
        Identity,
        Reverse,
    }

    impl ProcessorKind {
        fn build(self) -> KeyProcessor {
            match self {
                Self::Identity => Arc::new(Ok),
                Self::Reverse => Arc::new(|key| {
                    let mut bytes = key.to_vec();
                    bytes.reverse();
                    Ok(Bytes::from(bytes))
                }),
            }
        }
    }

    #[derive(Debug)]
    struct FakeResolver {
        headers: HashMap<String, String>,
        processor: ProcessorKind,
        final_url: Url,
        matches: bool,
    }

    impl FakeResolver {
        fn matching(final_url: &str, processor: ProcessorKind) -> Self {
            Self {
                processor,
                final_url: Url::parse(final_url).expect("test URL is valid"),
                headers: HashMap::new(),
                matches: true,
            }
        }

        fn not_matching() -> Self {
            Self {
                final_url: Url::parse("https://unused.example/key").expect("test URL is valid"),
                headers: HashMap::new(),
                matches: false,
                processor: ProcessorKind::Identity,
            }
        }

        fn with_header(mut self, name: &str, value: &str) -> Self {
            self.headers.insert(name.to_string(), value.to_string());
            self
        }
    }

    impl KeyRequestResolver for FakeResolver {
        fn prepare(&self, _key_url: &Url) -> Option<PreparedKeyRequest> {
            self.matches.then(|| {
                PreparedKeyRequest::new(
                    self.final_url.clone(),
                    self.headers.clone(),
                    self.processor.build(),
                )
            })
        }
    }

    #[kithara::test]
    fn registry_uses_first_resolver_that_prepares_a_request() {
        let mut reg = KeyProcessorRegistry::new();
        reg.register(Arc::new(FakeResolver::not_matching()));
        reg.register(Arc::new(FakeResolver::matching(
            "https://first.example/key",
            ProcessorKind::Identity,
        )));
        reg.register(Arc::new(FakeResolver::matching(
            "https://second.example/key",
            ProcessorKind::Identity,
        )));

        let source = Url::parse("https://source.example/key").expect("test URL is valid");
        let prepared = reg.prepare(&source).expect("resolver should match");

        assert_eq!(prepared.url.as_str(), "https://first.example/key");
    }

    #[kithara::test]
    fn registry_returns_prepared_wire_request_and_selected_processor() {
        let mut reg = KeyProcessorRegistry::new();
        reg.register(Arc::new(
            FakeResolver::matching(
                "https://wire.example/key?session=fresh",
                ProcessorKind::Reverse,
            )
            .with_header("X-Encrypted-Key", "fresh-salt"),
        ));

        let source = Url::parse("https://source.example/key").expect("test URL is valid");
        let prepared = reg.prepare(&source).expect("resolver should match");

        assert_eq!(
            prepared.url.as_str(),
            "https://wire.example/key?session=fresh"
        );
        assert_eq!(prepared.headers["X-Encrypted-Key"], "fresh-salt");
        let result = (prepared.processor)(Bytes::from_static(b"abcd")).expect("processor succeeds");
        assert_eq!(&result[..], b"dcba");
    }

    #[kithara::test]
    fn registry_returns_none_when_no_resolver_matches() {
        let mut reg = KeyProcessorRegistry::new();
        reg.register(Arc::new(FakeResolver::not_matching()));

        let source = Url::parse("https://source.example/key").expect("test URL is valid");

        assert!(reg.prepare(&source).is_none());
    }

    #[kithara::test]
    fn registry_reports_whether_it_has_resolvers() {
        let mut reg = KeyProcessorRegistry::new();
        assert!(reg.is_empty());

        reg.register(Arc::new(FakeResolver::not_matching()));

        assert!(!reg.is_empty());
    }

    #[kithara::test]
    fn registry_debug_does_not_format_resolvers() {
        let mut reg = KeyProcessorRegistry::new();
        reg.register(Arc::new(
            FakeResolver::matching(
                "https://wire.example/key?access_token=secret-query",
                ProcessorKind::Identity,
            )
            .with_header("Authorization", "secret-header"),
        ));

        let debug = format!("{reg:?}");

        assert!(debug.contains("resolver_count"));
        assert!(!debug.contains("secret-query"));
        assert!(!debug.contains("secret-header"));
    }

    #[kithara::test]
    fn prepared_request_debug_redacts_secrets() {
        let url = Url::parse(
            "https://api-user:api-password@example.com/key?access_token=secret-query-value",
        )
        .expect("test URL is valid");
        let headers = HashMap::from([(
            "Authorization".to_string(),
            "secret-header-value".to_string(),
        )]);
        let request = PreparedKeyRequest::new(url, headers, ProcessorKind::Identity.build());

        let debug = format!("{request:?}");

        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("api-user"));
        assert!(!debug.contains("api-password"));
        assert!(!debug.contains("secret-query-value"));
        assert!(!debug.contains("secret-header-value"));
    }
}
