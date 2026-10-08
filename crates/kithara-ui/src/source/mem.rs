use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    source::{
        resolve_uri,
        uri::{Fill, FillDocument, LoadedBytes, LoadedSource, SourceResolver},
    },
};

#[derive(Debug, Default)]
pub struct MemResolver {
    blobs: BTreeMap<String, Arc<[u8]>>,
    files: BTreeMap<String, String>,
    fills: Vec<Fill>,
}

impl MemResolver {
    pub fn insert(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_owned(), text.to_owned());
    }

    /// Appends a fill to `<module id>/<collection>` in registration order.
    pub fn fill(&mut self, address: &str, key: &str, document: FillDocument) {
        self.fills.push(Fill {
            document,
            address: address.to_owned(),
            key: key.to_owned(),
        });
    }

    /// Adds a source that is not text, such as a picture a skin names.
    pub fn insert_bytes(&mut self, path: &str, bytes: &[u8]) {
        self.blobs.insert(path.to_owned(), Arc::from(bytes));
    }
}

impl SourceResolver for MemResolver {
    fn fills(&self) -> Vec<&Fill> {
        self.fills.iter().collect()
    }

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
        let text = self
            .files
            .get(&uri.0)
            .cloned()
            .ok_or_else(|| UiDocError::NotFound {
                origin: base.cloned().unwrap_or_else(|| uri.clone()),
                rel: rel.to_owned(),
            })?;
        Ok(LoadedSource { uri, text })
    }
}
