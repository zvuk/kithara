use std::{num::NonZeroU32, ops::Range};

use bon::bon;
use kithara_command::Seq;
use kithara_dsp::param::SmootherConfig;
use kithara_events::TrackId;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{SegmentId, SessionFrame};

use super::{PlayerResource, RtSink, fade::TrackFade, gate::TrackGate};
use crate::{
    CrossfadeCurve, CrossfadeSettings,
    bridge::{DeckEvent, Fade, FadeDir, SlotMark, SlotState},
    consts::DEFAULT_DECLICK,
};

#[derive(Clone, Copy, Default)]
enum StopState {
    #[default]
    Idle,
    Pending {
        seq: Seq,
        deadline: SessionFrame,
    },
    Done {
        seq: Seq,
        resume: Option<SlotMark>,
    },
}

/// A slot's packet consumer and session-time envelope.
pub struct PlayerTrack {
    pub(super) item_id: TrackId,
    pub(super) resource: Box<PlayerResource>,
    pub(super) fade: TrackFade,
    pub(super) gate: TrackGate,
    pub(super) state: SlotState,
    pub(super) gap: u32,
    stop: StopState,
    sample_rate: NonZeroU32,
    declick: SmootherConfig,
}

#[bon]
impl PlayerTrack {
    #[builder]
    #[must_use]
    pub fn new(
        #[builder(finish_fn)] mut resource: Box<PlayerResource>,
        item_id: TrackId,
        sample_rate: NonZeroU32,
        #[builder(default = DEFAULT_DECLICK)] declick: SmootherConfig,
        #[builder(default = SegmentId::FIRST)] segment: SegmentId,
    ) -> Self {
        resource.select_segment(segment);
        Self {
            item_id,
            resource,
            fade: TrackFade::default(),
            gate: TrackGate::new(false, declick, sample_rate),
            state: SlotState::Stopped,
            gap: 0,
            stop: StopState::Idle,
            sample_rate,
            declick,
        }
    }

    #[must_use]
    pub const fn state(&self) -> SlotState {
        self.state
    }

    delegate::delegate! {
        to self.resource {
            #[must_use]
            pub fn duration(&self) -> f64;
            #[must_use]
            pub(crate) fn source_info(&self) -> Option<kithara_signal::AudioChunkInfo>;
            #[must_use]
            pub fn visible_origin(&self) -> Option<Duration>;
            #[must_use]
            pub fn media_origin(&self) -> Option<Duration>;
            #[must_use]
            pub fn decoded_frontier(&self) -> f64;
            #[must_use]
            pub fn cached_span(&self) -> f64;
            #[must_use]
            pub fn src(&self) -> &Arc<str>;
            #[must_use]
            pub fn segment(&self) -> SegmentId;
            #[must_use]
            pub fn mark(&self, session: SessionFrame) -> Option<SlotMark>;
            #[call(refresh_mark)]
            pub(crate) fn recycle_obsolete(&mut self, budget: &mut usize);
        }
    }

    #[must_use]
    pub fn position(&self) -> f64 {
        self.resource
            .position()
            .and_then(|point| point.seconds_at(0))
            .unwrap_or(0.0)
    }

    pub(crate) fn frames_until_boundary(&self, at: SessionFrame) -> Option<usize> {
        let packet = (self.state == SlotState::Playing)
            .then(|| self.resource.frames_until_boundary())
            .flatten();
        let fade = self
            .fade
            .is_fading_out()
            .then(|| usize::try_from(self.fade.remaining()).unwrap_or(usize::MAX));
        let stop = match self.stop {
            StopState::Pending { deadline, .. } => Some(
                deadline
                    .frames_since(at)
                    .map_or(0, |frames| usize::try_from(frames).unwrap_or(usize::MAX)),
            ),
            StopState::Idle | StopState::Done { .. } => None,
        };
        [packet, fade, stop].into_iter().flatten().min()
    }

    #[must_use]
    pub fn gain(&self) -> f32 {
        if self.state == SlotState::Playing {
            self.fade.gain()
        } else {
            0.0
        }
    }

    fn settings(&self, fade: Fade) -> CrossfadeSettings {
        match fade {
            Fade::Declick => CrossfadeSettings {
                duration: self.declick.smooth_seconds.max(0.0),
                curve: CrossfadeCurve::Linear,
                depth: 0.0,
                position: 0.5,
            },
            Fade::Crossfade(settings) => settings,
        }
    }

