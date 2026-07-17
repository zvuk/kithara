use kithara_audio::ServiceClass;
use kithara_bufpool::PcmPool;
use kithara_platform::{CancelToken, sync::Arc};

use super::super::{
    core::PlayerImpl,
    state::{
        items::{BoundLoad, ItemLoadContext, ItemQueue, restore_queued_resource},
        playlist::{Playlist, PreparedBindingStamp, QueuedResource},
    },
};
use crate::{
    api::TrackBinding,
    error::PlayError,
    resource::Resource,
    rt::{
        StreamShape,
        track::{PlayerResource, PreparedElasticRenderer},
    },
};

pub(crate) struct PreparedBindingResource {
    pub(crate) renderer: PreparedElasticRenderer,
    pub(crate) stamp: PreparedBindingStamp,
}

impl PlayerImpl {
    /// Prepares and queues a stream-backed resource for session-bound elastic
    /// playback. Failure leaves the playlist unchanged.
    pub async fn insert_with_binding(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        at_position: Option<usize>,
    ) -> Result<(), PlayError> {
        let prepared = self.prepare_bound_resource(&resource, &binding).await?;
        self.core
            .items
            .insert_with_binding(resource, item_id, binding, prepared, at_position);
        Ok(())
    }
}

impl ItemQueue {
    pub(crate) fn insert_with_binding(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        prepared: PreparedBindingResource,
        at_position: Option<usize>,
    ) {
        self.insert_queued(
            QueuedResource {
                binding: Some(binding),
                item_id,
                prepared: Some(prepared),
                resource: Some(resource),
            },
            at_position,
        );
    }
}

pub(crate) fn activate_load(
    player_resource: &mut Option<PlayerResource>,
    activation: &mut Option<(CancelToken, PcmPool)>,
) -> Result<(), PlayError> {
    let Some((cancel, pool)) = activation.take() else {
        return Ok(());
    };
    player_resource
        .as_mut()
        .ok_or_else(|| PlayError::Internal("load transaction lost its resource".into()))?
        .activate_prepared_elastic(cancel, pool)
        .map_err(|error| PlayError::ElasticPreparation {
            reason: error.to_string(),
        })
}

pub(crate) fn prepare_bound_load(
    playlist: &mut Playlist,
    index: usize,
    resource: Resource,
    prepared: Option<PreparedBindingResource>,
    context: ItemLoadContext<'_>,
) -> Result<BoundLoad, PlayError> {
    let Some(prepared) = prepared else {
        restore_queued_resource(playlist, index, None, resource)?;
        return Err(PlayError::BindingPreparationRequired { index });
    };
    if prepared.stamp != context.stamp {
        restore_queued_resource(playlist, index, Some(prepared), resource)?;
        return Err(PlayError::BindingPreparationStale { index });
    }

    let PreparedBindingResource {
        renderer,
        stamp: prepared_stamp,
    } = prepared;
    let abr_handle = renderer.abr_handle();
    prepare_resource(&resource, context.rate, context.pitch_bend, context.shape);
    resource.set_service_class(ServiceClass::Idle);
    let src = Arc::clone(resource.src());
    let mut player_resource = PlayerResource::new_elastic(resource, src);
    player_resource.install_prepared_elastic(renderer);
    Ok(BoundLoad {
        player_resource,
        prepared_stamp: Some(prepared_stamp),
        abr_handle,
        activation: Some((context.cancel, context.pool.clone())),
    })
}

pub(crate) fn restore_prepared_binding(
    bound: bool,
    renderer: Option<PreparedElasticRenderer>,
    stamp: Option<PreparedBindingStamp>,
) -> Result<Option<PreparedBindingResource>, PlayError> {
    match (bound, renderer, stamp) {
        (true, Some(renderer), Some(stamp)) => {
            Ok(Some(PreparedBindingResource { renderer, stamp }))
        }
        (false, None, None) => Ok(None),
        _ => Err(PlayError::Internal(
            "rejected load returned inconsistent binding state".into(),
        )),
    }
}

fn prepare_resource(resource: &Resource, rate: f32, pitch_bend: f32, shape: StreamShape) {
    resource.set_playback_rate(rate);
    resource.set_transport_bend(pitch_bend);
    resource.set_host_sample_rate(shape.sample_rate);
}
