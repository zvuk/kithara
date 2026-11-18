use crate::{SearchArtist, SearchRelease, Track};

/// One catalogue page and the exact result total established by the service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackPage {
    /// Tracks loaded in this page, in the service's order.
    pub(crate) tracks: Vec<Track>,
    /// Server count or terminal page length; unknown when neither is supplied.
    pub(crate) total: Option<usize>,
}

/// One typed search page in the service's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchPage<T> {
    /// Results supplied by the selected search scope.
    pub(crate) items: Vec<T>,
    /// Full result count established by the service.
    pub(crate) total: Option<usize>,
    /// Opaque continuation supplied by the service.
    pub(crate) cursor: Option<String>,
}

/// Search result page for the requested scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SearchResults {
    /// Matching tracks.
    Tracks(SearchPage<Track>),
    /// Matching artists.
    Artists(SearchPage<SearchArtist>),
    /// Matching releases.
    Albums(SearchPage<SearchRelease>),
}

/// Music catalogue scope queried by search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchScope {
    /// Track results.
    Tracks,
    /// Artist results.
    Artists,
    /// Release results.
    Albums,
}
