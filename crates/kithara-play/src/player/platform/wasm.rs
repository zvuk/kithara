use std::ops::Range;

use kithara_platform::sync::Arc;

use super::{
    super::{
        core::PlayerImpl,
        state::{
            items::{BoundLoad, restore_queued_resource},
            playlist::Playlist,
        },
    },
    ItemLoadContext,
};
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo, TrackBinding, TransportRevision},
    error::PlayError,
    player::{
        node::StreamShape,
        track::{PlayerResource, ReadOutcome, ReleasedPlayerResource},
    },
    resource::Resource,
    session::{protocol::PreparationContext, render::RenderContext},
};

pub(crate) enum ActiveBoundReader {}

pub(crate) enum PreparedBindingResource {}

fn elastic_unavailable<T>() -> Result<T, PlayError> {
    Err(PlayError::ElasticBackendUnavailable)
}

impl ActiveBoundReader {
    pub(in crate::player) fn decoded_frontier(&self) -> f64 {
        match *self {}
    }
}

impl PlayerResource {
    pub(crate) fn begin_session_seek(
        &mut self,
        _binding: &TrackBinding,
        _target: SessionBeat,
        _tempo: Tempo,
        _revision: TransportRevision,
    ) -> Result<(), PlayError> {
        elastic_unavailable()
    }

    pub(crate) fn cancel_session_seek(&mut self, _revision: TransportRevision) {}

    pub(crate) fn poll_session_seek(
        &mut self,
        _revision: TransportRevision,
    ) -> Result<bool, PlayError> {
        elastic_unavailable()
    }

    pub(crate) fn read_elastic(
        &mut self,
        _binding: &TrackBinding,
        _context: &RenderContext,
        _range: Range<usize>,
        _output: &mut [&mut [f32]],
    ) -> ReadOutcome {
        ReadOutcome::Failed
    }
}

impl PlayerImpl {
    /// Rejects session-bound insertion when the browser has no elastic reader.
    pub async fn insert_with_binding(
        &self,
        _resource: Resource,
        _item_id: Option<Arc<str>>,
        _binding: TrackBinding,
        _at_position: Option<usize>,
    ) -> Result<(), PlayError> {
        elastic_unavailable()
    }

    /// Browser elastic playback does not provide exact session joining.
    pub(in crate::player) async fn join_track_at(
        &self,
        _resource: Resource,
        _item_id: Option<Arc<str>>,
        _binding: TrackBinding,
        _target: SessionBeat,
    ) -> Result<(), PlayError> {
        elastic_unavailable()
    }

    pub(in crate::player) fn validate_session_seek(
        &self,
        _target: SessionBeat,
        _tempo: Tempo,
        _revision: TransportRevision,
        _shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        binding.map_or_else(
            || Err(PlayError::SessionSeekRequiresBoundTrack),
            |_| elastic_unavailable(),
        )
    }

    pub(in crate::player) fn validate_session_tempo(
        &self,
        _snapshot: SessionTransportSnapshot,
        _tempo: Tempo,
        _shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        binding.map_or(Ok(()), |_| elastic_unavailable())
    }

    pub(in crate::player) fn validate_successor_tempo(
        playlist: &Playlist,
        _tempo: Tempo,
        _shape: StreamShape,
    ) -> Result<(), PlayError> {
        if (playlist.current().saturating_add(1)..playlist.len()).any(|index| {
            playlist
                .get(index)
                .is_some_and(|queued| queued.binding.is_some())
        }) {
            elastic_unavailable()
        } else {
            Ok(())
        }
    }
}

pub(crate) fn prepare_bound_load(
    playlist: &mut Playlist,
    index: usize,
    resource: Resource,
    _binding: &TrackBinding,
    prepared: Option<PreparedBindingResource>,
    context: ItemLoadContext<'_>,
) -> Result<BoundLoad, PlayError> {
    drop(prepared);
    restore_queued_resource(playlist, index, None, resource)?;
    if context.preparation().is_none() {
        return Err(PlayError::BindingPreparationContextChanged);
    }
    Err(PlayError::Internal(
        "browser queue unexpectedly contained a bound resource".into(),
    ))
}

pub(crate) fn restore_prepared_binding(
    released: ReleasedPlayerResource,
    context: Option<PreparationContext>,
) -> Result<(Resource, Option<PreparedBindingResource>), PlayError> {
    match (released, context) {
        (ReleasedPlayerResource::Linear(resource), None) => Ok((resource, None)),
        (ReleasedPlayerResource::Linear(_), Some(_)) => Err(PlayError::Internal(
            "browser load returned inconsistent binding state".into(),
        )),
        (ReleasedPlayerResource::Bound(bound), _) => {
            let bound = *bound;
            match bound.reader {}
        }
    }
}