    pub fn start(&mut self, fade: Fade) {
        self.fade.fade_in(self.settings(fade), self.sample_rate);
        if self.fade.remaining() == 0 {
            self.fade.play(self.sample_rate);
        }
        self.gate.steer(true);
        self.gate.snap();
        self.state = SlotState::Playing;
        self.gap = 0;
        self.resource.set_playing(true);
    }

    pub(crate) fn stop(&mut self, seq: Seq, fade: Fade, at: SessionFrame) {
        self.gap = 0;
        if self.state == SlotState::Playing {
            match fade {
                Fade::Declick => self.fade.declick_out(self.settings(fade), self.sample_rate),
                Fade::Crossfade(settings) => self.fade.fade_out(settings, self.sample_rate),
            }
            let frames = i64::try_from(self.fade.remaining()).unwrap_or(i64::MAX);
            self.stop = StopState::Pending {
                seq,
                deadline: SessionFrame::new(i64::from(at).saturating_add(frames)),
            };
            if self.fade.remaining() > 0 {
                return;
            }
        } else {
            self.stop = StopState::Pending { seq, deadline: at };
        }
        self.settle_stop(at);
    }

    pub fn fade(&mut self, settings: CrossfadeSettings, dir: FadeDir) {
        match dir {
            FadeDir::In => self.fade.fade_in(settings, self.sample_rate),
            FadeDir::Out => self.fade.fade_out(settings, self.sample_rate),
        }
    }

    pub(crate) fn adopt(&mut self, segment: SegmentId) {
        let ended = self.state == SlotState::Ended;
        self.resource.select_segment(segment);
        self.gap = 0;
        if ended {
            self.start(Fade::Declick);
        } else if self.state == SlotState::Playing {
            self.gate.steer(false);
            self.gate.snap();
            self.gate.steer(true);
        }
    }

    pub(super) const fn stopping(&self) -> bool {
        matches!(self.stop, StopState::Pending { .. })
    }

    pub(crate) fn interrupt_stop(&mut self, at: SessionFrame) {
        self.finish_stop(at);
    }

    pub(crate) fn take_stop(&mut self) -> Option<(Seq, Option<SlotMark>)> {
        let StopState::Done { seq, resume } = self.stop else {
            return None;
        };
        self.stop = StopState::Idle;
        Some((seq, resume))
    }

    pub(super) fn finish_stop(&mut self, at: SessionFrame) {
        if let StopState::Pending { seq, .. } = self.stop {
            self.stop = StopState::Done {
                seq,
                resume: self.resource.mark(at),
            };
        }
    }

    fn settle_stop(&mut self, at: SessionFrame) {
        self.finish_stop(at);
        self.silence();
    }

    fn silence(&mut self) {
        self.state = SlotState::Stopped;
        self.shut();
        self.fade.stop();
    }

    pub(crate) fn advance(&mut self, range: Range<usize>, sink: &mut RtSink<'_>) {
        let fading_out = self.fade.is_fading_out();
        let remaining = self.fade.remaining();
        self.fade.advance(range.len());
        if fading_out && self.fade.settled() && self.fade.gain() == 0.0 {
            if self.state == SlotState::Playing {
                self.state = SlotState::Stopped;
            }
            self.shut();
            let offset = usize::try_from(remaining).unwrap_or(usize::MAX);
            self.finish_underrun(range.start.saturating_add(offset), sink);
            sink.report(DeckEvent::Faded {
                slot: sink.slot,
                at: sink.at(range.start.saturating_add(offset)),
            });
        }
        if let StopState::Pending { deadline, .. } = self.stop
            && deadline <= sink.at(range.end)
        {
            self.settle_stop(deadline);
        }
    }

    pub(crate) fn snap_gate(&mut self) {
        self.gate.snap();
    }

    pub(in crate::rt) fn shut(&mut self) {
        self.gate.steer(false);
        self.gate.snap();
        self.resource.set_playing(false);
    }

    pub fn set_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.fade.update_sample_rate(sample_rate);
        self.gate.update_sample_rate(sample_rate);
        self.sample_rate = sample_rate;
    }

    #[must_use]
    pub fn into_resource(mut self) -> Box<PlayerResource> {
        self.resource.set_playing(false);
        self.resource
    }
}
