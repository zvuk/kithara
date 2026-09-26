use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    source::{
        resolve_uri,
        uri::{LoadedBytes, LoadedSource, SourceResolver},
    },
};

#[derive(Debug, Default)]
pub struct MemResolver {
    blobs: BTreeMap<String, Arc<[u8]>>,
    files: BTreeMap<String, String>,
}

impl MemResolver {
    pub fn insert(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_owned(), text.to_owned());
    }

    /// Adds a source that is not text, such as a picture a skin names.
    pub fn insert_bytes(&mut self, path: &str, bytes: &[u8]) {
        self.blobs.insert(path.to_owned(), Arc::from(bytes));
    }
}

impl SourceResolver for MemResolver {
    fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError> {
        let uri = resolve_uri(base, rel)?;
        let origin = base.cloned().unwrap_or_else(|| uri.clone());
        self.blobs
            .get(&uri.0)
            .map(|bytes| LoadedBytes {
                uri,
                bytes: Arc::clone(bytes),
            })
            .ok_or_else(|| UiDocError::NotFound {
                origin,
                rel: rel.to_owned(),
            })
    }

    fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError> {
        let uri = resolve_uri(base, rel)?;
        let origin = base.cloned().unwrap_or_else(|| uri.clone());
        self.files
            .get(&uri.0)
            .map(|text| LoadedSource {
                uri,
                text: text.clone(),
            })
            .ok_or_else(|| UiDocError::NotFound {
                origin,
                rel: rel.to_owned(),
            })
    }
}
