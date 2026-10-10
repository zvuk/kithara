mod core;
mod terminal_tests;
mod transport_tests;

use kithara_audio::TrackFailureKind;
use kithara_decode::TrackMetadata;
use kithara_platform::{
    sync::{Arc, ThreadGate, WaitGate},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioSpec, SegmentId};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use triple_buffer::triple_buffer;

pub(in crate::worker) use self::core::packet_fixture;
pub(crate) use self::core::{PacketRing, chunk};
use super::{super::scheduler::StreamWake, core::*, packet::PcmPacket};
