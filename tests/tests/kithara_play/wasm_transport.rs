use std::num::NonZeroU32;

use kithara::{
    audio::{
        BeatGrid, PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome, TrackBeat,
        analysis::TrackAnalysis,
    },
    bufpool::{BytePool, PcmPool},
    decode::{DecodeError, PcmSpec, TrackMetadata},
    events::EventBus,
    platform::{sync::Arc, time::Duration},
    play::{
        MultiPlayer, PlayError, PlaybackDirection, PlayerConfig, PlayerImpl, Resource, SessionBeat,
        Tempo, TrackBinding,
    },
};
use kithara_integration_tests::kithara;

struct EmptyReader {
    bus: EventBus,
    spec: PcmSpec,
    metadata: TrackMetadata,
}

impl Default for EmptyReader {
    fn default() -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            spec: PcmSpec::new(2, sample_rate()),
        }
    }
}

impl PcmRead for EmptyReader {
    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl PcmSession for EmptyReader {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl PcmControl for EmptyReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
}

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(44_100).expect("static sample rate")
}

fn binding() -> TrackBinding {
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(
            120.0,
            vec![0, 22_050, 44_100],
            vec![0],
            Vec::new(),
        )),
        None,
        44_100,
        sample_rate(),
    );
    TrackBinding::new(
        &analysis,
        sample_rate(),
        SessionBeat::new(0.0).expect("valid session anchor"),
        TrackBeat::new(0.0).expect("valid track anchor"),
        PlaybackDirection::Forward,
    )
    .expect("analysed track can be bound")
}

fn player() -> PlayerImpl {
    PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .build(),
    )
}

#[kithara::test(browser, timeout(Duration::from_secs(10)))]
async fn browser_player_uses_shared_transport_contract() {
    let players = MultiPlayer::default();
    players
        .register(player())
        .expect("browser player registers");
    let error = players
        .set_session_tempo(Tempo::new(128.0).expect("valid tempo"))
        .expect_err("an unbound player has no registered session participant");
    assert!(matches!(error, PlayError::NotReady));

    let player = player();
    let resource = Resource::from_reader(EmptyReader::default(), Some(Arc::from("memory://wasm")));
    let error = player
        .insert_with_binding(resource, None, binding(), None)
        .await
        .expect_err("browser reports its elastic capability explicitly");

    assert!(matches!(error, PlayError::ElasticBackendUnavailable));
}
