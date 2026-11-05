use std::num::NonZeroU32;

use kithara_config::Config;
use kithara_signal::SessionFrame;
use kithara_sync::{SessionBeat, TempoStep, TempoTrajectory};

use crate::{
    MetronomeConfig, PlayError,
    api::{SESSION_BEATS_PER_BAR, SessionDuckingMode, Tempo},
    consts,
};

/// What a Host runs with that changes while it runs.
///
/// A change of one field goes to the Host through
/// [`kithara_config::Configure`], at the next block or at a session frame;
/// the getters read the settings as the render last applied them.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(
    default,
    builder(state_mod(vis = "pub")),
    check(error = PlayError),
    fields(value, get(copy))
)]
#[non_exhaustive]
pub struct HostSettings {
    /// Rate the Host asks the output to run at, 44.1 kHz unless changed. A
    /// change restarts the output route at the new rate.
    #[config(live(owner), builder(default = consts::DEFAULT_SAMPLE_RATE))]
    sample_rate: NonZeroU32,
    /// Tempo the session transport counts beats in, 120 BPM unless changed.
    #[config(live(owner), builder(default = Tempo::DEFAULT))]
    tempo: Tempo,
    /// How the Host metronome sounds, switched off unless changed.
    #[config(nested, live, builder(default))]
    metronome: MetronomeConfig,
    /// How far the whole session output is lowered under a competing sound,
    /// not at all unless changed.
    #[config(live, builder(default))]
    ducking: SessionDuckingMode,
}

impl HostSettings {
    /// The accepted tempo map a session starts from: session beat 0 on frame
    /// 0 at these settings' tempo and rate, [`SESSION_BEATS_PER_BAR`] beats
    /// to the bar.
    #[must_use]
    pub fn trajectory(&self) -> TempoTrajectory {
        TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: self.tempo,
            },
            SESSION_BEATS_PER_BAR,
            self.sample_rate,
        )
    }
}
