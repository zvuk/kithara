use kithara_queue::{Queue, QueueError, Transition};

use crate::{config::AppConfig, sources::build_source};

/// One track the app knows about, independent of any deck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub name: String,
    pub url: String,
}

impl CatalogEntry {
    fn new(url: String) -> Self {
        Self {
            name: display_name(&url),
            url,
        }
    }
}

/// The app's track list. Decks load from it; it never plays anything itself,
/// so it holds no player state.
#[derive(Debug, Default)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    #[must_use]
    pub fn new(urls: Vec<String>) -> Self {
        let mut catalog = Self::default();
        for url in urls {
            catalog.add(url);
        }
        catalog
    }

    /// Add a track, or return the position of the one already listed.
    pub fn add(&mut self, url: String) -> usize {
        if let Some(index) = self.entries.iter().position(|e| e.url == url) {
            return index;
        }
        self.entries.push(CatalogEntry::new(url));
        self.entries.len() - 1
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&CatalogEntry> {
        self.entries.get(index)
    }
}

/// Put `entry` on `queue` and make it current. A track already on this deck is
/// selected rather than appended, so loading twice is a no-op plus a select.
///
/// # Errors
/// Returns [`QueueError`] when the queue rejects the selection.
pub fn load_onto(
    queue: &Queue,
    entry: &CatalogEntry,
    config: &AppConfig,
) -> Result<(), QueueError> {
    let id = queue
        .tracks()
        .into_iter()
        .find(|track| track.url.as_deref() == Some(entry.url.as_str()))
        .map_or_else(
            || queue.append(build_source(&entry.url, config)),
            |track| track.id,
        );
    queue.select(id, Transition::None)
}

/// Whether this deck already holds the track — the library's per-deck marker.
#[must_use]
pub fn is_loaded(queue: &Queue, entry: &CatalogEntry) -> bool {
    queue
        .tracks()
        .iter()
        .any(|track| track.url.as_deref() == Some(entry.url.as_str()))
}

/// Last path segment without its extension; the whole URL when it has none.
fn display_name(url: &str) -> String {
    url.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map_or_else(
            || url.to_string(),
            |segment| {
                segment
                    .rsplit_once('.')
                    .map_or_else(|| segment.to_string(), |(stem, _)| stem.to_string())
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_the_same_url_twice_yields_one_entry() {
        let mut catalog = Catalog::new(vec!["/music/a.mp3".to_string()]);
        let first = catalog.add("/music/b.mp3".to_string());
        let again = catalog.add("/music/b.mp3".to_string());

        assert_eq!(first, again);
        assert_eq!(catalog.entries().len(), 2);
    }

    #[test]
    fn display_name_strips_directory_and_extension() {
        assert_eq!(display_name("https://host/path/Song 1.flac"), "Song 1");
        assert_eq!(display_name("/music/track.mp3"), "track");
        assert_eq!(display_name("noslash"), "noslash");
    }
}
