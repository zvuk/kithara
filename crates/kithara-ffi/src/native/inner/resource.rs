use std::collections::HashMap;

use bytes::Bytes;
use dashmap::DashMap;
use kithara::{
    abr::AbrMode,
    download::Downloader,
    drm::{KeyProcessor, KeyRequest, KeyRequestFactory},
    events::{EventBus, ScopeLabel},
    hls::{KeyOptions, KeyProcessorRegistry},
    platform::sync::{Arc, Mutex},
    play::{
        ResourceSrc,
        policy::{DomainKeyPolicy, DomainKeyRule},
    },
};

use crate::{
    asset::FfiAssetStore,
    item::AudioPlayerItem,
    native::salt,
    observer::{AUTH_TOKEN_HEADER, FfiKeyProcessor, SALT_HEADER},
    pools::{FfiResourceConfig, FfiTrackSource},
    types::{FfiAbrMode, FfiError, FfiKeyOptions, FfiKeyRule},
};

/// Prepares track resources from the player-wide store and network policy.
/// Items snapshot headers and keys when their resource configuration is built.
pub(super) struct ResourceFactory {
    /// Released before network handles during player teardown.
    store: Arc<FfiAssetStore>,
    player_headers: DashMap<String, String>,
    downloader: Downloader,
    key_options: Mutex<KeyOptions>,
}

impl ResourceFactory {
    pub(super) fn new(
        store: Arc<FfiAssetStore>,
        downloader: Downloader,
        keys: FfiKeyOptions,
    ) -> Self {
        let (key_options, player_headers) = build_initial_key_state(keys);
        Self {
            store,
            player_headers: player_headers.into_iter().collect(),
            downloader,
            key_options: Mutex::new(key_options),
        }
    }

