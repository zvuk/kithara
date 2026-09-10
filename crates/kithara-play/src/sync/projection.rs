use kithara_warp::{RateTarget, RenderContext, SessionAnchor, SyncMode};

/// Immutable projection of the deck owner into the output callback.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum DeckGrid {
    #[default]
    Off,
    Host,
    Local(SessionAnchor),
}

impl DeckGrid {
    pub(crate) fn project(
        self,
        context: &RenderContext,
        rate: RateTarget,
    ) -> Option<RenderContext> {
        match self {
            Self::Off => Some(context.clone().with_rate(SyncMode::Off, rate)),
            Self::Host => Some(context.clone().with_rate(SyncMode::HostSync, rate)),
            Self::Local(anchor) => {
                if anchor.sample_rate() != context.sample_rate() {
                    return None;
                }
                let frames = context.output_frames();
                let beats = anchor.beat_at(frames.start).ok()?..anchor.beat_at(frames.end).ok()?;
                RenderContext::new(
                    frames.clone(),
                    context.sample_rate(),
                    Some(beats),
                    context.session_epoch(),
                    context.transport_revision(),
                )
                .map(|context| context.with_rate(SyncMode::LocalSync, rate))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use kithara_warp::{
        RateTarget, RenderContext, SessionAnchor, SessionBeat, SessionEpoch, SessionFrame,
        SyncMode, TransportRevision,
    };

    use super::DeckGrid;

    fn context(end_beat: f64) -> RenderContext {
        RenderContext::new(
            SessionFrame::new(0)..SessionFrame::new(48_000),
            NonZeroU32::new(48_000).expect("sample rate"),
            Some(SessionBeat::default()..SessionBeat::new(end_beat).expect("beat")),
            SessionEpoch::new(0),
            Some(TransportRevision::first()),
        )
        .expect("context")
    }

    #[kithara::test]
    fn local_ninety_bpm_is_independent_of_host_one_twenty() {
        let host = context(2.0);
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            1.5,
            host.sample_rate(),
        )
        .expect("local anchor");
        let local = DeckGrid::Local(anchor)
            .project(&host, RateTarget::default())
            .expect("local context");
        assert_eq!(local.mode(), SyncMode::LocalSync);
        assert_eq!(
            local.session_beats(),
            Some(&(SessionBeat::default()..SessionBeat::new(1.5).expect("beat")))
        );
        assert_eq!(local.output_frames(), host.output_frames());
    }

    #[kithara::test]
    fn host_sync_uses_each_current_output_span() {
        for end in [2.0, 3.0] {
            let host = context(end);
            let projected = DeckGrid::Host
                .project(&host, RateTarget::default())
                .expect("host context");
            assert_eq!(projected.mode(), SyncMode::HostSync);
            assert_eq!(projected.session_beats(), host.session_beats());
        }
    }
}
