use std::fmt;

use kithara_audio::TrackFailureKind;
use kithara_command::{Protocol, Target};
use kithara_effects::{GainDb, eq::EqLayout};
use kithara_signal::{SegmentId, SessionFrame};

use super::{DeckMixSettingsChange, SlotMark};
use crate::{CrossfadeSettings, rt::track::PlayerResource};

/// One scoped deck's commands, clock and verdicts.
#[derive(Debug)]
pub enum DeckProtocol {}

impl Protocol for DeckProtocol {
    type Applied = ();
    type Clock = SessionFrame;
    type Command = DeckPart;
    type Refusal = DeckRefusal;
    type Target = Slot;

    fn frames_since(at: SessionFrame, start: SessionFrame) -> Option<u64> {
        at.frames_since(start)
    }
}

/// A slot assigned by the deck owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Slot(u16);

impl Slot {
    #[must_use]
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Target for Slot {
    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A whole-batch refusal, before any part changes the deck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeckRefusal {
    Occupied { slot: Slot },
    Empty { slot: Slot },
    Outdated { slot: Slot },
    Deferral,
    AxisRestarted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fade {
    Declick,
    Crossfade(CrossfadeSettings),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FadeDir {
    In,
    Out,
}

/// Parts are applied in order after validation against the whole batch.
pub enum DeckPart {
    Attach {
        slot: Slot,
        pcm: Box<PlayerResource>,
        segment: SegmentId,
    },
    Detach {
        slot: Slot,
    },
    Start {
        slot: Slot,
        fade: Fade,
    },
    Stop {
        slot: Slot,
        fade: Fade,
    },
    Adopt {
        slot: Slot,
        segment: SegmentId,
    },
    Fade {
        slot: Slot,
        settings: CrossfadeSettings,
        dir: FadeDir,
    },
    Replace {
        slot: Slot,
        pcm: Box<PlayerResource>,
        segment: SegmentId,
    },
    Chain {
        from: Slot,
        to: Slot,
    },
    Mix(DeckMixSettingsChange),
    Eq(DeckEqChange),
    Returned(Returned),
}

/// Owning values travel back in their receipt, never dropped by the mixer.
pub enum Returned {
    Pcm {
        slot: Slot,
        pcm: Box<PlayerResource>,
    },
    Eq(Box<EqLayout>),
    Stopped {
        slot: Slot,
        resume: SlotMark,
    },
}

#[derive(Debug)]
pub enum DeckEqChange {
    Gain { band: usize, gain: GainDb },
    Layout(Box<EqLayout>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeckEvent {
    Ended {
        slot: Slot,
        at: SessionFrame,
    },
    Failed {
        slot: Slot,
        at: SessionFrame,
        fault: PlaybackFault,
    },
    Faded {
        slot: Slot,
        at: SessionFrame,
    },
    Underrun {
        slot: Slot,
        at: SessionFrame,
        frames: u32,
    },
}

impl fmt::Debug for DeckPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attach { slot, pcm, segment } | Self::Replace { slot, pcm, segment } => formatter
                .debug_struct(if matches!(self, Self::Attach { .. }) {
                    "Attach"
                } else {
                    "Replace"
                })
                .field("slot", slot)
                .field("src", pcm.src())
                .field("segment", segment)
                .finish(),
            Self::Detach { slot } => formatter
                .debug_struct("Detach")
                .field("slot", slot)
                .finish(),
            Self::Start { slot, fade } | Self::Stop { slot, fade } => formatter
                .debug_struct(if matches!(self, Self::Start { .. }) {
                    "Start"
                } else {
                    "Stop"
                })
                .field("slot", slot)
                .field("fade", fade)
                .finish(),
            Self::Adopt { slot, segment } => formatter
                .debug_struct("Adopt")
                .field("slot", slot)
                .field("segment", segment)
                .finish(),
            Self::Fade {
                slot,
                settings,
                dir,
            } => formatter
                .debug_struct("Fade")
                .field("slot", slot)
                .field("settings", settings)
                .field("dir", dir)
                .finish(),
            Self::Chain { from, to } => formatter
                .debug_struct("Chain")
                .field("from", from)
                .field("to", to)
                .finish(),
            Self::Mix(change) => formatter.debug_tuple("Mix").field(change).finish(),
            Self::Eq(change) => formatter.debug_tuple("Eq").field(change).finish(),
            Self::Returned(returned) => formatter.debug_tuple("Returned").field(returned).finish(),
        }
    }
}

impl fmt::Debug for Returned {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pcm { slot, pcm } => formatter
                .debug_struct("Pcm")
                .field("slot", slot)
                .field("src", pcm.src())
                .finish(),
            Self::Eq(_) => formatter.write_str("Eq"),
            Self::Stopped { slot, resume } => formatter
                .debug_struct("Stopped")
                .field("slot", slot)
                .field("resume", resume)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SlotState {
    #[default]
    Empty,
    Stopped,
    Playing,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackFault {
    Source(TrackFailureKind),
    OutputRateMismatch,
    OutputRangeUnavailable,
}

impl fmt::Display for PlaybackFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(failure) => write!(formatter, "{failure}"),
            Self::OutputRateMismatch => formatter.write_str("output sample-rate mismatch"),
            Self::OutputRangeUnavailable => {
                formatter.write_str("render context has no output range")
            }
        }
    }
}
