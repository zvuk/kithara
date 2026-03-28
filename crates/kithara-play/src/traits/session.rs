pub use kithara_events::{PortDescription, PortType, RouteDescription};
use kithara_platform::{MaybeSend, MaybeSync, time::Duration};

use crate::error::PlayError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionCategory {
    Ambient,
    #[default]
    SoloAmbient,
    Playback,
    Record,
    PlayAndRecord,
    MultiRoute,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionMode {
    #[default]
    Default,
    VoiceChat,
    VideoChat,
    GameChat,
    Measurement,
    MoviePlayback,
    SpokenAudio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionOptions {
    pub mix_with_others: bool,
    pub duck_others: bool,
    pub allow_bluetooth: bool,
    pub allow_bluetooth_a2dp: bool,
    pub allow_air_play: bool,
    pub default_to_speaker: bool,
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = AudioSessionMock)
)]
pub trait AudioSession: MaybeSend + MaybeSync + 'static {
    fn category(&self) -> SessionCategory;

    fn set_category(&self, category: SessionCategory) -> Result<(), PlayError>;

    fn set_category_with_options(
        &self,
        category: SessionCategory,
        mode: SessionMode,
        options: SessionOptions,
    ) -> Result<(), PlayError>;

    fn mode(&self) -> SessionMode;

    fn set_mode(&self, mode: SessionMode) -> Result<(), PlayError>;

    fn is_active(&self) -> bool;

    fn set_active(&self, active: bool) -> Result<(), PlayError>;

    fn sample_rate(&self) -> f64;

    fn set_preferred_sample_rate(&self, rate: f64) -> Result<(), PlayError>;

    fn output_channels(&self) -> u16;

    fn set_preferred_output_channels(&self, count: u16) -> Result<(), PlayError>;

    fn output_latency(&self) -> Duration;

    fn io_buffer_duration(&self) -> Duration;

    fn set_preferred_io_buffer_duration(&self, duration: Duration) -> Result<(), PlayError>;

    fn output_volume(&self) -> f32;

    fn current_route(&self) -> RouteDescription;

    fn is_other_audio_playing(&self) -> bool;
}
