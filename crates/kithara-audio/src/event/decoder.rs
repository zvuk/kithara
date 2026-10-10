#![forbid(unsafe_code)]

use kithara_decode::GaplessInfo;
use kithara_events::Event;
use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ContainerFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderBackend {
    Symphonia,
    Apple,
    Android,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderChangeCause {
    Initial,
    VariantSwitch,
    FormatBoundary,
    HostRateChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeErrorClass {
    Interrupted,
    VariantChange,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeErrorKind {
    Io,
    UnsupportedCodec,
    UnsupportedContainer,
    InvalidData,
    SeekFailed,
    SeekOutOfRange,
    Parse,
    ProbeFailed,
    BackendUnavailable,
    InvalidSampleRate,
    BackendStatus,
    Interrupted,
    Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDomain {
    Source,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerKind {
    Rubato,
    Apple,
    Glide,
    None,
}

#[derive(Debug, Clone, Event)]
pub enum DecoderEvent {
    DecoderChanged {
        backend: DecoderBackend,
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        sample_rate: u32,
        channels: u16,
        bit_depth: Option<u16>,
        bitrate: Option<u32>,
        cause: DecoderChangeCause,
        variant: Option<u32>,
        base_offset: u64,
        duration: Option<Duration>,
        gapless: Option<GaplessInfo>,
    },
    DecodeError {
        class: DecodeErrorClass,
        kind: DecodeErrorKind,
        codec: Option<AudioCodec>,
        detail: &'static str,
    },
    GaplessResolved {
        leading_frames: u64,
        trailing_frames: u64,
        domain: FrameDomain,
        codec: Option<AudioCodec>,
        sample_rate: u32,
    },
    ResamplerConfigured {
        backend: ResamplerKind,
        input_rate: u32,
        output_rate: u32,
        channels: u16,
        bypassed: bool,
    },
    /// The playing generation is being held back for an in-flight variant
    /// transition: its output stays parked (and, when `source_exhausted`,
    /// its EOF stays unsurfaced) until the transition promotes or fails,
    /// so a pending switch cannot be outrun by the outgoing track's end.
    TransitionHold { source_exhausted: bool },
}
