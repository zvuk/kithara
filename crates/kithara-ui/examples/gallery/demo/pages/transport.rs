use kithara_ui::render::{Zoom, zoom_in, zoom_out};
use num_traits::cast::AsPrimitive;

fn zoom_from_f64(value: f64) -> Zoom {
    let value: f32 = value.as_();
    value.into()
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct DeckTransport {
    #[field(get, vis = "pub(crate)", copy)]
    loop_region: Option<[f32; 2]>,
    #[field(get, vis = "pub(crate)")]
    cues: Vec<f32>,
    zoom: Zoom,
    #[field(get, vis = "pub(crate)")]
    playing: bool,
    #[field(get, vis = "pub(crate)")]
    reverse: bool,
    loop_anchor: f32,
    bpm: f64,
    duration_secs: f64,
    #[field(get, vis = "pub(crate)")]
    position_secs: f64,
}

impl DeckTransport {
    const BARS_PER_LOOP: f64 = 4.0;
    const BEATS_PER_BAR: f64 = 4.0;
    const MAX_CUES: usize = 4;
    const SECS_PER_MINUTE: f64 = 60.0;

    pub(crate) fn new(
        bpm: f32,
        cues: &[f32],
        duration_secs: f64,
        loop_region: [f32; 2],
        position_secs: f64,
        zoom: f64,
    ) -> Self {
        Self {
            duration_secs,
            position_secs,
            zoom: zoom_from_f64(zoom),
            bpm: f64::from(bpm),
            cues: cues.to_vec(),
            loop_anchor: loop_region[0],
            loop_region: Some(loop_region),
            playing: true,
            reverse: false,
        }
    }

    pub(crate) fn activate(&mut self, path: &str) -> bool {
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
            Some("zoom-in") => self.zoom = zoom_in(self.zoom),
            Some("zoom-out") => self.zoom = zoom_out(self.zoom),
            _ => return false,
        }
        true
    }

    fn jump_bars(&mut self, bars: f64) {
        let delta = bars * Self::BEATS_PER_BAR * Self::SECS_PER_MINUTE / self.bpm;
        self.position_secs = (self.position_secs + delta).clamp(0.0, self.duration_secs);
    }

    pub(crate) fn position_normalized(&self) -> f64 {
        self.position_secs / self.duration_secs
    }

    pub(crate) fn seek_normalized(&mut self, position: f64) {
        self.position_secs = position.clamp(0.0, 1.0) * self.duration_secs;
    }

    fn set_cue(&mut self) {
        let cue = self.position_normalized().as_();
        if let Err(index) = self.cues.binary_search_by(|probe| probe.total_cmp(&cue))
            && self.cues.len() < Self::MAX_CUES
        {
            self.cues.insert(index, cue);
        }
    }

    pub(crate) fn set_loop_end(&mut self, end: f64) {
        self.loop_region = normalized_loop(self.loop_anchor, end.clamp(0.0, 1.0).as_());
    }

    pub(crate) fn set_loop_start(&mut self, start: f64) {
        self.loop_anchor = start.clamp(0.0, 1.0).as_();
        self.loop_region = None;
    }

    pub(crate) fn set_zoom(&mut self, zoom: f64) {
        self.zoom = zoom_from_f64(zoom);
    }

    fn toggle_loop(&mut self) {
        if self.loop_region.take().is_some() {
            return;
        }
        let loop_secs =
            Self::BARS_PER_LOOP * Self::BEATS_PER_BAR * Self::SECS_PER_MINUTE / self.bpm;
        let start = self.position_normalized();
        let end = ((self.position_secs + loop_secs) / self.duration_secs).min(1.0);
        self.loop_anchor = start.as_();
        self.loop_region = Some([start.as_(), end.as_()]);
    }

    pub(crate) const fn toggle_play(&mut self) {
        self.playing = !self.playing;
    }

    pub(crate) fn zoom(&self) -> f64 {
        f64::from(f32::from(self.zoom))
    }
}

fn normalized_loop(start: f32, end: f32) -> Option<[f32; 2]> {
    (start != end).then(|| [start.min(end), start.max(end)])
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    mod consts {
        pub(super) const BPM: f32 = 70.0;
        pub(super) const CUES: &[f32] = &[0.27, 0.31];
        pub(super) const DURATION_SECS: f64 = 360.0;
        pub(super) const LOOP_REGION: [f32; 2] = [0.30, 0.34];
        pub(super) const POSITION_SECS: f64 = 103.0;
        pub(super) const ZOOM: f64 = 0.12;
    }

    fn transport() -> DeckTransport {
        DeckTransport::new(
            consts::BPM,
            consts::CUES,
            consts::DURATION_SECS,
            consts::LOOP_REGION,
            consts::POSITION_SECS,
            consts::ZOOM,
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
                .contains(&(consts::POSITION_SECS / consts::DURATION_SECS).as_())
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
            / f64::from(consts::BPM);
        assert_eq!(
            transport.loop_region(),
            Some([0.25, (0.25 + four_bars / consts::DURATION_SECS).as_()])
        );
    }

    #[kithara::test]
    fn loop_drag_sets_region_from_start_to_end() {
        let mut transport = transport();

        transport.set_loop_start(0.4);
        transport.set_loop_end(0.5);

        assert_eq!(transport.loop_region(), Some([0.4, 0.5]));
    }

    #[kithara::test]
    fn loop_drag_normalizes_reverse_direction() {
        let mut transport = transport();

        transport.set_loop_start(0.5);
        transport.set_loop_end(0.4);

        assert_eq!(transport.loop_region(), Some([0.4, 0.5]));
    }

    #[kithara::test]
    fn zero_length_loop_drag_clears_region() {
        let mut transport = transport();

        transport.set_loop_start(0.4);
        transport.set_loop_end(0.4);

        assert_eq!(transport.loop_region(), None);
    }

    #[kithara::test]
    fn beat_jump_moves_one_bar_and_clamps_to_track_bounds() {
        let mut transport = transport();
        let one_bar =
            DeckTransport::BEATS_PER_BAR * DeckTransport::SECS_PER_MINUTE / f64::from(consts::BPM);

        transport.seek_normalized(0.5);
        transport.activate("modules/deck/transport/jump-back");
        assert_eq!(
            transport.position_secs(),
            consts::DURATION_SECS * 0.5 - one_bar
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
        assert_eq!(
            transport.zoom(),
            f64::from(f32::from(zoom_out(zoom_from_f64(consts::ZOOM))))
        );
        transport.set_zoom(0.49);
        transport.activate("modules/deck/transport/zoom-out");
        assert_eq!(transport.zoom(), f64::from(f32::from(Zoom::MAX)));
        transport.set_zoom(0.016);
        transport.activate("modules/deck/transport/zoom-in");
        assert_eq!(transport.zoom(), f64::from(f32::from(Zoom::MIN)));
    }
}
