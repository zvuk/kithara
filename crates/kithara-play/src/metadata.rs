use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub duration: Option<Duration>,
    pub artwork: Option<Artwork>,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Artwork {
    pub data: Vec<u8>,
    pub mime_type: String,
}

impl Artwork {
    #[must_use]
    pub fn new(data: Vec<u8>, mime_type: String) -> Self {
        Self { data, mime_type }
    }
}
