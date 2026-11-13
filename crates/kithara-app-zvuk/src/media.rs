use serde::{Deserialize, Deserializer};
use url::Url;

use crate::TrackId;

/// A track identity paired with its stream-resolution result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaTrack {
    /// Stable identity supplied by the catalogue.
    pub(crate) id: TrackId,
    #[serde(deserialize_with = "Option::deserialize")]
    /// Resolved stream, or no available stream.
    pub(crate) stream_v3: Option<HlsStream>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub(crate) stream: Option<ProgressiveStream>,
}

impl MediaTrack {
    pub(crate) fn urls(&self) -> impl Iterator<Item = &Url> {
        let progressive = self.stream.as_ref();
        [
            self.stream_v3
                .as_ref()
                .and_then(|stream| stream.hls.as_ref()),
            progressive.and_then(|stream| stream.high.as_ref()),
            progressive.and_then(|stream| stream.mid.as_ref()),
            progressive.and_then(|stream| stream.flac.as_ref()),
        ]
        .into_iter()
        .flatten()
    }
}

/// The progressive qualities made available by the catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ProgressiveStream {
    #[serde(deserialize_with = "optional_url")]
    pub(crate) high: Option<Url>,
    #[serde(deserialize_with = "optional_url")]
    pub(crate) mid: Option<Url>,
    #[serde(deserialize_with = "optional_url")]
    pub(crate) flac: Option<Url>,
}

/// The optional HLS URL and server-supplied expiration value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct HlsStream {
    #[serde(deserialize_with = "optional_url")]
    /// A nonempty HTTP HLS URL, or no playable URL.
    pub(crate) hls: Option<Url>,
    #[serde(deserialize_with = "Option::deserialize")]
    /// Expiration text supplied by the server, without a non-expiry guarantee.
    pub(crate) expire: Option<String>,
}

fn optional_url<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Url>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => Url::parse(&value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
