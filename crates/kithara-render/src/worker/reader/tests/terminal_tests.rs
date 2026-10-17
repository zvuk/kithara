use kithara_test_utils::kithara;

use super::*;

#[kithara::test]
fn producer_drop_releases_ownership_before_the_final_deferred_wake() {
    let (receiver, producer) = packet_fixture(
        true,
        AudioSpec::new(
            2,
            std::num::NonZeroU32::new(44_100).expect("test sample rate"),
        ),
    );
    let ready = receiver.ready.as_ref().expect("blocking wake");
    let since = ready.current();
    assert!(receiver.forward.write_is_held());
    drop(producer);
    assert!(!receiver.forward.write_is_held());
    assert_eq!(ready.current().wrapping_sub(since), 1);
    assert!(
        ready.wait_timeout(since, Duration::ZERO),
        "closure between a snapshot and a wait must leave an observable wake edge"
    );
    assert!(!ready.wait_timeout(ready.current(), Duration::ZERO));
}

struct RecreateFailureSource;

impl kithara_audio::AudioSource for RecreateFailureSource {
    type Chunk = AudioChunk;
    fn step_track(&mut self) -> kithara_audio::TrackStep<AudioChunk> {
        kithara_audio::TrackStep::Failed(TrackFailureKind::RecreateFailed { offset: 0 })
    }
    fn seek(
        &mut self,
        position: Duration,
    ) -> Result<kithara_audio::SeekOutcome, kithara_audio::AudioReadError> {
        Ok(kithara_audio::SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
    fn host_sample_rate(&self) -> Option<std::num::NonZeroU32> {
        None
    }
    fn set_host_sample_rate(&mut self, _rate: std::num::NonZeroU32) {}
}

#[kithara::test]
fn terminal_recreate_failure_wakes_the_reader_once() {
    use kithara_worker::{Task, TickResult};
    let (mut node, receiver, _lane) = super::super::super::terminal_node(
        RecreateFailureSource,
        AudioSpec::new(
            2,
            std::num::NonZeroU32::new(44_100).expect("test sample rate"),
        ),
        true,
    );
    let ready = receiver.ready.as_ref().expect("blocking reader gate");
    let since = ready.current();
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(
        ready.current().wrapping_sub(since),
        1,
        "factory panic must wake the reader"
    );
    assert!(matches!(
        receiver.peek(),
        Some(PcmPacket::Failed {
            failure: TrackFailureKind::RecreateFailed { offset: 0 },
            ..
        })
    ));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(ready.current().wrapping_sub(since), 1);
}
