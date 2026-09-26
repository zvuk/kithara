#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

mod continuity;
mod desktop;
mod switch;
mod timeline;
mod underrun;

use std::num::NonZeroU32;

use kithara::{
    abr::{AbrHandle, AbrMode},
    audio::{DecoderBackend as DecoderBackendKind, DecoderChangeCause, DecoderEvent},
    decode::DecoderBackend,
    events::{EventBus, EventReceiver},
    host::HostConfig,
    platform::{
        time::{Duration, Instant, sleep},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{Resource, ResourceConfig},
    stream::AudioCodec,
};
use kithara_integration_tests::{HlsFixtureBuilder, offline::OfflinePlayer};
use kithara_test_utils::TestTempDir;
use num_traits::ToPrimitive;
use switch::{
    AAC_HIGH, AAC_LOW, ACTIVE_SAMPLE_THRESHOLD, BLOCK_FRAMES, CHANNELS, COCHLEA_WINDOW_MS,
    ControlRender, DecoderObservation, FLAC, ORACLE_RATIO, ORACLE_SLACK, PreparedPlayer,
    REQUEST_AT_SECS, SAMPLE_RATE, SINE_HZ, TRANSITIONS, Transition, assert_initial_decoder,
    drain_decoder_events, fixture, fixture_with_signal, longest_silent_run, observation_frames,
    render_no_switch_control, render_paced, render_switch, source_frame,
};
