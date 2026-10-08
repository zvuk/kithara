use kithara_platform::sync::Arc;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    module::{ModuleDoc, parse_module},
};

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LoadedSource {
    pub uri: SourceUri,
    pub text: String,
}

/// A source that is not text: the bytes, and where they came from.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LoadedBytes {
    pub bytes: Arc<[u8]>,
    pub uri: SourceUri,
}

/// A module supplied as authored text or a ready document.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ModuleSource {
    Text(String),
    Document(Box<ModuleDoc>),
}

/// A module and its resolved source location.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LoadedModule {
    pub uri: SourceUri,
    pub source: ModuleSource,
}

/// Fill supplied as a parsed module or a path resolved through the package.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FillDocument {
    /// Parsed module. Its origin must identify one document and differ from package paths.
    Parsed {
        document: Box<ModuleDoc>,
        origin: SourceUri,
    },
    /// Package-relative path; package overlays apply.
    Path(String),
}

impl FillDocument {
    /// Parses a fill module with the given diagnostic and relative-path origin.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when `text` does not parse as a module.
    pub fn parse(text: &str, origin: SourceUri) -> Result<Self, UiDocError> {
        let document = parse_module(text, &origin)?;
        Ok(Self::Parsed {
            document: Box::new(document),
            origin,
        })
    }
}

/// Document registered under a unique key in `<module id>/<collection>`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Fill {
    pub address: String,
    pub key: String,
    pub document: FillDocument,
}

pub trait SourceResolver {
    /// Registered fills in insertion order.
    fn fills(&self) -> Vec<&Fill>;

    /// Loads a module's text or a ready document at the same resolved path.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when the source is unavailable or escapes the root.
    fn module(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedModule, UiDocError> {
        let loaded = self.load(base, rel)?;
        Ok(LoadedModule {
            uri: loaded.uri,
            source: ModuleSource::Text(loaded.text),
        })
    }

    /// Loads `rel` as bytes, resolved against `base` on the same terms.
    ///
    /// A picture is not a document: a skin that names one reads it through
    /// this door rather than the one every text source comes through, because
    /// PNG bytes are not valid UTF-8 and would be refused on the way in.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when the path escapes the root or is unavailable.
    fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError>;

    /// Loads `rel`, resolved against the directory containing `base`.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when the path escapes the root or is unavailable.
    fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError>;
}

pub(crate) fn base_dir(base: Option<&SourceUri>) -> &str {
    let Some(base) = base else {
        return "";
    };
    base.0.rfind('/').map_or("", |index| &base.0[..index])
}

pub(crate) fn join_rel(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in rel.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn resolve_uri(base: Option<&SourceUri>, rel: &str) -> Result<SourceUri, UiDocError> {
    let origin = base.cloned().unwrap_or_else(|| SourceUri("<entry>".into()));
    if rel.starts_with('/') {
        return Err(UiDocError::RootEscape {
            origin,
            rel: rel.to_owned(),
        });
    }
    join_rel(base_dir(base), rel)
        .map(SourceUri)
        .ok_or_else(|| UiDocError::RootEscape {
            origin,
            rel: rel.to_owned(),
        })
}
