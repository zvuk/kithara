use std::collections::HashMap;

use kithara_ui::render::{ReadValue, TableRow};

use super::{consts, request::Mode, row::Row};
use crate::{MediaTrack, PlaylistId, TrackId, TrackPage};

/// The current node's confirmed row snapshot and local query projection.
pub(super) struct Catalogue {
    query: String,
    count: String,
    total: Option<usize>,
    mode: Mode,
    rows: Vec<Row>,
    visible: Vec<usize>,
}

impl Default for Catalogue {
    fn default() -> Self {
        let mut catalogue = Self {
            query: String::new(),
            count: String::new(),
            total: None,
            mode: Mode::Search,
            rows: Vec::new(),
            visible: Vec::new(),
        };
        catalogue.filter();
        catalogue
    }
}

impl Catalogue {
    pub(super) fn request(&self) -> Option<(Mode, String)> {
        (self.mode != Mode::Search || !self.query.trim().is_empty())
            .then(|| (self.mode.clone(), self.query.trim().to_owned()))
    }

    fn filter(&mut self) {
        let query = if self.mode == Mode::Search {
            String::new()
        } else {
            self.query.trim().to_lowercase()
        };
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.matches(&query))
            .map(|(index, _)| index)
            .collect();
        self.count = format!(
            "{} / {}",
            self.visible.len(),
            self.total
                .map_or_else(|| "\u{221e}".to_owned(), |total| total.to_string())
        );
    }

    /// Takes the typed text; only a changed trimmed search is a new remote query.
    pub(super) fn search(&mut self, query: &str) -> bool {
        let remote = self.mode == Mode::Search && self.query.trim() != query.trim();
        query.clone_into(&mut self.query);
        if remote {
            self.rows.clear();
            self.total = None;
        }
        self.filter();
        remote
    }

    pub(super) fn select(&mut self, node: &str) -> Option<bool> {
        let mode = match node {
            consts::SEARCH => Mode::Search,
            consts::LIKED => Mode::Liked,
            _ => {
                let id = node.strip_prefix(consts::PLAYLIST_PREFIX)?;
                Mode::Playlist(PlaylistId(id.to_owned()))
            }
        };
        if self.mode == mode {
            return Some(false);
        }
        self.mode = mode;
        self.query.clear();
        self.clear();
        Some(true)
    }

    /// Drops the loaded rows and their total.
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.total = None;
        self.filter();
    }

    pub(super) fn accept(&mut self, page: TrackPage, streams: Vec<MediaTrack>) {
        self.total = page.total;
        let streams: HashMap<_, _> = streams
            .into_iter()
            .map(|stream| (stream.id.clone(), stream))
            .collect();
        self.rows = page
            .tracks
            .into_iter()
            .map(|track| {
                let stream = streams.get(&track.id);
                Row::new(track, stream)
            })
            .collect();
        self.filter();
    }

    pub(super) fn reaction(&self, id: &str) -> Option<(TrackId, bool)> {
        self.visible
            .iter()
            .map(|index| &self.rows[*index])
            .find(|row| row.id.0 == id)
            .map(|row| (row.id.clone(), !row.liked))
    }

    pub(super) fn confirm_reaction(&mut self, id: &TrackId, liked: bool) -> bool {
        for row in self.rows.iter_mut().filter(|row| &row.id == id) {
            row.liked = liked;
        }
        self.mode == Mode::Liked
    }

    pub(super) fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        Some(match endpoint {
            "query" => ReadValue::Text(&self.query),
            "count" => ReadValue::Text(&self.count),
            _ => return None,
        })
    }

    pub(super) fn analysis_key(&self, row: usize) -> Option<&str> {
        self.rows.get(*self.visible.get(row)?)?.source()
    }

    pub(super) fn row_key(&self, row: usize) -> Option<&str> {
        Some(self.rows.get(*self.visible.get(row)?)?.id.0.as_str())
    }

    pub(super) fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>> {
        self.visible
            .iter()
            .map(|index| {
                let row = &self.rows[*index];
                row.view(selected == Some(row.id.0.as_str()))
            })
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }
}
