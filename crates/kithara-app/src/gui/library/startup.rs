use kithara::ui::{error::UiDocError, module::IconName, render::TableRow, text::TextDoc};
use kithara_app_library::{BranchNode, LibrarySource, PageStatus, Registration, worded};

use super::track::{Track, display_name};

/// The tracks the app started with, listed under Collection.
pub(in crate::gui) struct StartupSource {
    branch: BranchNode,
    tracks: Vec<Track>,
    /// Whether the Startup node is the selected one.
    listing: bool,
}

impl StartupSource {
    const COLLECTION: &str = "collection";
    const ID: &'static str = "startup";

    pub(in crate::gui) fn registered(urls: Vec<String>) -> Registration {
        super::listed(Self::ID, move |text| Ok(Box::new(Self::new(urls, text)?)))
    }

    pub(in crate::gui) fn new(urls: Vec<String>, text: &TextDoc) -> Result<Self, UiDocError> {
        let mut tracks: Vec<Track> = Vec::with_capacity(urls.len());
        for url in urls {
            if tracks.iter().all(|track| track.url() != url) {
                tracks.push(Track::new(display_name(&url), url));
            }
        }
        let label = worded(text, "library.source.startup", Self::ID)?;
        let mut startup = BranchNode::new(Self::ID, label, IconName::Playlist);
        startup.count = u32::try_from(tracks.len()).ok();
        startup.page = true;
        let label = worded(text, "library.source.collection", Self::ID)?;
        let mut branch = BranchNode::new(Self::COLLECTION, label, IconName::Disc);
        branch.children = vec![startup];
        Ok(Self {
            branch,
            tracks,
            listing: false,
        })
    }
}

impl LibrarySource for StartupSource {
    fn analysis_key(&self, row: usize) -> Option<&str> {
        self.listing
            .then(|| self.tracks.get(row))
            .flatten()
            .map(Track::key)
    }

    fn branch(&self) -> &BranchNode {
        &self.branch
    }

    fn expand(&mut self, _node: &str) {}

    fn id(&self) -> &str {
        Self::ID
    }

    fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>> {
        if !self.listing {
            return Vec::new();
        }
        self.tracks
            .iter()
            .map(|track| track.row(selected == Some(track.url())))
            .collect()
    }

    fn row_key(&self, row: usize) -> Option<&str> {
        self.listing
            .then(|| self.tracks.get(row))
            .flatten()
            .map(Track::url)
    }

    fn select(&mut self, node: &str) {
        self.listing = node == Self::ID;
    }

    fn status(&self) -> PageStatus {
        if !self.listing || self.tracks.is_empty() {
            PageStatus::Empty
        } else {
            PageStatus::Ready
        }
    }

    fn tick(&mut self) {}
}
