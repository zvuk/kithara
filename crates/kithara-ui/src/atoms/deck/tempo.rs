use crate::{
    atoms::{deck::clock::clock_reading, design::quad::center_y},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{DeckSkin, TextRoleSkin},
};

mod consts {
    /// What the caption above a position reading says.
    pub(super) const ELAPSED: &str = "TIME";
}

/// The deck's tempo, or where it is when no tempo has been measured.
#[derive(Clone, PartialEq, kithara_derive::ControlPainter)]
#[control_painter(
    data = Reading,
    draw = self.paint(list, text, data, bounds)
)]
#[derive(kithara_derive::Retained)]
pub(crate) struct Tempo {
    metrics: DeckSkin,
    caption: Rgba,
    panel: Rgba,
    reading: Rgba,
    caption_role: TextRoleSkin,
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
        Self {
            caption: skin.rgba(metrics.readout_label.color),
            caption_role: metrics.readout_label,
            metrics,
            panel: skin.rgba(metrics.panel_color),
            reading: skin.rgba(metrics.bpm_text.color),
            role: metrics.bpm_text,
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
                let caption = text.shape(consts::ELAPSED, self.caption_role, None);
                let run = text.shape(&content, self.role, None);
                let stacked = caption.height() + self.metrics.readout_gap + run.height();
                let y = bounds.y + (bounds.h - stacked) / 2.0;
                list.text(
                    &caption,
                    consts::ELAPSED,
                    Transform::translate(Pt {
                        y,
                        x: bounds.x + (bounds.w - caption.width()) / 2.0,
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

    mod consts {
        use super::*;

        pub(super) const BOUNDS: Rect = Rect {
            h: 34.0,
            w: 56.0,
            x: 1.0,
            y: 2.0,
        };
    }

    fn drawn(data: Reading) -> DrawList {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Tempo::new(skin).paint(&mut list, &mut text, &data, consts::BOUNDS);
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
