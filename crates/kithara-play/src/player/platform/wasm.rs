use kithara_platform::sync::Arc;

use super::{
    super::{
        core::PlayerImpl,
        state::{
            items::{BoundLoad, restore_queued_resource},
            playlist::{Playlist, PreparedBindingStamp},
        },
    },
    ItemLoadContext,
};
use crate::{
    api::{SessionTransportSnapshot, Tempo, TrackBinding},
    error::PlayError,
    player::{node::StreamShape, track::ReleasedPlayerResource},
    resource::Resource,
};

pub(crate) enum PreparedBindingResource {}

impl PlayerImpl {
    /// Rejects session-bound elastic insertion because the browser backend
    /// does not provide the required renderer.
    pub async fn insert_with_binding(
        &self,
        _resource: Resource,
        _item_id: Option<Arc<str>>,
        _binding: TrackBinding,
        _at_position: Option<usize>,
    ) -> Result<(), PlayError> {
        Err(PlayError::ElasticBackendUnavailable)
    }

    pub(in crate::player) fn validate_session_tempo(
        &self,
        _snapshot: SessionTransportSnapshot,
        _tempo: Tempo,
        _shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        if binding.is_some() {
            Err(PlayError::ElasticBackendUnavailable)
        } else {
            Ok(())
        }
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
            Err(PlayError::ElasticBackendUnavailable)
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
    let ItemLoadContext {
        tempo: _binding_tempo,
        ..
    } = context;
    drop(prepared);
    restore_queued_resource(playlist, index, None, resource)?;
    Err(PlayError::Internal(
        "browser queue unexpectedly contained a bound resource".into(),
    ))
}

pub(crate) fn restore_prepared_binding(
    released: ReleasedPlayerResource,
    stamp: Option<PreparedBindingStamp>,
) -> Result<(Resource, Option<PreparedBindingResource>), PlayError> {
    match (released, stamp) {
        (ReleasedPlayerResource::Linear(resource), None) => Ok((resource, None)),
        _ => Err(PlayError::Internal(
            "browser load returned inconsistent binding state".into(),
        )),
    }
}
