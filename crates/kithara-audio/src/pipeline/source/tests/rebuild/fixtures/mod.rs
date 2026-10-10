use std::{
    io::Cursor,
    num::NonZeroU32,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo,
    GaplessMode, GaplessProfile,
};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{
    sync::{Arc, Condvar, Mutex, Notify},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, ActivityWriter, AudioCodec, ByteMap, ContainerFormat, DeferredWake, MediaInfo,
    OpenedReader, OpenedVariantReader, PlayheadRead, PlayheadState, PlayheadWrite, PrerollHint,
    ReadOutcome, ReaderProfile, SegmentDescriptor, Source, SourceError, SourcePhase, SourceProbe,
    SourceSeekAnchor, Stream, StreamError, StreamResult, StreamType, VariantControl,
    VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition, VariantTransitionId,
    mock::NoopWorkerWake,
};
use kithara_test_fixtures::unit_fixtures::RoutePcm;

use crate::{
    consts,
    pipeline::{
        decode::{
            DecoderGeneration,
            core::{ActiveDecode, DecoderFactory},
        },
        fetch::{Fetch, SourceEnd},
        rebuild::{RecreateCause, RecreateState},
        source::{SourceDecoderConfig, StreamAudioSource},
        stream::shared::SharedStream,
        track::TrackStep,
    },
    test_pools::{Pools, pools, sample_buffer},
    traits::AudioSource,
};

mod doubles;
mod route;
pub(in crate::pipeline::source) use doubles::*;
pub(in crate::pipeline::source) use route::*;
