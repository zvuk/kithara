use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_audio::ConsumerWakeMode;
use kithara_platform::sync::{Arc, Mutex};

use super::{AllocatedSlot, Cmd, Reply, SessionDispatcher, SessionSampleRate};
use crate::{
    PlayError, SessionDuckingMode, SharedEq, SlotId,
    bridge::{NodeInputs, slot_channels},
    rt::StreamShape,
};

struct TestSession {
    next_player: AtomicU64,
    next_slot: AtomicU64,
    nodes: Mutex<Vec<NodeInputs>>,
}

impl<S> SessionDispatcher<S> for TestSession {
    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        let reply = match cmd {
            Cmd::RegisterPlayer { .. } => {
                Reply::PlayerRegistered(self.next_player.fetch_add(1, Ordering::Relaxed))
            }
            Cmd::AllocateSlot { .. } => {
                let slot = SlotId::new(self.next_slot.fetch_add(1, Ordering::Relaxed));
                let (inputs, control) = slot_channels(SharedEq::new(10));
                self.nodes.lock().push(inputs);
                Reply::SlotAllocated(AllocatedSlot::new(control, slot))
            }
            Cmd::QuerySampleRate => Reply::SampleRate(SessionSampleRate::new(None, 44_100)),
            Cmd::QueryStreamShape => Reply::StreamShape(StreamShape {
                max_block_frames: NonZeroU32::new(128).expect("fixture block size is non-zero"),
                sample_rate: NonZeroU32::new(44_100).expect("fixture sample rate is non-zero"),
            }),
            Cmd::SessionDucking => Reply::SessionDucking(SessionDuckingMode::Off),
            _ => Reply::Ok,
        };
        Ok(reply)
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

pub(crate) fn test_session<S>() -> Arc<dyn SessionDispatcher<S>> {
    Arc::new(TestSession {
        next_player: AtomicU64::new(1),
        next_slot: AtomicU64::new(0),
        nodes: Mutex::default(),
    })
}
