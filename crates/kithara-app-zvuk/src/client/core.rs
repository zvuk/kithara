use std::collections::HashSet;

use bytes::Bytes;
use kithara_app_library::AccessToken;
use kithara_config::Config as _;
use kithara_net::{Headers, HttpClient, Net, RetryPolicy};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use crate::{
    Config, Error, GraphQlError, MediaTrack, Playlist, PlaylistId, Track, TrackId, TrackPage,
    consts,
};

/// Catalogue operations using the caller's transport and the identity its
/// configuration names.
///
/// Reads retain the configured retry policy; mutations use a single attempt.
#[derive(Clone)]
pub struct Client<N> {
    pub(crate) net: N,
    pub(crate) mutations: N,
    pub(super) endpoint: Url,
    headers: Headers,
}

impl Client<HttpClient> {
    /// Uses one shared HTTP transport with operation-specific retry policies.
    #[must_use]
    pub(crate) fn new(net: HttpClient, config: &Config) -> Self {
        let current = net.options().values().retry_policy;
        let policy = RetryPolicy::builder()
            .base_delay(current.base_delay)
            .max_delay(current.max_delay)
            .max_retries(0)
            .build();
        let mutations = net.with_retry_policy(policy);
        Self::with_transports(net, mutations, config)
    }
}

impl<N: Net> Client<N> {
    /// Supplies read and mutation transports; mutations must use one attempt.
    ///
    /// # Panics
    ///
    /// Panics if the constant service endpoint fails to parse as a URL.
    pub fn with_transports(net: N, mutations: N, config: &Config) -> Self {
        let mut headers = Headers::default();
        headers.insert("User-Agent", config.user_agent.as_str());
        Self {
            net,
            mutations,
            endpoint: Url::parse(consts::ENDPOINT).expect("BUG: the endpoint is a valid URL"),
            headers,
        }
    }

    /// Loads the first search page and the service's total for this query.
    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn search(
        &self,
        token: &AccessToken,
        query: &str,
    ) -> Result<TrackPage, Error> {
        let track_fields = consts::TRACK_FIELDS;
        let operation = format!(
            "query KitharaSearch($query: String!) {{ search(query: $query) {{ tracks(limit: 100) {{ items {{ {track_fields} }} page {{ total }} }} }} }}"
        );
        let data = self
            .execute(&self.net, token, &operation, json!({ "query": query }))
            .await?;
        Ok(TrackPage {
            tracks: decode(&data, "/search/tracks/items")?,
            total: decode_count(&data, "/search/tracks/page/total")?,
        })
    }

    /// Loads the first liked page and the configured account's collection count.
    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn liked_tracks(&self, token: &AccessToken) -> Result<TrackPage, Error> {
        let track_fields = consts::TRACK_FIELDS;
        let operation = format!(
            "query KitharaLiked {{ collectionCount {{ tracks }} paginatedCollection {{ tracks(pagination: {{ first: 100 }}) {{ items {{ {track_fields} }} page {{ hasNextPage }} }} }} }}"
        );
        let data = self
            .execute(&self.net, token, &operation, json!({}))
            .await?;
        let tracks: Vec<Track> = decode(&data, "/paginatedCollection/tracks/items")?;
        let total = decode_count(&data, "/collectionCount/tracks")?.or_else(|| {
            (data.pointer("/paginatedCollection/tracks/page/hasNextPage")
                == Some(&Value::Bool(false)))
            .then_some(tracks.len())
        });
        Ok(TrackPage { tracks, total })
    }

    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn playlists(&self, token: &AccessToken) -> Result<Vec<Playlist>, Error> {
        let data = self
            .execute(
                &self.net,
                token,
                "query KitharaPlaylists { collection { playlists { id title } } }",
                json!({}),
            )
            .await?;
        decode(&data, "/collection/playlists")
    }

