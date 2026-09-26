use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs, io,
    io::{Error, ErrorKind},
    path::{Path, PathBuf},
};

use kithara_platform::sync::Arc;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    source::{
        resolve_uri,
        uri::{LoadedBytes, LoadedSource, SourceResolver},
    },
};

/// Reads sources from one directory on disk.
///
/// The root is made real once, when the resolver is built, so every later read
/// compares against a path that already exists. A name reaching outside it is
/// refused on the same terms whether it spelled its way out with `..` or was
/// led out by a symlink.
///
/// What it has read, it keeps. A document graph names the same module from
/// several places, and parsing is already deduplicated by uri while reading is
/// not; without this the application would open a third of its files twice and
/// the gallery nearly two thirds. A name that was not there is not kept, so a
/// file appearing later is still found.
#[derive(Debug)]
pub struct FileResolver {
    root: PathBuf,
    blobs: RefCell<BTreeMap<String, Arc<[u8]>>>,
    texts: RefCell<BTreeMap<String, String>>,
}

impl FileResolver {
    /// Opens `root` as the directory every name is resolved against.
    ///
    /// # Errors
    /// Returns the io error when `root` cannot be made real.
    pub fn new<P: AsRef<Path>>(root: P) -> io::Result<Self> {
        Ok(Self {
            blobs: RefCell::default(),
            root: fs::canonicalize(root)?,
            texts: RefCell::default(),
        })
    }

    fn real(&self, origin: &SourceUri, uri: &SourceUri, rel: &str) -> Result<PathBuf, UiDocError> {
        let real = fs::canonicalize(self.root.join(&uri.0))
            .map_err(|error| refusal(origin.clone(), rel, &error))?;
        if real.starts_with(&self.root) {
            Ok(real)
        } else {
            Err(UiDocError::RootEscape {
                origin: origin.clone(),
                rel: rel.to_owned(),
            })
        }
    }
}

/// Tells a name that is not there from one that is there and would not open.
fn refusal(origin: SourceUri, rel: &str, error: &Error) -> UiDocError {
    if error.kind() == ErrorKind::NotFound {
        return UiDocError::NotFound {
            origin,
            rel: rel.to_owned(),
        };
    }
    UiDocError::Unreadable {
        origin,
        rel: rel.to_owned(),
        source: Error::new(error.kind(), error.to_string()),
    }
}

impl SourceResolver for FileResolver {
    fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError> {
        let uri = resolve_uri(base, rel)?;
        if let Some(bytes) = self.blobs.borrow().get(&uri.0) {
            return Ok(LoadedBytes {
                uri,
                bytes: Arc::clone(bytes),
            });
        }
        let origin = base.cloned().unwrap_or_else(|| uri.clone());
        let path = self.real(&origin, &uri, rel)?;
        let read = fs::read(path).map_err(|error| refusal(origin, rel, &error))?;
        let bytes = Arc::<[u8]>::from(read.as_slice());
        self.blobs
            .borrow_mut()
            .insert(uri.0.clone(), Arc::clone(&bytes));
        Ok(LoadedBytes { bytes, uri })
    }

    fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError> {
        let uri = resolve_uri(base, rel)?;
        if let Some(text) = self.texts.borrow().get(&uri.0) {
            return Ok(LoadedSource {
                uri,
                text: text.clone(),
            });
        }
        let origin = base.cloned().unwrap_or_else(|| uri.clone());
        let path = self.real(&origin, &uri, rel)?;
        let text = fs::read_to_string(path).map_err(|error| refusal(origin, rel, &error))?;
        self.texts.borrow_mut().insert(uri.0.clone(), text.clone());
        Ok(LoadedSource { uri, text })
    }
}
