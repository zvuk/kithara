use serde::Deserialize;

/// Stable identity of a catalogue track.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct TrackId(pub(crate) String);

/// Stable identity of a collection playlist.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct PlaylistId(pub(crate) String);

/// Stable identity of a catalogue artist.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct ArtistId(pub(crate) String);

/// Stable identity of a catalogue release.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct ReleaseId(pub(crate) String);

/// Artist metadata returned with a track.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Artist {
    /// Display title supplied by the catalogue.
    pub(crate) title: String,
}

/// One track in the first catalogue page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Track {
    /// Stable identity supplied by the catalogue.
    pub(crate) id: TrackId,
    /// Display title supplied by the catalogue.
    pub(crate) title: String,
    /// Track duration in seconds.
    pub(crate) duration: u64,
    /// Artist metadata supplied for the track.
    pub(crate) artists: Vec<Artist>,
    release: Option<Release>,
    /// The account reaction returned with this track.
    pub(crate) collection_item_data: Option<CollectionItemData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct Release {
    title: Option<String>,
    image: Option<ReleaseImage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ReleaseImage {
    src: Option<String>,
}

impl Track {
    /// Album title supplied by the track's release, when present.
    #[must_use]
    pub(crate) fn album(&self) -> Option<&str> {
        self.release.as_ref()?.title.as_deref()
    }

    /// Artwork URL template supplied by the track's release, when present.
    #[must_use]
    pub(crate) fn artwork(&self) -> Option<&str> {
        self.release.as_ref()?.image.as_ref()?.src.as_deref()
    }
}

/// The account reaction recorded for a catalogue item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CollectionItemData {
    /// The confirmed account reaction, when supplied.
    pub(crate) item_status: Option<ItemStatus>,
}

/// A confirmed account reaction returned by the service.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ItemStatus {
    /// The account has liked the track.
    Liked,
    /// The account has disliked the track.
    Disliked,
}

/// One artist search result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct SearchArtist {
    /// Stable artist identity.
    pub(crate) id: ArtistId,
    /// Display title supplied by the catalogue.
    pub(crate) title: String,
}

/// One release search result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct SearchRelease {
    /// Stable release identity.
    pub(crate) id: ReleaseId,
    /// Display title supplied by the catalogue.
    pub(crate) title: String,
    /// Release artists supplied by the catalogue.
    pub(crate) artists: Vec<Artist>,
}

/// One playlist in the configured account collection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct Playlist {
    /// Stable identity supplied by the catalogue.
    pub(crate) id: PlaylistId,
    /// Display title supplied by the catalogue.
    pub(crate) title: String,
}
