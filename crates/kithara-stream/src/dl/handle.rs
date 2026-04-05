//! Track handle returned by [`Downloader::register`](super::Downloader::register).

/// Handle to a registered protocol track.
///
/// Returned by [`Downloader::register`](super::Downloader::register).
/// Use [`into_source`](Self::into_source) to obtain the underlying source
/// for sync Read+Seek access.
pub struct TrackHandle<S> {
    source: S,
}

impl<S> TrackHandle<S> {
    pub(super) fn new(source: S) -> Self {
        Self { source }
    }

    /// Consume the handle and return the underlying source.
    pub fn into_source(self) -> S {
        self.source
    }
}
