use std::collections::{HashMap, HashSet};

use kithara_ui::render::{ReadValue, TableRow};

use super::{
    consts,
    request::{Loaded, Mode, Request},
    row::Row,
};
use crate::{
    ArtistId, MediaTrack, PlaylistId, ReleaseId, SearchResults, SearchScope, Track, TrackId,
};

struct SearchSnapshot {
    query: String,
    scope: SearchScope,
    rows: Vec<Row>,
    total: Option<usize>,
    cursor: Option<String>,
    suggestions: Vec<String>,
}

/// The current node's confirmed row snapshot and local query projection.
pub(super) struct Catalogue {
    query: String,
    count: String,
    total: Option<usize>,
    mode: Mode,
    scope: SearchScope,
    cursor: Option<String>,
    suggestions: Vec<String>,
    search: Option<SearchSnapshot>,
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
            scope: SearchScope::Tracks,
            cursor: None,
            suggestions: Vec::new(),
            search: None,
            rows: Vec::new(),
            visible: Vec::new(),
        };
        catalogue.filter();
        catalogue
    }
}

impl Catalogue {
    pub(super) fn request(&self) -> Option<Request> {
        (self.mode != Mode::Search || !self.query.trim().is_empty()).then(|| Request {
            mode: self.mode.clone(),
            query: self.query.trim().to_owned(),
            scope: self.scope,
            cursor: None,
        })
    }

    pub(super) fn continuation(&self) -> Option<Request> {
        if self.mode != Mode::Search {
            return None;
        }
        let cursor = self.cursor.as_ref()?.clone();
        (!cursor.is_empty()).then(|| Request {
            mode: Mode::Search,
            query: self.query.trim().to_owned(),
            scope: self.scope,
            cursor: Some(cursor),
        })
    }

    pub(super) fn autocomplete_query(&self) -> Option<String> {
        (self.mode == Mode::Search && !self.query.trim().is_empty())
            .then(|| self.query.trim().to_owned())
    }

    fn filter(&mut self) {
        let query = if matches!(self.mode, Mode::Liked | Mode::Playlist(_)) {
            self.query.trim().to_lowercase()
        } else {
            String::new()
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
            self.clear();
        }
        self.filter();
        remote
    }

    pub(super) fn set_scope(&mut self, scope: SearchScope) -> bool {
        if self.mode != Mode::Search || self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.clear();
        true
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

    /// Drops the loaded rows, continuation and transient search state.
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.total = None;
        self.cursor = None;
        self.suggestions.clear();
        if let Some(search) = self.search.take()
            && matches!(self.mode, Mode::Artist(_) | Mode::Album(_))
        {
            self.mode = Mode::Search;
            self.query = search.query;
            self.scope = search.scope;
        }
        self.filter();
    }

    pub(super) fn accept(&mut self, loaded: Loaded, adaptive: bool) -> Vec<TrackId> {
        let previous = self.cursor.clone();
        let ids = match loaded {
            Loaded::Tracks(page) => {
                self.total = page.total;
                self.cursor = None;
                self.replace_tracks(page.tracks, adaptive)
            }
            Loaded::Search(SearchResults::Tracks(page), append) => {
                self.total = page.total;
                let ids = if append {
                    self.append_tracks(page.items, adaptive)
                } else {
                    self.replace_tracks(page.items, adaptive)
                };
                let appended = ids.len();
                self.cursor = next_cursor(previous.as_deref(), page.cursor, append, appended);
                ids
            }
            Loaded::Search(SearchResults::Artists(page), append) => {
                self.total = page.total;
                let appended = if append {
                    self.append_entities(page.items.into_iter().map(Row::artist))
                } else {
                    self.rows = page.items.into_iter().map(Row::artist).collect();
                    0
                };
                self.cursor = next_cursor(previous.as_deref(), page.cursor, append, appended);
                Vec::new()
            }
            Loaded::Search(SearchResults::Albums(page), append) => {
                self.total = page.total;
                let appended = if append {
                    self.append_entities(page.items.into_iter().map(Row::release))
                } else {
                    self.rows = page.items.into_iter().map(Row::release).collect();
                    0
                };
                self.cursor = next_cursor(previous.as_deref(), page.cursor, append, appended);
                Vec::new()
            }
        };
        self.filter();
        ids
    }

    fn replace_tracks(&mut self, tracks: Vec<Track>, adaptive: bool) -> Vec<TrackId> {
        let ids = unique_track_ids(&tracks);
        self.rows = tracks
            .into_iter()
            .map(|track| Row::new(track, None, adaptive))
            .collect();
        ids
    }

    fn append_tracks(&mut self, tracks: Vec<Track>, adaptive: bool) -> Vec<TrackId> {
        let mut known: HashSet<TrackId> = self
            .rows
            .iter()
            .filter_map(|row| row.track_id().cloned())
            .collect();
        let mut added: Vec<TrackId> = Vec::new();
        for track in tracks {
            if known.insert(track.id.clone()) {
                added.push(track.id.clone());
                self.rows.push(Row::new(track, None, adaptive));
            }
        }
        added
    }

    fn append_entities(&mut self, rows: impl Iterator<Item = Row>) -> usize {
        let mut known: HashSet<String> = self.rows.iter().map(|row| row.key().to_owned()).collect();
        let before = self.rows.len();
        for row in rows {
            if known.insert(row.key().to_owned()) {
                self.rows.push(row);
            }
        }
        self.rows.len() - before
    }

