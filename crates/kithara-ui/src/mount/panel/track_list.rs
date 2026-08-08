use bon::Builder;

use crate::{expand::Binding, module::TrackColumn, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A table of tracks, with columns the document declares.
#[derive(Builder)]
pub(crate) struct TrackList<'a> {
    pub(crate) columns: &'a [TrackColumn],
    pub(crate) columns_state: Option<&'a Binding>,
}

impl Control for TrackList<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.track_list.size
    }
}
