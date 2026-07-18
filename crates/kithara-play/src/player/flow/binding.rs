#![cfg(not(target_arch = "wasm32"))]

use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Duration, sleep},
    tokio::select,
};

use super::super::{
    core::PlayerImpl,
    state::{PreparedBindingResource, PreparedBindingStamp},
};
use crate::{
    api::{SessionBeat, Tempo, TrackBinding},
    error::PlayError,
    player::track::{ElasticPreparationOutcome, PlayerResource},
    resource::Resource,
};

impl PlayerImpl {
    pub(in crate::player) async fn prepare_bound_resource(
        &self,
        resource: &Resource,
        binding: &TrackBinding,
    ) -> Result<PreparedBindingResource, PlayError> {
        self.prepare_bound_resource_at(resource, binding, binding.session_anchor())
            .await
    }

    pub(in crate::player) async fn prepare_bound_resource_at(
        &self,
        resource: &Resource,
        binding: &TrackBinding,
        anchor: SessionBeat,
    ) -> Result<PreparedBindingResource, PlayError> {
        self.ensure_engine_started()?;
        let cancel = self
            .core
            .engine
            .cancel_token()
            .ok_or_else(|| PlayError::Internal("player preparation has no cancel owner".into()))?
            .child();
        let (tempo, stamp) = self.current_binding_preparation()?;
        let binding_sample_rate = binding.map().host_sample_rate();
        if binding_sample_rate != stamp.shape.sample_rate {
            return Err(PlayError::BindingSampleRateMismatch {
                binding_sample_rate: binding_sample_rate.get(),
                stream_sample_rate: stamp.shape.sample_rate.get(),
            });
        }
        ensure_preparation_active(&cancel)?;
        let blueprint = resource
            .blueprint()
            .ok_or(PlayError::BindingSourceNotReopenable)?;
        let src = Arc::clone(resource.src());
        let mut prepared_resource = select! {
            biased;
            () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
            result = blueprint.open_isolated() => result.map_err(|error| PlayError::ItemFailed {
                reason: error.to_string(),
            }),
        }?;
        select! {
            biased;
            () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
            result = prepared_resource.preload() => result.map_err(|error| PlayError::ItemFailed {
                reason: error.to_string(),
            }),
        }?;
        let mut relocation_resource = select! {
            biased;
            () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
            result = blueprint.open_isolated() => result.map_err(|error| PlayError::ItemFailed {
                reason: error.to_string(),
            }),
        }?;
        select! {
            biased;
            () = cancel.cancelled() => Err(PlayError::BindingPreparationCancelled),
            result = relocation_resource.preload() => result.map_err(|error| PlayError::ItemFailed {
                reason: error.to_string(),
            }),
        }?;
        ensure_preparation_active(&cancel)?;
        prepared_resource.set_playback_rate(self.core.timestretch.speed());
        prepared_resource.set_transport_bend(self.core.params.pitch_bend());
        prepared_resource.set_host_sample_rate(stamp.shape.sample_rate);
        relocation_resource.set_playback_rate(self.core.timestretch.speed());
        relocation_resource.set_transport_bend(self.core.params.pitch_bend());
        relocation_resource.set_host_sample_rate(stamp.shape.sample_rate);
        if !relocation_resource
            .activate_source_audio_authoritative()
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })?
        {
            return Err(PlayError::ElasticPreparation {
                reason: "relocation source audio is unavailable".into(),
            });
        }
        let mut prepared = PlayerResource::new_elastic(prepared_resource, src);
        prepared
            .prepare_elastic(
                binding,
                anchor,
                tempo,
                stamp.transport_revision,
                stamp.shape,
                self.core.engine.pcm_pool(),
            )
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })?;

        loop {
            match prepared.poll_elastic_preparation().map_err(|error| {
                PlayError::ElasticPreparation {
                    reason: error.to_string(),
                }
            })? {
                ElasticPreparationOutcome::Ready => break,
                ElasticPreparationOutcome::Pending => {
                    wait_for_preparation_poll(&cancel).await?;
                }
            }
        }

        let (_, current_stamp) = self.current_binding_preparation()?;
        if stamp != current_stamp {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        let renderer = prepared
            .finish_elastic_preparation(relocation_resource)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })?;

        Ok(PreparedBindingResource { renderer, stamp })
    }

    fn current_binding_preparation(&self) -> Result<(Tempo, PreparedBindingStamp), PlayError> {
        let preparation = self.core.engine.binding_preparation()?;
        Ok((
            preparation.tempo,
            PreparedBindingStamp::new(preparation.shape, preparation.revision),
        ))
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
