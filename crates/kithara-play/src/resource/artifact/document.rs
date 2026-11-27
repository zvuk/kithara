use kithara_analysis::TrackAnalysis;

/// A prepared artifact read back from the bytes of its own document.
///
/// Each artifact kind carries its own format, chosen by whoever owns the
/// artifact: there is no probing and no guessing chain. A loader asks the kind
/// it wants for a document, and the kind either recognises the bytes or names
/// why it does not.
pub trait ArtifactDocument: Sized {
    /// What this artifact is called in an error a caller reads.
    const KIND: &'static str;

    /// Read the artifact out of one whole document.
    ///
    /// # Errors
    ///
    /// Returns why the bytes are not a document of this kind.
    fn decode(bytes: Vec<u8>) -> Result<Self, String>;
}

/// A track's analysis document: the snapshot an analysis server wrote, read
/// as this build reads its own analysis snapshots.
impl ArtifactDocument for TrackAnalysis {
    const KIND: &'static str = "analysis";

    fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        Self::try_from(bytes.as_slice()).map_err(|error| error.to_string())
    }
}

/// A track's encoded cover image, kept as received: the display decodes the
/// picture.
#[derive(Debug)]
pub struct Cover(Vec<u8>);

impl From<Cover> for Vec<u8> {
    fn from(cover: Cover) -> Self {
        cover.0
    }
}

impl ArtifactDocument for Cover {
    const KIND: &'static str = "artwork";

    fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        Ok(Self(bytes))
    }
}
