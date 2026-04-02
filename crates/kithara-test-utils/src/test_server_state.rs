use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use uuid::Uuid;

use crate::{
    hls_spec::{HlsSpecError, ResolvedHlsSpec, parse_hls_spec_with, resolve_hls_spec_with},
    hls_stream::{GeneratedHls, GeneratedHlsCache, load_hls},
    hls_url::HlsSpec,
    signal_spec::SignalRequest,
};

pub(crate) struct TestServerState {
    tokens: RwLock<HashMap<String, StoredToken>>,
    hls_cache: GeneratedHlsCache,
    hls_blobs: RwLock<HashMap<String, Arc<Vec<u8>>>>,
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
        })
    }

    pub(crate) fn insert_signal(&self, request: SignalRequest) -> String {
        self.insert(StoredToken::Signal(request))
    }

    pub(crate) fn get_signal(&self, token: &str) -> Option<SignalRequest> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Signal(request)) => Some(request.clone()),
            _ => None,
        }
    }

    pub(crate) fn register_hls_blob(&self, bytes: &[u8]) -> String {
        let key = crate::hls_blob_store::blob_key(bytes);
        let mut blobs = self.hls_blobs.write().expect("hls blob store poisoned");
        blobs
            .entry(key.clone())
            .or_insert_with(|| Arc::new(bytes.to_vec()));
        key
    }

    pub(crate) fn insert_hls_spec(&self, spec: HlsSpec) -> Result<String, HlsSpecError> {
        let resolved = self.resolve_hls_spec(spec)?;
        self.insert_hls(resolved)
    }

    pub(crate) fn parse_hls_spec(&self, encoded: &str) -> Result<ResolvedHlsSpec, HlsSpecError> {
        parse_hls_spec_with(encoded, |key| self.resolve_hls_blob(key))
    }

    pub(crate) fn resolve_hls_spec(&self, spec: HlsSpec) -> Result<ResolvedHlsSpec, HlsSpecError> {
        resolve_hls_spec_with(spec, |key| self.resolve_hls_blob(key))
    }

    pub(crate) fn get_hls(&self, token: &str) -> Option<Arc<GeneratedHls>> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Hls(hls)) => Some(Arc::clone(hls)),
            _ => None,
        }
    }

    pub(crate) fn load_hls(
        &self,
        spec: ResolvedHlsSpec,
    ) -> Result<Arc<GeneratedHls>, HlsSpecError> {
        load_hls(&self.hls_cache, spec)
    }

    fn insert_hls(&self, spec: ResolvedHlsSpec) -> Result<String, HlsSpecError> {
        let hls = self.load_hls(spec)?;
        Ok(self.insert(StoredToken::Hls(hls)))
    }

    fn resolve_hls_blob(&self, key: &str) -> Result<Arc<Vec<u8>>, HlsSpecError> {
        let blobs = self.hls_blobs.read().expect("hls blob store poisoned");
        blobs
            .get(key)
            .cloned()
            .ok_or_else(|| HlsSpecError::MissingBlob(key.to_owned()))
    }

    fn insert(&self, value: StoredToken) -> String {
        let token = Uuid::new_v4().to_string();
        let mut store = self.tokens.write().expect("token store poisoned");
        store.insert(token.clone(), value);
        token
    }
}