    pub(super) fn enrich(&mut self, streams: Vec<MediaTrack>, adaptive: bool) {
        let streams: HashMap<_, _> = streams
            .into_iter()
            .map(|stream| (stream.id.clone(), stream))
            .collect();
        for row in &mut self.rows {
            if let Some(stream) = row.track_id().and_then(|id| streams.get(id)) {
                row.enrich(stream, adaptive);
            }
        }
    }

    pub(super) fn accept_suggestions(&mut self, suggestions: Vec<String>) {
        self.suggestions = suggestions.into_iter().take(3).collect();
    }

    pub(super) fn suggestion(&self, slot: usize) -> Option<&str> {
        self.suggestions.get(slot).map(String::as_str)
    }

    pub(super) fn open(&mut self, action: &str) -> bool {
        if self.mode != Mode::Search
            || !self
                .visible
                .iter()
                .any(|index| self.rows[*index].entity_action() == Some(action))
        {
            return false;
        }
        let mode = if let Some(id) = action.strip_prefix("artist:") {
            Mode::Artist(ArtistId(id.to_owned()))
        } else if let Some(id) = action.strip_prefix("album:") {
            Mode::Album(ReleaseId(id.to_owned()))
        } else {
            return false;
        };
        self.search = Some(SearchSnapshot {
            query: self.query.clone(),
            scope: self.scope,
            rows: std::mem::take(&mut self.rows),
            total: self.total.take(),
            cursor: self.cursor.take(),
            suggestions: std::mem::take(&mut self.suggestions),
        });
        self.mode = mode;
        self.filter();
        true
    }

    pub(super) fn back(&mut self) -> bool {
        if !matches!(self.mode, Mode::Artist(_) | Mode::Album(_)) {
            return false;
        }
        let Some(search) = self.search.take() else {
            return false;
        };
        self.mode = Mode::Search;
        self.query = search.query;
        self.scope = search.scope;
        self.rows = search.rows;
        self.total = search.total;
        self.cursor = search.cursor;
        self.suggestions = search.suggestions;
        self.filter();
        true
    }

    pub(super) fn reaction(&self, id: &str) -> Option<(TrackId, bool)> {
        self.visible
            .iter()
            .map(|index| &self.rows[*index])
            .find_map(|row| {
                let reaction = row.reaction()?;
                (reaction.0.0 == id).then_some(reaction)
            })
    }

    pub(super) fn set_adaptive(&mut self, adaptive: bool) {
        for row in &mut self.rows {
            row.set_adaptive(adaptive);
        }
    }

    pub(super) fn confirm_reaction(&mut self, id: &TrackId, liked: bool) -> bool {
        for row in &mut self.rows {
            row.confirm_reaction(id, liked);
        }
        self.mode == Mode::Liked
    }

    pub(super) fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        Some(match endpoint {
            "query" => ReadValue::Text(&self.query),
            "count" => ReadValue::Text(&self.count),
            "scope_tracks" => ReadValue::Bool(self.scope == SearchScope::Tracks),
            "scope_artists" => ReadValue::Bool(self.scope == SearchScope::Artists),
            "scope_albums" => ReadValue::Bool(self.scope == SearchScope::Albums),
            "search_controls_hidden" => ReadValue::Bool(self.mode != Mode::Search),
            "back_hidden" => {
                ReadValue::Bool(!matches!(self.mode, Mode::Artist(_) | Mode::Album(_)))
            }
            "load_more_hidden" => ReadValue::Bool(
                self.mode != Mode::Search || self.cursor.as_deref().is_none_or(str::is_empty),
            ),
            "suggestions_hidden" => ReadValue::Bool(self.suggestions.is_empty()),
            "suggestion_0" => ReadValue::Text(self.suggestion(0).unwrap_or("")),
            "suggestion_1" => ReadValue::Text(self.suggestion(1).unwrap_or("")),
            "suggestion_2" => ReadValue::Text(self.suggestion(2).unwrap_or("")),
            "suggestion_0_hidden" => ReadValue::Bool(self.suggestion(0).is_none()),
            "suggestion_1_hidden" => ReadValue::Bool(self.suggestion(1).is_none()),
            "suggestion_2_hidden" => ReadValue::Bool(self.suggestion(2).is_none()),
            _ => return None,
        })
    }

    pub(super) fn analysis_key(&self, row: usize) -> Option<&str> {
        self.rows.get(*self.visible.get(row)?)?.source()
    }

    pub(super) fn row_key(&self, row: usize) -> Option<&str> {
        Some(self.rows.get(*self.visible.get(row)?)?.key())
    }

    pub(super) fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>> {
        self.visible
            .iter()
            .map(|index| {
                let row = &self.rows[*index];
                row.view(selected == Some(row.key()))
            })
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }
}

fn unique_track_ids(tracks: &[Track]) -> Vec<TrackId> {
    let mut listed = HashSet::new();
    tracks
        .iter()
        .filter(|track| listed.insert(&track.id))
        .map(|track| track.id.clone())
        .collect()
}

fn next_cursor(
    previous: Option<&str>,
    next: Option<String>,
    append: bool,
    appended: usize,
) -> Option<String> {
    let next = next.filter(|cursor| !cursor.is_empty());
    if append && (appended == 0 || next.as_deref() == previous) {
        None
    } else {
        next
    }
}
