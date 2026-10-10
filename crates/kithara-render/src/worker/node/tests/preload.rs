use std::{future::poll_fn, task::Poll};

use kithara_audio::{AudioSource, TrackFailureKind};
use kithara_signal::AudioChunk;
use kithara_worker::{Task, TickResult};

use crate::{DecoderNode, LaneTask, LoadRefusal, test_pools::TestPools};

pub(super) async fn preload<T>(node: &mut DecoderNode<T, TestPools>) -> Result<(), TrackFailureKind>
where
    T: AudioSource<Chunk = AudioChunk>,
{
    node.warm_up();
    poll_fn(|context| {
        let _ = node.poll_commands(context);
        node.recycle();
        let result = node.tick();
        match node.preload_status() {
            Ok(true) => Poll::Ready(Ok(())),
            Err(LoadRefusal::Source(error)) => Poll::Ready(Err(error)),
            Err(error) => panic!("unexpected preload refusal: {error:?}"),
            Ok(false) => {
                if result == TickResult::Progress {
                    context.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
    })
    .await
}
