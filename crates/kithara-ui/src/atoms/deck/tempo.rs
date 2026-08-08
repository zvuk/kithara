use crate::{
    atoms::{deck::clock::clock_reading, design::quad::center_y},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{ColorRole, DeckSkin, FontFamily, TextRoleSkin},
    text::TextContext,
};

/// What the caption above a position reading says.
const ELAPSED: &str = "TIME";

/// The deck's tempo, or where it is when no tempo has been measured.
pub(crate) struct Tempo {
    caption: Rgba,
    caption_role: TextRoleSkin,
    metrics: DeckSkin,
    panel: Rgba,
    reading: Rgba,
    role: TextRoleSkin,
}

/// What a tempo readout is handed each frame.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Reading {
    /// The measured tempo of the loaded track.
    Bpm(f64),
    /// Where the deck is, under a caption of its own, shown when the track has
    /// no tempo to report.
    Position(f64),
}

impl Tempo {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.deck;
        let role = |font: crate::skin::FontSkin, color| TextRoleSkin {
            color,
            font: FontFamily::Mono,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        };
        Self {
            caption: skin.palette.muted,
            caption_role: role(metrics.readout_label, ColorRole::Muted),
            metrics,
            panel: skin.palette.bg_panel,
            reading: skin.palette.accent_strong,
            role: role(metrics.bpm_text, ColorRole::AccentStrong),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Reading,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.panel);
        match *data {
            Reading::Bpm(bpm) => {
                let content = format!("{bpm:.2}");
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
            Reading::Position(seconds) => {
                let content = clock_reading(seconds);
                let caption = text.shape(ELAPSED, self.caption_role, None);
                let run = text.shape(&content, self.role, None);
                let stacked = caption.height() + self.metrics.readout_gap + run.height();
                let y = bounds.y + (bounds.h - stacked) / 2.0;
                list.text(
                    &caption,
                    ELAPSED,
                    Transform::translate(Pt {
                        x: bounds.x + (bounds.w - caption.width()) / 2.0,
                        y,
                    }),
                    self.caption,
                );
                list.text(
                    &run,
                    &content,
                    Transform::translate(Pt {
                        x: bounds.x + (bounds.w - run.width()) / 2.0,
                        y: y + caption.height() + self.metrics.readout_gap,
                    }),
                    self.reading,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Reading, Rect, Tempo, TextContext};
    use crate::{builtin, draw::DrawList};

    const BOUNDS: Rect = Rect {
        h: 34.0,
        w: 56.0,
        x: 1.0,
        y: 2.0,
    };

    fn drawn(data: Reading) -> DrawList {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Tempo::new(skin).paint(&mut list, &mut text, &data, BOUNDS);
        list.finish()
    }

    /// A tempo is one reading; a position is a caption and a reading, because
    /// a bare clock in a tempo's place would read as a tempo.
    #[kithara::test]
    fn a_position_is_captioned_and_a_tempo_is_not() {
        assert_eq!(drawn(Reading::Bpm(128.0)).commands().len(), 2);
        assert_eq!(drawn(Reading::Position(61.0)).commands().len(), 3);
    }

    #[kithara::test]
    fn a_tempo_is_shown_to_two_places() {
        let list = drawn(Reading::Bpm(70.0));
        let [_, crate::draw::DrawCmd::Text { content, .. }] = list.commands() else {
            panic!("a tempo must draw its panel and one reading");
        };

        assert_eq!(content, "70.00");
    }
}
