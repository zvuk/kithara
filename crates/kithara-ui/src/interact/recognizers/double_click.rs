use kithara_platform::time::Instant;

use crate::draw::Pt;

#[derive(Default)]
pub(crate) struct DoubleClick {
    previous: Option<(Pt, Instant)>,
}

impl DoubleClick {
    pub(crate) fn register(&mut self, position: Pt, now: Instant) -> bool {
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

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn double_click_slop_excludes_exactly_six_pixels() {
        for (separation, resets) in [(5.0, true), (6.0, false)] {
            let mut double_click = DoubleClick::default();
            let now = Instant::now();

            assert!(!double_click.register(Pt { x: 17.0, y: 17.0 }, now));
            assert_eq!(
                double_click.register(
                    Pt {
                        x: 17.0,
                        y: 17.0 + separation,
                    },
                    now,
                ),
                resets,
                "a {separation} px separation must {} reset",
                if resets { "" } else { "not" }
            );
        }
    }

    #[kithara::test]
    fn a_double_click_pair_is_spent() {
        let mut double_click = DoubleClick::default();
        let position = Pt { x: 17.0, y: 17.0 };
        let now = Instant::now();

        assert!(!double_click.register(position, now));
        assert!(double_click.register(position, now));
        assert!(
            !double_click.register(position, now),
            "the third press starts a fresh pair rather than resetting again"
        );
    }

    #[kithara::test]
    fn the_truncating_double_click_window_includes_300_5_milliseconds() {
        let mut double_click = DoubleClick::default();
        let position = Pt { x: 17.0, y: 17.0 };
        let now = Instant::now();

        assert!(!double_click.register(position, now));
        assert!(double_click.register(position, now + Duration::from_micros(300_500)));
    }
}
