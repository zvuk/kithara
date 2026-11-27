use kithara_net::Headers;

use crate::resource::ResourceSrc;

/// Where a track's prepared document is read from, and the headers its
/// download sends.
///
/// The download sends exactly these headers: the audio source's headers never
/// go with it, and nothing else derives the address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRequest {
    /// The document's own address.
    pub src: ResourceSrc,
    /// The headers sent with the document's download.
    pub headers: Option<Headers>,
}
