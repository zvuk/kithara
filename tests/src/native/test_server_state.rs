use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use moka::sync::Cache;
use uuid::Uuid;

use crate::{
    hls_spec::{HlsSpecError, ResolvedHlsSpec, parse_hls_spec_with, resolve_hls_spec_with},
    hls_stream::{GeneratedHls, GeneratedHlsCache, load_hls},
    hls_url::HlsSpec,
    signal_spec::SignalRequest,
};

/// Soft cap on distinct encoded-signal entries kept in memory per
/// `TestServerState`. Each entry is a couple of `MiB` at most; 256
/// leaves plenty of headroom while bounding worst-case test-server
/// memory use.
const ENCODED_SIGNAL_CACHE_CAPACITY: u64 = 256;

#[derive(Clone)]
pub(crate) struct EncodedSignal {
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
}

#[derive(Clone)]
pub enum Content {
    HtmlError(&'static str),
    Status(u16),
}

#[derive(Clone)]
pub enum Delivery {
    Normal,
}

#[derive(Clone)]
pub struct FixtureBehavior {
    pub content: Content,
    pub delivery: Delivery,
}

struct BehaviorEntry {
    behavior: FixtureBehavior,
    hits: AtomicU64,
}

pub(crate) struct TestServerState {
    hls_cache: GeneratedHlsCache,
    hls_blobs: RwLock<HashMap<String, Arc<Vec<u8>>>>,
    tokens: RwLock<HashMap<String, StoredToken>>,
    encoded_signals: Cache<String, EncodedSignal>,
    behaviors: RwLock<HashMap<String, Arc<BehaviorEntry>>>,
}

#[derive(Clone)]
enum StoredToken {
    Signal(SignalRequest),
    Hls(Arc<GeneratedHls>),
}

impl TestServerState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: RwLock::new(HashMap::new()),
            hls_cache: RwLock::new(HashMap::new()),
            hls_blobs: RwLock::new(HashMap::new()),
            encoded_signals: Cache::new(ENCODED_SIGNAL_CACHE_CAPACITY),
            behaviors: RwLock::new(HashMap::new()),
        })
    }

    pub(crate) fn insert_behavior(&self, behavior: FixtureBehavior) -> String {
        let token = Uuid::new_v4().to_string();
        let mut map = self.behaviors.write().expect("behaviors poisoned");
        map.insert(
            token.clone(),
            Arc::new(BehaviorEntry {
                behavior,
                hits: AtomicU64::new(0),
            }),
        );
        token
    }

    pub(crate) fn get_behavior(&self, token: &str) -> Option<FixtureBehavior> {
        let map = self.behaviors.read().expect("behaviors poisoned");
        map.get(token).map(|e| e.behavior.clone())
    }

    pub(crate) fn bump_behavior(&self, token: &str) -> u64 {
        let map = self.behaviors.read().expect("behaviors poisoned");
        match map.get(token) {
            Some(e) => e.hits.fetch_add(1, Ordering::Relaxed) + 1,
            None => 0,
        }
    }

    pub(crate) fn behavior_hits(&self, token: &str) -> Option<u64> {
        let map = self.behaviors.read().expect("behaviors poisoned");
        map.get(token).map(|e| e.hits.load(Ordering::Relaxed))
    }

    /// Lookup a previously encoded signal payload. Test fixture builders
    /// (`TestServerHelper::{sine,sweep,…}`) populate this cache at setup
    /// time via [`Self::insert_encoded_signal`], so request handlers
    /// never encode on the critical path (the production analogue is "a
    /// file already exists on disk"). A miss is a fixture-setup bug.
    pub(crate) fn get_encoded_signal(&self, key: &str) -> Option<EncodedSignal> {
        self.encoded_signals.get(key)
    }

    /// Insert a pre-encoded signal payload. Test helpers call this at
    /// fixture build time so that the request handler can serve range
    /// requests immediately, without inline encoding work.
    pub(crate) fn insert_encoded_signal(&self, key: String, encoded: EncodedSignal) {
        self.encoded_signals.insert(key, encoded);
    }

    pub(crate) fn get_hls(&self, token: &str) -> Option<Arc<GeneratedHls>> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Hls(hls)) => Some(Arc::clone(hls)),
            _ => None,
        }
    }

    pub(crate) fn get_signal(&self, token: &str) -> Option<SignalRequest> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Signal(request)) => Some(request.clone()),
            _ => None,
        }
    }

    fn insert(&self, value: StoredToken) -> String {
        let token = Uuid::new_v4().to_string();
        let mut store = self.tokens.write().expect("token store poisoned");
        store.insert(token.clone(), value);
        token
    }

    fn insert_hls(&self, spec: ResolvedHlsSpec) -> Result<String, HlsSpecError> {
        let hls = self.load_hls(spec)?;
        Ok(self.insert(StoredToken::Hls(hls)))
    }

    pub(crate) fn insert_hls_spec(&self, spec: HlsSpec) -> Result<String, HlsSpecError> {
        let resolved = self.resolve_hls_spec(spec)?;
        self.insert_hls(resolved)
    }

    pub(crate) fn insert_signal(&self, request: SignalRequest) -> String {
        self.insert(StoredToken::Signal(request))
    }

    pub(crate) fn load_hls(
        &self,
        spec: ResolvedHlsSpec,
    ) -> Result<Arc<GeneratedHls>, HlsSpecError> {
        load_hls(&self.hls_cache, spec)
    }

    pub(crate) fn parse_hls_spec(&self, encoded: &str) -> Result<ResolvedHlsSpec, HlsSpecError> {
        parse_hls_spec_with(encoded, |key| self.resolve_hls_blob(key))
    }

    pub(crate) fn register_hls_blob(&self, bytes: &[u8]) -> String {
        let key = crate::hls_blob_store::blob_key(bytes);
        let mut blobs = self.hls_blobs.write().expect("hls blob store poisoned");
        blobs
            .entry(key.clone())
            .or_insert_with(|| Arc::new(bytes.to_vec()));
        key
    }

    fn resolve_hls_blob(&self, key: &str) -> Result<Arc<Vec<u8>>, HlsSpecError> {
        let blobs = self.hls_blobs.read().expect("hls blob store poisoned");
        blobs
            .get(key)
            .cloned()
            .ok_or_else(|| HlsSpecError::MissingBlob(key.to_owned()))
    }

    pub(crate) fn resolve_hls_spec(&self, spec: HlsSpec) -> Result<ResolvedHlsSpec, HlsSpecError> {
        resolve_hls_spec_with(spec, |key| self.resolve_hls_blob(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behavior_register_returns_token_and_counts_start_at_zero() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::HtmlError("<html>captive</html>"),
            delivery: Delivery::Normal,
        });
        assert_eq!(state.behavior_hits(&token), Some(0));
        assert!(state.get_behavior(&token).is_some());
        assert_eq!(state.behavior_hits("nonexistent"), None);
    }

    #[test]
    fn behavior_bump_increments_count() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::Status(503),
            delivery: Delivery::Normal,
        });
        state.bump_behavior(&token);
        state.bump_behavior(&token);
        assert_eq!(state.behavior_hits(&token), Some(2));
    }
}
