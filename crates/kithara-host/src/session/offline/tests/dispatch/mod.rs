use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::AtomicBool,
};

use firewheel::{
    FirewheelContext,
    channel_config::{ChannelConfig, ChannelCount},
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
    },
};
use kithara_command::{When, mailbox};
use kithara_config::{Config, ConfigOwner};
use kithara_output::OutputGroup;
use kithara_platform::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use kithara_play::{
    BufferGeometryError, PlayError, PlayWorker, PlayWorkerConfig, ResourcePrep, Tempo,
};
use kithara_queue::{Queue, QueueConfig};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_warp::{BeatGridId, BeatGridSnapshot, BeatGridState, BeatGridUnavailable, MapAxis};
use ringbuf::{HeapRb, traits::Split};

use super::super::backend::{BackendConfig, OfflineStream};
use crate::{
    HostCommand, HostOwner, HostSettingsExec,
    api::{SessionDuckingMode, Tap},
    bridge::MixTapWriter,
    host::HostSettingsChange,
    rt::MetronomeConfigChange,
    session::{
        applied_spans,
        dispatch::{OwnerPosts, tick_session},
        protocol::{SessionError, SessionSampleRate},
        state::{DeckNode, SessionStream, TapSlot, add_graph_node},
    },
};

mod command_contracts;
mod manual_crossfade;
mod membership;
#[cfg(feature = "mock")]
mod mock;
mod restart;
mod restored_engine_config;
mod restored_graph;
mod restored_identity;
mod restored_lifecycle;
mod restored_mix;
mod settings;

use mock::*;
