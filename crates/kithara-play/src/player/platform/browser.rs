use kithara_audio::ServiceClass;

use super::PlayerResource;
use crate::{
    api::{SessionBeat, Tempo, TrackBinding},
    error::PlayError,
};

pub(crate) struct PreparedElasticRenderer {
    _private: (),
}

impl PreparedElasticRenderer {
    pub(super) const fn decoded_frontier(&self) -> f64 {
        0.0
    }

    pub(super) fn set_service_class(&mut self, _class: ServiceClass) {}
}

impl PlayerResource {
    pub(crate) fn begin_session_seek(
        &mut self,
        _binding: &TrackBinding,
        _target: SessionBeat,
        _tempo: Tempo,
        _revision: u64,
    ) -> Result<(), PlayError> {
        Err(PlayError::ElasticBackendUnavailable)
    }

    pub(crate) fn poll_session_seek(&mut self, _revision: u64) -> Result<bool, PlayError> {
        Err(PlayError::ElasticBackendUnavailable)
    }

    pub(crate) fn cancel_session_seek(&mut self, _revision: u64) -> Result<(), PlayError> {
        Ok(())
    }
}
