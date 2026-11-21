use kithara_app_library::AccessToken;
use kithara_net::Net;

use crate::{
    ArtistId, Client, Error, PlaylistId, ReleaseId, SearchResults, SearchScope, TrackPage,
};

#[derive(Clone, PartialEq)]
pub(super) enum Mode {
    Search,
    Liked,
    Playlist(PlaylistId),
    Artist(ArtistId),
    Album(ReleaseId),
}

/// One immutable navigation request; replacing its job cancels every later phase.
#[derive(Clone)]
pub(super) struct Request {
    pub(super) mode: Mode,
    pub(super) query: String,
    pub(super) scope: SearchScope,
    pub(super) cursor: Option<String>,
}

pub(super) enum Loaded {
    Tracks(TrackPage),
    Search(SearchResults, bool),
}

impl Request {
    pub(super) async fn load<N: Net>(
        self,
        client: &Client<N>,
        token: &AccessToken,
    ) -> Result<Loaded, Error> {
        match self.mode {
            Mode::Search => {
                let append = self.cursor.is_some();
                client
                    .search(token, &self.query, self.scope, self.cursor.as_deref())
                    .await
                    .map(|page| Loaded::Search(page, append))
            }
            Mode::Liked => client.liked_tracks(token).await.map(Loaded::Tracks),
            Mode::Playlist(id) => client.playlist_tracks(token, &id).await.map(Loaded::Tracks),
            Mode::Artist(id) => client.artist_tracks(token, &id).await.map(Loaded::Tracks),
            Mode::Album(id) => client.release_tracks(token, &id).await.map(Loaded::Tracks),
        }
    }
}
