use super::EngineImpl;
use crate::{
    api::{SessionTransportSnapshot, Tempo},
    error::PlayError,
    player::node::StreamShape,
    session::{PlayerId, protocol::BindingPreparation},
};

impl EngineImpl {
    /// Changes the tempo of the shared session transport.
    /// Returns an error when the session is inactive or rejects the update.
    pub fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
        self.session().set_session_tempo(tempo)
    }

    pub(crate) fn set_session_tempo_checked(
        &self,
        tempo: Tempo,
        expected_revision: u64,
        expected_shape: StreamShape,
        player_ids: Vec<PlayerId>,
    ) -> Result<(), PlayError> {
        self.session().set_session_tempo_checked(
            tempo,
            expected_revision,
            expected_shape,
            player_ids,
        )
    }

    /// Returns the last transport state processed by the audio graph.
    /// Returns an error before the active graph has processed a render block.
    pub fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
        self.session().session_transport()
    }

    pub(crate) fn binding_preparation(&self) -> Result<BindingPreparation, PlayError> {
        self.session().binding_preparation()
    }

    pub(crate) fn stream_shape(&self) -> Result<StreamShape, PlayError> {
        self.session().query_stream_shape()
    }
}
