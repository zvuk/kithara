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
    api::{SessionTransportSnapshot, Tempo, TrackBinding},
    error::PlayError,
    player::{
        node::StreamShape,
        track::{PlayerResource, ReadOutcome, ReleasedPlayerResource},
    },
    resource::Resource,
    session::{protocol::PreparationContext, render::RenderContext},
};

pub(crate) enum ElasticRendererUnavailable {}
pub(crate) type PreparedElasticRenderer = ElasticRendererUnavailable;
pub(crate) type ActiveElasticRenderer = ElasticRendererUnavailable;

pub(crate) enum PreparedBindingResource {}

impl ElasticRendererUnavailable {
    pub(in crate::player) fn decoded_frontier(&self) -> f64 {
        match *self {}
    }
}

impl PlayerResource {
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
        _ => Err(PlayError::Internal(
            "browser load returned inconsistent binding state".into(),
        )),
    }
}
