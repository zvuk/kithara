#![cfg(not(target_arch = "wasm32"))]

use kithara_audio::elastic::{ElasticPreparationPoll, ElasticReader, Unprepared};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep},
    tokio::select,
};

use super::super::{core::PlayerImpl, state::PreparedBindingResource};
use crate::{
    api::{PlaybackDirection, SessionBeat, TrackBinding},
    error::PlayError,
    player::track::{PlayerResource, plan_elastic_anchor},
    resource::Resource,
    session::protocol::PreparationContext,
};

impl PlayerImpl {
    fn current_preparation_context(&self) -> Result<PreparationContext, PlayError> {
        self.core.engine.preparation_context()
    }

    pub(in crate::player) async fn prepare_bound_resource(
        &self,
        resource: Resource,
        binding: &TrackBinding,
    ) -> Result<(Resource, PreparedBindingResource), PlayError> {
        self.prepare_bound_resource_at(resource, binding, binding.session_anchor())
            .await
    }

    pub(in crate::player) async fn prepare_bound_resource_at(
        &self,
        mut resource: Resource,
        binding: &TrackBinding,
        anchor: SessionBeat,
    ) -> Result<(Resource, PreparedBindingResource), PlayError> {
        self.ensure_engine_started()?;
        let cancel = self
            .core
            .engine
            .cancel_token()
            .ok_or_else(|| PlayError::Internal("player preparation has no cancel owner".into()))?
            .child();
        let context = self.current_preparation_context()?;
        let binding_sample_rate = binding.map().host_sample_rate();
        if binding_sample_rate != context.shape().sample_rate {
            return Err(PlayError::BindingSampleRateMismatch {
                binding_sample_rate: binding_sample_rate.get(),
                stream_sample_rate: context.shape().sample_rate.get(),
            });
        }
        ensure_preparation_active(&cancel)?;
        if binding.direction() == PlaybackDirection::Reverse && !resource.supports_reverse_source()
        {
            return Err(PlayError::ReverseSourceUnavailable);
        }
        select! {
            biased;
            () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
            result = resource.preload() => result.map_err(|error| PlayError::ItemFailed {
                reason: error.to_string(),
            }),
        }?;
        ensure_preparation_active(&cancel)?;
        resource.set_playback_rate(self.core.timestretch.speed());
        resource.set_transport_bend(self.core.params.pitch_bend());
        resource.set_host_sample_rate(context.shape().sample_rate);
        let reader = ElasticReader::<Unprepared>::allocate(
            resource.spec(),
            binding.map().source_frame_count(),
            context.shape().sample_rate,
            context.shape().max_block_frames,
            PlayerResource::OUTPUT_CHANNELS,
            self.core.elastic,
            self.core.engine.pcm_pool(),
        )
        .map_err(|error| PlayError::ElasticPreparation {
            reason: error.to_string(),
        })?;
        let elastic_anchor =
            plan_elastic_anchor(binding, anchor, context.tempo(), reader.capabilities()).map_err(
                |error| PlayError::ElasticPreparation {
                    reason: error.to_string(),
                },
            )?;
        let mut preparation = reader
            .begin(&mut resource, elastic_anchor)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })?;

        let reader = loop {
            match preparation.poll(&mut resource).map_err(|error| {
                PlayError::ElasticPreparation {
                    reason: error.to_string(),
                }
            })? {
                ElasticPreparationPoll::Ready(reader) => break reader,
                ElasticPreparationPoll::Pending(next) => {
                    preparation = next;
                    wait_for_preparation_poll(&cancel).await?;
                }
            }
        };

        if context != self.current_preparation_context()? {
            return Err(PlayError::BindingPreparationContextChanged);
        }

        Ok((resource, PreparedBindingResource { context, reader }))
    }
}

fn ensure_preparation_active(cancel: &CancelToken) -> Result<(), PlayError> {
    if cancel.is_cancelled() {
        Err(PlayError::BindingPreparationCancelled)
    } else {
        Ok(())
    }
}

async fn wait_for_preparation_poll(cancel: &CancelToken) -> Result<(), PlayError> {
    select! {
        biased;
        () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
        () = sleep(Duration::from_millis(1)) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::CancelScope;
    use kithara_test_utils::kithara;

    use super::wait_for_preparation_poll;
    use crate::error::PlayError;

    #[kithara::test(tokio)]
    async fn cancelled_preparation_wait_returns_typed_error() {
        let scope = CancelScope::new(None);
        let cancel = scope.token();
        scope.cancel();

        assert!(matches!(
            wait_for_preparation_poll(&cancel).await,
            Err(PlayError::BindingPreparationCancelled)
        ));
    }
}
