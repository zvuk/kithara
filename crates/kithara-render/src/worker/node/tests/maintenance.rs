use std::num::NonZeroU32;

use futures::channel::oneshot;
use kithara_audio::{AudioReadError, AudioSource, SeekOutcome};
use kithara_platform::sync::mpsc;

use super::core::*;
use crate::worker::PcmPacket;

struct DeferredSource {
    source: ScriptedSource,
    entered: Option<oneshot::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl AudioSource for DeferredSource {
    type Chunk = AudioChunk;

    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).expect("announce source preparation");
            self.release.recv().expect("release source preparation");
        }
        self.source.prepare_deferred()
    }

    delegate::delegate! {
        to self.source {
            fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
            fn seek_for_render(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
            fn host_sample_rate(&self) -> Option<NonZeroU32>;
            fn native_pcm_basis(&self) -> Option<kithara_signal::AudioChunkInfo>;
            fn step_track(&mut self) -> TrackStep<AudioChunk>;
        }
    }
}

#[kithara::test(tokio)]
#[case::playing(true, false)]
#[case::paused(false, false)]
#[case::cancelled(true, true)]
async fn ready_pcm_does_not_wait_for_deferred_source_preparation(
    #[case] playing: bool,
    #[case] cancelled: bool,
) {
    let (entered, preparing) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let next = chunk(spec, SegmentId::FIRST, 0, 1, &[0.5, 0.5]);
    let source = DeferredSource {
        source: ScriptedSource::new([produced(), TrackStep::Produced(Fetch::data(next))]),
        entered: Some(entered),
        release: released,
    };
    let cancel = kithara_test_utils::cancel_token();
    let initial = kithara_signal::AudioChunkInfo {
        spec,
        ..kithara_signal::AudioChunkInfo::default()
    };
    let (mut node, mut receiver, _lane) = crate::worker::source_fixture::prepared_node_with_cancel(
        source,
        1,
        1,
        initial,
        Some(cancel.clone()),
    )
    .await;
    receiver.set_playing(playing);
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(node.pending.is_some(), "PCM is ready behind the full port");
    assert!(receiver.pop().is_some(), "consumer frees the full port");
    if cancelled {
        cancel.cancel();
    }
    let worker = kithara_platform::tokio::task::spawn_blocking(move || {
        node.recycle();
        let result = node.tick();
        node.recycle();
        result
    });
    preparing.await.expect("source preparation entered");
    let admitted = receiver.pop();
    release.send(()).expect("release blocked preparation");
    assert_eq!(
        worker.await.expect("worker completed"),
        TickResult::Progress
    );
    assert_eq!(
        admitted.is_some(),
        playing && !cancelled,
        "playing PCM precedes source preparation; paused and cancelled maintenance stays live"
    );
    assert!(matches!(
        admitted.or_else(|| receiver.pop()),
        Some(PcmPacket::Chunk(packet)) if packet.meta.lane_frame == 1
    ));
}
