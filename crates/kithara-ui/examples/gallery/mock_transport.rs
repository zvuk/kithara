use num_traits::cast::AsPrimitive;

pub(super) struct DeckTransport {
    bpm: f64,
    cues: Vec<f32>,
    duration_secs: f64,
    loop_region: Option<[f32; 2]>,
    playing: bool,
    position_secs: f64,
    reverse: bool,
    zoom: f64,
}

impl DeckTransport {
    const BARS_PER_LOOP: f64 = 4.0;
    const BEATS_PER_BAR: f64 = 4.0;
    const MAX_CUES: usize = 4;
    const MAX_ZOOM: f64 = 0.5;
    const MIN_ZOOM: f64 = 0.015;
    const SECS_PER_MINUTE: f64 = 60.0;
    const ZOOM_FACTOR: f64 = 0.7;

    pub(super) fn new(
        bpm: f32,
        cues: &[f32],
        duration_secs: f64,
        loop_region: [f32; 2],
        position_secs: f64,
        zoom: f64,
    ) -> Self {
        Self {
            bpm: f64::from(bpm),
            cues: cues.to_vec(),
            duration_secs,
            loop_region: Some(loop_region),
            playing: true,
            position_secs,
            reverse: false,
            zoom,
        }
    }

    pub(super) fn activate(&mut self, path: &str) -> bool {
        let action = path.rsplit('/').next();
        if !path.contains("/transport/") {
            return false;
        }
        match action {
            Some("cue") => self.set_cue(),
            Some("jump-back") => self.jump_bars(-1.0),
            Some("jump-forward") => self.jump_bars(1.0),
            Some("loop") => self.toggle_loop(),
            Some("reverse") => self.reverse = !self.reverse,
            Some("zoom-in") => self.zoom = (self.zoom * Self::ZOOM_FACTOR).max(Self::MIN_ZOOM),
            Some("zoom-out") => self.zoom = (self.zoom / Self::ZOOM_FACTOR).min(Self::MAX_ZOOM),
            _ => return false,
        }
        true
    }

    pub(super) fn cues(&self) -> &[f32] {
        &self.cues
    }

    pub(super) const fn loop_region(&self) -> Option<[f32; 2]> {
        self.loop_region
    }

    pub(super) const fn playing(&self) -> bool {
        self.playing
    }

    pub(super) const fn position_secs(&self) -> f64 {
        self.position_secs
    }

    pub(super) fn position_normalized(&self) -> f64 {
        self.position_secs / self.duration_secs
    }

    pub(super) const fn reverse(&self) -> bool {
        self.reverse
    }

    pub(super) fn seek_normalized(&mut self, position: f64) {
        self.position_secs = position.clamp(0.0, 1.0) * self.duration_secs;
    }

    pub(super) fn set_zoom(&mut self, zoom: f64) {
        self.zoom = zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    pub(super) fn toggle_play(&mut self) {
        self.playing = !self.playing;
    }

    pub(super) const fn zoom(&self) -> f64 {
        self.zoom
    }

    fn jump_bars(&mut self, bars: f64) {
        let delta = bars * Self::BEATS_PER_BAR * Self::SECS_PER_MINUTE / self.bpm;
        self.position_secs = (self.position_secs + delta).clamp(0.0, self.duration_secs);
    }

    fn set_cue(&mut self) {
        let cue = self.position_normalized().as_();
        if let Err(index) = self.cues.binary_search_by(|probe| probe.total_cmp(&cue))
            && self.cues.len() < Self::MAX_CUES
        {
            self.cues.insert(index, cue);
        }
    }

    fn toggle_loop(&mut self) {
        if self.loop_region.take().is_some() {
            return;
        }
        let loop_secs =
            Self::BARS_PER_LOOP * Self::BEATS_PER_BAR * Self::SECS_PER_MINUTE / self.bpm;
        let start = self.position_normalized();
        let end = ((self.position_secs + loop_secs) / self.duration_secs).min(1.0);
        self.loop_region = Some([start.as_(), end.as_()]);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    struct Consts;

    impl Consts {
        const BPM: f32 = 70.0;
        const CUES: &[f32] = &[0.27, 0.31];
        const DURATION_SECS: f64 = 360.0;
        const LOOP_REGION: [f32; 2] = [0.30, 0.34];
        const POSITION_SECS: f64 = 103.0;
        const ZOOM: f64 = 0.12;
    }

    fn transport() -> DeckTransport {
        DeckTransport::new(
            Consts::BPM,
            Consts::CUES,
            Consts::DURATION_SECS,
            Consts::LOOP_REGION,
            Consts::POSITION_SECS,
            Consts::ZOOM,
        )
    }

    #[kithara::test]
    fn cue_adds_current_position_without_duplicates_and_stops_at_four() {
        let mut transport = transport();

        transport.activate("modules/deck/transport/cue");
        transport.activate("modules/deck/transport/cue");
        transport.seek_normalized(0.5);
        transport.activate("modules/deck/transport/cue");
        transport.seek_normalized(0.75);
        transport.activate("modules/deck/transport/cue");

        assert_eq!(transport.cues().len(), 4);
        assert!(
            transport
                .cues()
                .contains(&(Consts::POSITION_SECS / Consts::DURATION_SECS).as_())
        );
        assert!(transport.cues().contains(&0.5));
        assert!(!transport.cues().contains(&0.75));
    }

    #[kithara::test]
    fn loop_toggle_replaces_initial_region_with_four_bars_from_position() {
        let mut transport = transport();

        transport.activate("modules/deck/transport/loop");
        assert_eq!(transport.loop_region(), None);

        transport.seek_normalized(0.25);
        transport.activate("modules/deck/transport/loop");
        let four_bars = DeckTransport::BARS_PER_LOOP
            * DeckTransport::BEATS_PER_BAR
            * DeckTransport::SECS_PER_MINUTE
            / f64::from(Consts::BPM);
        assert_eq!(
            transport.loop_region(),
            Some([0.25, (0.25 + four_bars / Consts::DURATION_SECS).as_()])
        );
    }

    #[kithara::test]
    fn beat_jump_moves_one_bar_and_clamps_to_track_bounds() {
        let mut transport = transport();
        let one_bar =
            DeckTransport::BEATS_PER_BAR * DeckTransport::SECS_PER_MINUTE / f64::from(Consts::BPM);

        transport.seek_normalized(0.5);
        transport.activate("modules/deck/transport/jump-back");
        assert_eq!(
            transport.position_secs(),
            Consts::DURATION_SECS * 0.5 - one_bar
        );
        transport.seek_normalized(0.999);
        transport.activate("modules/deck/transport/jump-forward");
        assert_eq!(transport.position_normalized(), 1.0);
        transport.seek_normalized(0.001);
        transport.activate("modules/deck/transport/jump-back");
        assert_eq!(transport.position_normalized(), 0.0);
    }

    #[kithara::test]
    fn zoom_buttons_use_wheel_factor_and_clamp() {
        let mut transport = transport();

        transport.activate("modules/deck/transport/zoom-out");
        assert_eq!(transport.zoom(), Consts::ZOOM / DeckTransport::ZOOM_FACTOR);
        transport.set_zoom(0.49);
        transport.activate("modules/deck/transport/zoom-out");
        assert_eq!(transport.zoom(), DeckTransport::MAX_ZOOM);
        transport.set_zoom(0.016);
        transport.activate("modules/deck/transport/zoom-in");
        assert_eq!(transport.zoom(), DeckTransport::MIN_ZOOM);
    }
}
