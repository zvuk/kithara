use std::collections::BTreeMap;

use crate::{error::UiDocError, ids::SourceUri};

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LoadedSource {
    pub uri: SourceUri,
    pub text: String,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Limits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_depth: 8,
            max_nodes: 10_000,
        }
    }
}

pub trait SourceResolver {
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

fn resolve_uri(base: Option<&SourceUri>, rel: &str) -> Result<SourceUri, UiDocError> {
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

#[derive(Debug, Default)]
pub struct MemResolver {
    files: BTreeMap<String, String>,
}

impl MemResolver {
    pub fn insert(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_owned(), text.to_owned());
    }
}

impl SourceResolver for MemResolver {
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn relative_include_resolves_against_base_dir() {
        let mut resolver = MemResolver::default();
        resolver.insert("modules/deck/transport.kmodule.ron", "x");
        let base = SourceUri("modules/deck.kmodule.ron".into());
        let loaded = resolver
            .load(Some(&base), "deck/transport.kmodule.ron")
            .unwrap();
        assert_eq!(loaded.uri.0, "modules/deck/transport.kmodule.ron");
    }

    #[kithara::test]
    fn parent_escape_is_rejected() {
        let resolver = MemResolver::default();
        let base = SourceUri("modules/deck.kmodule.ron".into());
        let error = resolver.load(Some(&base), "../../etc/passwd").unwrap_err();
        assert!(matches!(error, UiDocError::RootEscape { .. }));
    }

    #[kithara::test]
    fn absolute_path_is_rejected() {
        let resolver = MemResolver::default();
        let error = resolver.load(None, "/etc/passwd").unwrap_err();
        assert!(matches!(error, UiDocError::RootEscape { .. }));
    }

    #[kithara::test]
    fn missing_source_is_not_found() {
        let resolver = MemResolver::default();
        let error = resolver.load(None, "nope.ron").unwrap_err();
        assert!(matches!(error, UiDocError::NotFound { .. }));
    }
}
