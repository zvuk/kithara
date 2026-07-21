use super::EngineImpl;
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo},
    error::PlayError,
    player::node::StreamShape,
    session::{PlayerId, protocol::PreparationContext},
};

impl EngineImpl {
    delegate::delegate! {
        to self.session() {
            /// Changes the tempo of the shared session transport.
            /// Returns an error when the session is inactive or rejects the update.
            pub fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError>;
            pub(crate) fn set_session_tempo_checked(
                &self,
                tempo: Tempo,
                expected_context: PreparationContext,
                player_ids: Vec<PlayerId>,
            ) -> Result<(), PlayError>;
            pub(crate) fn seek_session_checked(
                &self,
                target: SessionBeat,
                expected_context: PreparationContext,
                player_ids: Vec<PlayerId>,
            ) -> Result<(), PlayError>;
            /// Returns the last transport state processed by the audio graph.
            /// Returns an error before the active graph has processed a render block.
            pub fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError>;
            pub(crate) fn preparation_context(&self) -> Result<PreparationContext, PlayError>;
            #[call(query_stream_shape)]
            pub(crate) fn stream_shape(&self) -> Result<StreamShape, PlayError>;
        }
    }
}
