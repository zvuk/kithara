pub(super) use std::num::{NonZeroU32, NonZeroUsize};
#[cfg(not(target_os = "android"))]
pub(super) use std::{env, io};

#[cfg(not(target_os = "android"))]
pub(super) use kithara::{
    assets::{AssetResource, AssetResourceState, AssetSource, AssetStore, ReadSide, ResourceKey},
    encode::EncodeConfig,
    host::Host,
    output::{OfflineRenderRequest, OfflineRenderer},
    platform::CancelScope,
    record::{RecordingConfig, RecordingCore, RecordingSink},
    signal::AudioSpec,
};
pub(super) use kithara::{
    beat::{BeatGridModel, BeatGridState, GridBeat, RawBeatGrid, SCHEMA_VERSION},
    hls::AbrMode,
    host::{
        HostConfig, HostSettings, HostSettingsControl, MetronomeConfig, MetronomeConfigControl,
        Tap, api::Tempo,
    },
    link::{
        GridAnswer, LinkConfig, LinkedConfig, LinkedFactory, LinkedHost, LinkedHostCommand,
        LinkedHostConfig, TempoStep, TempoTrajectory,
    },
    platform::{
        sync::{Arc, Mutex},
        time::{self, Duration, Instant},
    },
    play::{
        ArtifactSource, PlayWorker, PlayWorkerConfig, ResourceConfig, ResourcePrep, ResourceSrc,
    },
    queue::{Queue, QueueConfig, TrackSource, TrackStatus},
    signal::SessionFrame,
    sync::SessionBeat,
};
#[cfg(not(target_os = "android"))]
pub(super) use kithara_app::recording::AssetPartSink;
pub(super) use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    cochlea::{marked_synchronization_failures, synchronization_failures},
    grid::{Start, analysed_grid},
    kithara, memory_asset_store,
    offline::{
        OfflineHostHarness, TapProbe,
        linked::{LinkObservation, LinkedOwner, LinkedQueueHandle, ObservedFactory},
    },
    usdt_trace,
};
pub(super) use num_traits::AsPrimitive;
