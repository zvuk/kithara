use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_bufpool::PoolConfig;
use kithara_decode::{DecodeError, DecoderChunkOutcome, GaplessMode, SilenceTrimParams};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioChunkInfo};
use kithara_stream::{SourcePhase, Stream};
use kithara_test_fixtures::unit_fixtures::{RoutePcm, route_pcm};
use kithara_test_utils::kithara;

use crate::{
    AudioEvent, AudioLaneEvent, DecoderChangeCause, DecoderEvent, TrackFailureKind, consts,
    pipeline::{
        decode::{DecoderGeneration, core::DecoderFactory, transition::OutgoingFrontier},
        fetch::SourceEnd,
        track::{TrackStep, WaitingReason},
    },
    test_pools::{pools, pools_with, sample_buffer},
    traits::AudioSource,
};

mod fixtures;
mod lifecycle;
mod route;
pub(in crate::pipeline::source) use fixtures::*;