    /// Loads the first playlist page and its track count, checking the returned identity.
    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn playlist_tracks(
        &self,
        token: &AccessToken,
        id: &PlaylistId,
    ) -> Result<TrackPage, Error> {
        let ids = encode(&[id])?;
        let track_fields = consts::TRACK_FIELDS;
        let operation = format!(
            "query KitharaPlaylist {{ playlists(ids: {ids}) {{ id trackCount tracks(limit: 100, offset: 0) {{ {track_fields} }} }} }}"
        );
        let data = self
            .execute(&self.net, token, &operation, json!({}))
            .await?;
        let playlists = data
            .get("playlists")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol("missing playlist result"))?;
        if playlists.len() != 1
            || playlists[0].get("id").and_then(Value::as_str) != Some(id.0.as_str())
        {
            return Err(Error::Protocol("playlist identity does not match request"));
        }
        Ok(TrackPage {
            tracks: decode(&playlists[0], "/tracks")?,
            total: decode_count(&playlists[0], "/trackCount")?,
        })
    }

    /// Resolve one batch with one request; an empty batch performs no I/O.
    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn streams(
        &self,
        token: &AccessToken,
        ids: &[TrackId],
    ) -> Result<Vec<MediaTrack>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let encoded_ids = encode(ids)?;
        let media_fields = consts::MEDIA_FIELDS;
        let operation = format!(
            "query KitharaStreams {{ mediaContents(ids: {encoded_ids}, quality: \"auto\", encodeType: \"fp\") {{ {media_fields} }} }}"
        );
        let envelope = self
            .request(&self.net, token, &operation, json!({}))
            .await?;
        let errors = decode_errors(&envelope)?;
        if errors
            .iter()
            .any(|error| !unavailable_stream(error, &envelope))
        {
            return Err(Error::GraphQl(errors));
        }
        let rows: Vec<MediaTrack> = decode(&envelope, "/data/mediaContents")?;
        let returned: HashSet<&TrackId> = rows.iter().map(|row| &row.id).collect();
        let requested: HashSet<&TrackId> = ids.iter().collect();
        if returned != requested || returned.len() != rows.len() {
            return Err(Error::Protocol("stream identities do not match request"));
        }
        if !rows.iter().all(|row| {
            let mut urls = HashSet::new();
            row.urls()
                .all(|url| matches!(url.scheme(), "http" | "https") && urls.insert(url))
        }) {
            return Err(Error::Protocol("stream URLs are invalid or repeated"));
        }
        Ok(rows)
    }

    /// Return success only after the server confirms the requested mutation.
    /// # Errors
    ///
    /// Returns authentication, transport, `GraphQL` or protocol errors.
    pub(crate) async fn set_liked(
        &self,
        token: &AccessToken,
        id: &TrackId,
        liked: bool,
    ) -> Result<(), Error> {
        let field = if liked { "addItem" } else { "removeItem" };
        let encoded_id = encode(id)?;
        let operation = format!(
            "mutation KitharaReaction {{ collection {{ {field}(id: {encoded_id}, type: track) }} }}"
        );
        let data = self
            .execute(&self.mutations, token, &operation, json!({}))
            .await?;
        let confirmation = data
            .get("collection")
            .and_then(|collection| collection.get(field))
            .ok_or(Error::Protocol("missing mutation confirmation"))?;
        if !confirmation.is_null() {
            return Err(Error::Protocol("unexpected mutation confirmation"));
        }
        Ok(())
    }

    /// The client identity, carrying `token` when one is given.
    pub(crate) fn headers(&self, token: Option<&AccessToken>) -> Headers {
        let mut headers = self.headers.clone();
        if let Some(token) = token {
            headers.insert(consts::AUTH_HEADER, token.expose());
        }
        headers
    }

    async fn execute(
        &self,
        net: &N,
        token: &AccessToken,
        query: &str,
        variables: Value,
    ) -> Result<Value, Error> {
        let envelope = self.request(net, token, query, variables).await?;
        let errors = decode_errors(&envelope)?;
        if !errors.is_empty() {
            return Err(Error::GraphQl(errors));
        }
        envelope
            .get("data")
            .filter(|data| data.is_object())
            .cloned()
            .ok_or(Error::Protocol("missing data object"))
    }

    async fn request(
        &self,
        net: &N,
        token: &AccessToken,
        query: &str,
        variables: Value,
    ) -> Result<Value, Error> {
        let body = serde_json::to_vec(&json!({ "query": query, "variables": variables }))
            .map_err(|_| Error::Protocol("request encoding failed"))?;
        let mut headers = self.headers(Some(token));
        headers.insert("Content-Type", "application/json");
        let response = net
            .post_bytes(self.endpoint.clone(), Bytes::from(body), Some(headers))
            .await?;
        serde_json::from_slice(&response).map_err(|_| Error::Protocol("invalid JSON envelope"))
    }
}

fn decode_errors(envelope: &Value) -> Result<Vec<GraphQlError>, Error> {
    envelope.get("errors").map_or_else(
        || Ok(Vec::new()),
        |errors| {
            serde_json::from_value(errors.clone())
                .map_err(|_| Error::Protocol("invalid GraphQL errors"))
        },
    )
}

fn unavailable_stream(error: &GraphQlError, envelope: &Value) -> bool {
    let [field, index, property, rest @ ..] = error.path.as_slice() else {
        return false;
    };
    let Some(property) = property.as_str() else {
        return false;
    };
    let known = match (property, rest) {
        ("streamV3" | "stream", []) => true,
        ("streamV3", [leaf]) => leaf == "hls",
        ("stream", [leaf]) => matches!(leaf.as_str(), Some("mid" | "high" | "flac")),
        _ => false,
    };
    if field != "mediaContents" || !known {
        return false;
    }
    index
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| envelope.pointer("/data/mediaContents")?.get(index))
        .and_then(|row| row.get(property))
        .and_then(|stream| match rest {
            [] => Some(stream),
            [leaf] => stream.get(leaf.as_str()?),
            _ => None,
        })
        .is_some_and(Value::is_null)
}

fn encode<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::Protocol("request encoding failed"))
}

fn decode<T: DeserializeOwned>(data: &Value, pointer: &str) -> Result<T, Error> {
    let value = data
        .pointer(pointer)
        .ok_or(Error::Protocol("missing operation result"))?;
    serde_json::from_value(value.clone()).map_err(|_| Error::Protocol("invalid operation result"))
}

fn decode_count(data: &Value, pointer: &str) -> Result<Option<usize>, Error> {
    data.pointer(pointer)
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| Error::Protocol("invalid catalogue total"))
        })
        .transpose()
}
