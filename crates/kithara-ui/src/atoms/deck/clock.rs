use num_traits::ToPrimitive;

use crate::{
    atoms::design::quad::center_y,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{FontFamily, TextRoleSkin},
};

const SECONDS_PER_MINUTE: u64 = 60;

/// A pair of clock readings, centred on the deep panel behind them.
pub(crate) struct Clock {
    deep: Rgba,
    reading: Rgba,
    role: TextRoleSkin,
}

/// What a clock is handed each frame: where the track is, and how long it is.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Elapsed {
    pub(crate) duration: f64,
    pub(crate) position: f64,
}

impl Clock {
    pub(crate) fn new(skin: &Skin) -> Self {
        Self {
            deep: skin.palette.bg_deep,
            reading: skin.palette.accent_strong,
            role: TextRoleSkin {
                color: crate::skin::ColorRole::AccentStrong,
                font: FontFamily::Mono,
                size: skin.deck.time_text.size,
                spacing: 0.0,
                weight: skin.deck.time_text.weight,
            },
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Elapsed,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.deep);
        let content = format!(
            "{} / {}",
            clock_reading(data.position),
            clock_reading(data.duration)
        );
        let run = text.shape(&content, self.role, None);
        list.text(
            &run,
            &content,
            Transform::translate(Pt {
                x: bounds.x + (bounds.w - run.width()) / 2.0,
                y: center_y(bounds, &run),
            }),
            self.reading,
        );
    }
}

/// Seconds as minutes and seconds, both zero-padded so the reading keeps one
/// width as the track plays.
pub(crate) fn clock_reading(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u64().unwrap_or(0);
    let minutes = total / SECONDS_PER_MINUTE;
    let seconds = total % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::clock_reading;

    #[kithara::test]
    fn a_reading_is_zero_padded_minutes_and_seconds() {
        assert_eq!(clock_reading(0.0), "00:00");
        assert_eq!(clock_reading(61.4), "01:01");
        assert_eq!(clock_reading(3600.0), "60:00");
    }

    #[kithara::test]
    fn a_negative_reading_reads_as_the_start() {
        assert_eq!(clock_reading(-5.0), "00:00");
    }
}
