use kithara_platform::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub artwork: Option<Artwork>,
    pub disc_number: Option<u32>,
    pub duration: Option<Duration>,
    pub genre: Option<String>,
    pub title: Option<String>,
    pub track_number: Option<u32>,
    pub year: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Artwork {
    pub mime_type: String,
    pub data: Vec<u8>,
}

impl Artwork {
    #[must_use]
    pub fn new(data: Vec<u8>, mime_type: String) -> Self {
        Self { mime_type, data }
    }
}
