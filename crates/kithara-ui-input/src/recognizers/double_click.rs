use kithara_platform::time::Instant;
use kithara_ui_draw::Pt;

#[derive(Default)]
pub struct DoubleClick {
    previous: Option<(Pt, Instant)>,
}

impl DoubleClick {
    pub fn register(&mut self, position: Pt, now: Instant) -> bool {
        let consecutive = self
            .previous
            .is_some_and(|(previous_position, previous_time)| {
                previous_position.distance(position) < 6.0
                    && now.saturating_duration_since(previous_time).as_millis() <= 300
            });
        self.previous = (!consecutive).then_some((position, now));
        consecutive
    }
}