    pub(super) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = salt::drm_lowercase_hex_salt();
        let mut rule_headers = HashMap::new();
        rule_headers.insert(SALT_HEADER.to_string(), salt.clone());
        let rule = FfiKeyRule {
            processor,
            headers: Some(rule_headers),
            query_params: None,
            domains: vec!["*".to_string()],
            salt: Some(salt),
        };
        self.setup_hls_aes_with_rule(rule);
    }

    pub(super) fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule) {
        if let Some(headers) = rule.headers.as_ref() {
            for (k, v) in headers {
                self.player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = rule.salt.as_ref() {
            self.player_headers
                .insert(SALT_HEADER.to_string(), salt.clone());
        }

        let processor_rule = build_processor_rule(rule);
        let mut opts = self.key_options.lock();
        let mut registry = opts.key_registry.take().unwrap_or_default();
        registry.register(Arc::new(DomainKeyPolicy::new([processor_rule])));
        *opts = KeyOptions::builder().key_registry(registry).build();
    }

    pub(super) fn setup_network(&self, auth_token: String) {
        if auth_token.is_empty() {
            self.player_headers.remove(AUTH_TOKEN_HEADER);
        } else {
            self.player_headers
                .insert(AUTH_TOKEN_HEADER.to_string(), auth_token);
        }
    }

    /// Build an [`FfiTrackSource::Config`] from the item's fields. Also attaches
    /// a scoped bus so the item's per-resource event bridge captures events
    /// published during `Resource::new` (`VariantsDiscovered` fires
    /// synchronously during stream open; a late subscriber would miss it).
    pub(super) fn source(
        &self,
        item: &AudioPlayerItem,
        events: &EventBus,
    ) -> Result<FfiTrackSource, FfiError> {
        let scoped = events.scoped_labeled(ScopeLabel {
            track: Some(item.track_id()),
            ..ScopeLabel::default()
        });
        let abr_mode = item.abr_mode().map(|mode| match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
        });
        let src = ResourceSrc::parse(item.url()).map_err(|e| FfiError::InvalidArgument {
            reason: e.to_string(),
        })?;
        let config = FfiResourceConfig::for_src(src)
            .preferred_peak_bitrate(item.preferred_peak_bitrate().max(0.0))
            .maybe_headers(self.merged_headers(item).map(Into::into))
            .events(scoped.clone())
            .downloader(self.downloader.clone())
            .store(self.store.handle().clone())
            .keys(self.key_options.lock().clone())
            .initial_abr_mode(abr_mode.unwrap_or_default())
            .build();
        *item.bus.lock() = Some(scoped);

        Ok(FfiTrackSource::Config(Box::new(config)))
    }

    /// Merge player-wide auth and salt headers into the item's own headers.
    /// Item-supplied entries win on key collision so callers can override
    /// player defaults per-item.
    fn merged_headers(&self, item: &AudioPlayerItem) -> Option<HashMap<String, String>> {
        let item_headers = item.headers();
        if self.player_headers.is_empty() && item_headers.is_none() {
            return None;
        }
        let mut merged: HashMap<String, String> = self
            .player_headers
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        if let Some(item_h) = item_headers {
            merged.extend(item_h);
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    }
}

fn build_processor_closure(processor: Arc<dyn FfiKeyProcessor>, salt: String) -> KeyProcessor {
    Arc::new(move |key: Bytes| {
        Ok(Bytes::from(
            processor.process_key(key.to_vec(), salt.clone()),
        ))
    })
}

/// A caller-provided salt remains fixed for legacy FFI compatibility.
fn build_processor_rule(rule: FfiKeyRule) -> DomainKeyRule {
    let processor = rule.processor;
    let salt_template = rule.salt.unwrap_or_default();
    let factory: KeyRequestFactory = Arc::new(move || {
        let salt = salt_template.clone();
        let mut headers = HashMap::new();
        headers.insert(SALT_HEADER.to_string(), salt.clone());
        let proc = build_processor_closure(Arc::clone(&processor), salt);
        KeyRequest::new(headers, proc)
    });
    DomainKeyRule::for_domains(&rule.domains, factory)
        .maybe_headers(rule.headers)
        .maybe_query_params(rule.query_params)
        .build()
}

/// Convert the FFI-level [`crate::types::FfiKeyOptions`] into the
/// initial registry + the player-wide header snapshot to expose to
/// outgoing HTTP requests.
fn build_initial_key_state(ffi: FfiKeyOptions) -> (KeyOptions, HashMap<String, String>) {
    if ffi.rules.is_empty() {
        return (KeyOptions::default(), HashMap::new());
    }
    let mut registry = KeyProcessorRegistry::new();
    let mut player_headers: HashMap<String, String> = HashMap::new();
    let mut rules: Vec<DomainKeyRule> = Vec::with_capacity(ffi.rules.len());
    for r in ffi.rules {
        if let Some(headers) = r.headers.as_ref() {
            for (k, v) in headers {
                player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = r.salt.as_ref() {
            player_headers.insert(SALT_HEADER.to_string(), salt.clone());
        }
        rules.push(build_processor_rule(r));
    }
    registry.register(Arc::new(DomainKeyPolicy::new(rules)));
    let key_options = KeyOptions::builder().key_registry(registry).build();
    (key_options, player_headers)
}

#[cfg(test)]
mod tests {
    use unimock::Unimock;

    use super::{super::NativeInner, *};
    use crate::config::FfiPlayerConfig;

    struct TaggedProcessor(u8);

    impl FfiKeyProcessor for TaggedProcessor {
        fn process_key(&self, _key: Vec<u8>, _salt: String) -> Vec<u8> {
            vec![self.0]
        }
    }

    fn tagged_rule(tag: u8, salt: &str, domains: &[&str]) -> FfiKeyRule {
        FfiKeyRule {
            processor: Arc::new(TaggedProcessor(tag)),
            headers: Some(HashMap::from([("X-Provider".to_string(), tag.to_string())])),
            query_params: None,
            domains: domains.iter().map(ToString::to_string).collect(),
            salt: Some(salt.to_string()),
        }
    }

    #[kithara::test]
    fn shared_store_outlives_each_player() {
        crate::mock::offline();
        let store = Arc::new(FfiAssetStore::for_test());
        let cancel = store.cancel_token();
        let config = |store| FfiPlayerConfig {
            store,
            ..FfiPlayerConfig::for_test()
        };
        let first = NativeInner::new(config(Arc::clone(&store))).expect("create first player");
        let second = NativeInner::new(config(Arc::clone(&store))).expect("create second player");

        assert!(Arc::ptr_eq(&first.resources.store, &second.resources.store));
        assert!(
            first
                .resources
                .store
                .handle()
                .is_same(second.resources.store.handle())
        );

        drop(store);
        drop(first);
        assert!(!cancel.is_cancelled());

        drop(second);
        assert!(cancel.is_cancelled());
    }

    #[kithara::test]
    fn initial_key_rules_keep_policy_order_and_global_header_semantics() {
        let ffi = FfiKeyOptions {
            rules: vec![
                tagged_rule(1, "first-salt", &["keys.example.com"]),
                tagged_rule(2, "second-salt", &["*"]),
            ],
        };

        let (options, player_headers) = build_initial_key_state(ffi);

        assert_eq!(
            player_headers.get(SALT_HEADER).map(String::as_str),
            Some("second-salt"),
            "player-wide headers retain their existing last-rule-wins merge"
        );
        assert_eq!(
            player_headers.get("X-Provider").map(String::as_str),
            Some("2")
        );

        let registry = options.key_registry.expect("registry populated");
        let url = url::Url::parse("https://keys.example.com/key").expect("valid key URL");
        let request = registry.prepare(&url).expect("matching key request");

        assert_eq!(
            request.headers.get(SALT_HEADER).map(String::as_str),
            Some("first-salt"),
            "the first matching domain rule prepares the key request"
        );
        assert_eq!(
            request.headers.get("X-Provider").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            (request.processor)(Bytes::from_static(b"encrypted")).expect("processor succeeds"),
            Bytes::from_static(&[1])
        );
    }

    #[kithara::test]
    fn runtime_key_rules_append_in_registration_order() {
        crate::mock::offline();
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_hls_aes_with_rule(tagged_rule(1, "first-salt", &["keys.example.com"]));
        inner.setup_hls_aes_with_rule(tagged_rule(2, "second-salt", &["*"]));

        let registry = inner
            .resources
            .key_options
            .lock()
            .key_registry
            .clone()
            .expect("registry populated");
        let url = url::Url::parse("https://keys.example.com/key").expect("valid key URL");
        let request = registry.prepare(&url).expect("matching key request");

        assert_eq!(
            request.headers.get(SALT_HEADER).map(String::as_str),
            Some("first-salt")
        );
        assert_eq!(
            (request.processor)(Bytes::from_static(b"encrypted")).expect("processor succeeds"),
            Bytes::from_static(&[1])
        );
        assert_eq!(
            inner
                .resources
                .player_headers
                .get(SALT_HEADER)
                .map(|header| header.value().clone())
                .as_deref(),
            Some("second-salt")
        );
    }

    #[kithara::test]
    fn setup_network_writes_auth_token_into_player_headers() {
        crate::mock::offline();
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_network("token-123".to_string());
        let token = inner
            .resources
            .player_headers
            .get(AUTH_TOKEN_HEADER)
            .map(|r| r.value().clone());
        assert_eq!(token.as_deref(), Some("token-123"));
    }

    #[kithara::test]
    fn setup_network_clears_auth_token_when_empty() {
        crate::mock::offline();
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_network("token-123".to_string());
        inner.setup_network(String::new());
        assert!(
            !inner
                .resources
                .player_headers
                .contains_key(AUTH_TOKEN_HEADER)
        );
    }

    #[kithara::test]
    fn setup_hls_aes_registers_wildcard_rule_with_prod_salt() {
        crate::mock::offline();
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_hls_aes(Arc::new(Unimock::new(())));

        let salt = inner
            .resources
            .player_headers
            .get(SALT_HEADER)
            .map(|r| r.value().clone())
            .expect("salt header populated");
        assert_eq!(salt.len(), 8, "prod auto-salt length");
        assert!(
            salt.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "prod auto-salt must be lowercase hex, got {salt:?}"
        );

        let key_options = inner.resources.key_options.lock().clone();
        assert!(
            key_options.key_registry.is_some(),
            "registry must hold the wildcard rule"
        );
    }
}
