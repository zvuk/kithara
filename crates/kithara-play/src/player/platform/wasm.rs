use kithara_bufpool::PcmPool;
use kithara_platform::{CancelToken, sync::Arc};

use super::super::{
    core::PlayerImpl,
    state::{
        items::{BoundLoad, ItemLoadContext, restore_queued_resource},
        playlist::{Playlist, PreparedBindingStamp},
    },
};
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo, TrackBinding},
    error::PlayError,
    player::track::{PlayerResource, PreparedElasticRenderer},
    resource::Resource,
};

pub(crate) enum PreparedBindingResource {}

impl PlayerImpl {
    pub(in crate::player) fn validate_session_tempo(
        &self,
        _snapshot: SessionTransportSnapshot,
        _tempo: Tempo,
        _shape: crate::player::node::StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        if binding.is_some() {
            Err(PlayError::ElasticBackendUnavailable)
        } else {
            Ok(())
        }
    }

    pub(in crate::player) fn validate_session_seek(
        &self,
        _target: SessionBeat,
        _tempo: Tempo,
        _revision: u64,
        _shape: crate::player::node::StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        if binding.is_some() {
            Err(PlayError::ElasticBackendUnavailable)
        } else {
            Ok(())
        }
    }

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
}

pub(crate) fn activate_load(
    _player_resource: &mut Option<PlayerResource>,
    activation: &mut Option<(CancelToken, PcmPool)>,
) -> Result<(), PlayError> {
    if activation.take().is_none() {
        Ok(())
    } else {
        Err(PlayError::Internal(
            "browser load unexpectedly requested elastic activation".into(),
        ))
    }
}

pub(crate) fn prepare_bound_load(
    playlist: &mut Playlist,
    index: usize,
    resource: Resource,
    prepared: Option<PreparedBindingResource>,
    context: ItemLoadContext<'_>,
) -> Result<BoundLoad, PlayError> {
    let _ = context.stamp;
    drop((prepared, context));
    restore_queued_resource(playlist, index, None, resource)?;
    Err(PlayError::Internal(
        "browser queue unexpectedly contained a bound resource".into(),
    ))
}

pub(crate) fn restore_prepared_binding(
    bound: bool,
    renderer: Option<PreparedElasticRenderer>,
    stamp: Option<PreparedBindingStamp>,
) -> Result<Option<PreparedBindingResource>, PlayError> {
    match (bound, renderer, stamp) {
        (false, None, None) => Ok(None),
        _ => Err(PlayError::Internal(
            "browser load returned inconsistent binding state".into(),
        )),
    }
}
