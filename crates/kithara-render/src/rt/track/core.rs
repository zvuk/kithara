use std::num::NonZeroU32;

use bon::bon;
use kithara_dsp::param::SmootherConfig;
use kithara_platform::sync::Arc;
use kithara_signal::{SegmentId, SessionFrame};

use super::{PlayerResource, fade::TrackFade, gate::TrackGate};
use crate::{
    CrossfadeCurve, CrossfadeSettings,
    bridge::{Fade, FadeDir, SlotMark, SlotState},
    consts::DEFAULT_DECLICK,
};

/// A slot's packet consumer and data-driven envelope.
pub struct PlayerTrack {
    pub(super) resource: Box<PlayerResource>,
    pub(super) fade: TrackFade,
    pub(super) gate: TrackGate,
    pub(super) state: SlotState,
    pub(super) gap: u32,
    pub(super) stop_at: Option<SessionFrame>,
    pub(super) stop_resume: Option<SlotMark>,
    sample_rate: NonZeroU32,
    declick: SmootherConfig,
}

#[bon]
impl PlayerTrack {
    #[builder]
    #[must_use]
    pub fn new(
        #[builder(finish_fn)] mut resource: Box<PlayerResource>,
        sample_rate: NonZeroU32,
        #[builder(default = DEFAULT_DECLICK)] declick: SmootherConfig,
        #[builder(default = SegmentId::FIRST)] segment: SegmentId,
    ) -> Self {
        resource.select_segment(segment);
        Self {
            resource,
            fade: TrackFade::default(),
            gate: TrackGate::new(false, declick, sample_rate),
            state: SlotState::Stopped,
            gap: 0,
            stop_at: None,
            stop_resume: None,
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
            pub fn decoded_frontier(&self) -> f64;
            #[must_use]
            pub fn cached_span(&self) -> f64;
            #[must_use]
            pub fn src(&self) -> &Arc<str>;
            #[must_use]
            pub fn segment(&self) -> SegmentId;
            #[must_use]
            pub fn mark(&self, session: SessionFrame) -> Option<SlotMark>;
        }
        to self {
            #[expr(self.stop_resume)]
            pub(crate) fn stop_resume(&self) -> Option<SlotMark>;
        }
    }

    #[must_use]
    pub fn position(&self) -> f64 {
        self.resource
            .position()
            .and_then(|point| point.seconds_at(0))
            .unwrap_or(0.0)
    }

    pub(crate) fn frames_until_boundary(&self) -> Option<usize> {
        let packet = self.resource.frames_until_boundary();
        if self.fade.is_fading_out() {
            let fade = usize::try_from(self.fade.remaining()).unwrap_or(usize::MAX);
            Some(packet.map_or(fade, |frames| frames.min(fade)))
        } else {
            packet
        }
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

    pub(crate) fn stop(&mut self, fade: Fade, at: SessionFrame) {
        self.gap = 0;
        self.stop_resume = None;
        if self.state == SlotState::Playing {
            match fade {
                Fade::Declick => self.fade.declick_out(self.settings(fade), self.sample_rate),
                Fade::Crossfade(settings) => self.fade.fade_out(settings, self.sample_rate),
            }
            let frames = i64::try_from(self.fade.remaining()).unwrap_or(i64::MAX);
            self.stop_at = Some(SessionFrame::new(i64::from(at).saturating_add(frames)));
            if self.fade.remaining() > 0 {
                return;
            }
        } else {
            self.stop_at = Some(at);
        }
        self.settle_stop();
    }

    pub fn fade(&mut self, settings: CrossfadeSettings, dir: FadeDir) {
        match dir {
            FadeDir::In => self.fade.fade_in(settings, self.sample_rate),
            FadeDir::Out => self.fade.fade_out(settings, self.sample_rate),
        }
        if dir == FadeDir::Out && self.fade.remaining() == 0 {
            self.settle_stop();
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

    pub(crate) fn recycle_obsolete(&mut self, budget: &mut usize) {
        self.resource.refresh_mark(budget);
        if self.state == SlotState::Stopped && self.stop_at.is_some() {
            self.settle_stop();
        }
    }

    pub(crate) fn clear_stop(&mut self) {
        self.stop_at = None;
        self.stop_resume = None;
    }

    pub(crate) fn interrupt_stop(&mut self, at: SessionFrame) -> Option<SlotMark> {
        let resume = self.stop_resume.or_else(|| self.resource.mark(at));
        self.clear_stop();
        resume
    }

    pub(super) fn settle_stop(&mut self) {
        self.state = SlotState::Stopped;
        self.shut();
        self.fade.stop(self.sample_rate);
        if let Some(at) = self.stop_at {
            self.stop_resume = self.resource.mark(at);
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
