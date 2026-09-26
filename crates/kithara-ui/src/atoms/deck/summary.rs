use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::DeckSummaryStyle,
    render::Skin,
    shaping::TextContext,
    skin::{DeckSkin, TextRoleSkin},
};

/// The deck's headline: what is loaded, and where it came from.
///
/// Both looks stack the two words; the compact one leads with the source and
/// takes its type straight from the skin's roles.
#[derive(Clone, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Summary {
    #[field(get, vis = "pub(crate)", copy)]
    metrics: DeckSkin,
    style: DeckSummaryStyle,
    panel: Rgba,
    source: Rgba,
    title: Rgba,
    source_role: TextRoleSkin,
    title_role: TextRoleSkin,
}

/// What a summary is handed each frame: the track's name, and what it is
/// playing from.
#[derive(Clone, PartialEq)]
pub(crate) struct Loaded {
    pub(crate) source: String,
    pub(crate) title: String,
}

impl Summary {
    pub(crate) fn new(style: DeckSummaryStyle, skin: &Skin) -> Self {
        let metrics = skin.deck;
        let (source_role, title_role) = if style == DeckSummaryStyle::Micro {
            (metrics.micro_source, metrics.micro_title)
        } else {
            (metrics.artist, metrics.title)
        };
        Self {
            metrics,
            panel: skin.rgba(metrics.panel_color),
            source: skin.rgba(source_role.color),
            source_role,
            style,
            title: skin.rgba(title_role.color),
            title_role,
        }
    }

    /// As wide as the wider of the two words it stacks, plus the inset it
    /// draws them at. A summary shrinks to this, so measuring nothing would
    /// lay it out into a box with no width at all.
    pub(crate) fn intrinsic_width(&self, text: &mut TextContext, data: &Loaded) -> f32 {
        let title = text.shape(&data.title, self.title_role, None).width();
        let source = text.shape(&data.source, self.source_role, None).width();
        title.max(source) + self.metrics.summary_padding_x * 2.0
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Loaded,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.panel);
        let inner = Rect {
            h: (bounds.h - self.metrics.summary_padding_y * 2.0).max(0.0),
            w: (bounds.w - self.metrics.summary_padding_x * 2.0).max(0.0),
            x: bounds.x + self.metrics.summary_padding_x,
            y: bounds.y + self.metrics.summary_padding_y,
        };
        let title = text.shape(&data.title, self.title_role, None);
        let source = text.shape(&data.source, self.source_role, None);
        let compact = self.style == DeckSummaryStyle::Micro;
        let gap = if compact {
            self.metrics.micro_summary_gap
        } else {
            self.metrics.readout_gap
        };
        let [upper, lower] = if compact {
            [
                (&source, &data.source, self.source),
                (&title, &data.title, self.title),
            ]
        } else {
            [
                (&title, &data.title, self.title),
                (&source, &data.source, self.source),
            ]
        };
        let stacked = upper.0.height() + gap + lower.0.height();
        let y = inner.y + (inner.h - stacked) / 2.0;
        let mut content = list.child();
        content.text(
            upper.0,
            upper.1,
            Transform::translate(Pt { y, x: inner.x }),
            upper.2,
        );
        content.text(
            lower.0,
            lower.1,
            Transform::translate(Pt {
                x: inner.x,
                y: y + upper.0.height() + gap,
            }),
            lower.2,
        );
        list.clip(inner, content.finish());
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DeckSummaryStyle, DrawListBuilder, Loaded, Rect, Summary, TextContext};
    use crate::{atoms::painter::ControlPainter, builtin, draw::DrawList};

    mod consts {
        use super::*;

        pub(super) const BOUNDS: Rect = Rect {
            h: 44.0,
            w: 200.0,
            x: 2.0,
            y: 4.0,
        };
    }

    fn loaded(title: &str) -> Loaded {
        Loaded {
            source: "FILE".to_owned(),
            title: title.to_owned(),
        }
    }

    /// The width a summary settles for itself, asked the way a row asks it.
    fn measured(title: &str) -> f32 {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        Summary::new(DeckSummaryStyle::Default, skin).intrinsic_width(&mut text, &loaded(title))
    }

    fn drawn(style: DeckSummaryStyle) -> DrawList {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Summary::new(style, skin).paint(
            &mut list,
            &mut text,
            &Loaded {
                source: "FILE".to_owned(),
                title: "Midnight Circuit".to_owned(),
            },
            consts::BOUNDS,
        );
        list.finish()
    }

    /// The compact look sets the two words side by side and the full one
    /// stacks them, which is the whole difference between the styles.
    #[kithara::test]
    fn the_compact_style_lays_its_words_out_differently() {
        assert_ne!(
            drawn(DeckSummaryStyle::Micro),
            drawn(DeckSummaryStyle::Default)
        );
    }

    /// A summary shrinks to its own width, so it has to name one. A painter
    /// that measures nothing on the axis it shrinks along is laid out into a
    /// box of no width, and the control is gone from the page.
    #[kithara::test]
    fn a_summary_measures_a_width_of_its_own() {
        let width = measured("Midnight Circuit");

        assert!(
            width > 0.0,
            "a summary that shrinks and measures nothing has no box to draw in: {width}"
        );
    }

    /// And the width is the words, not a number the skin settled: two titles
    /// of different lengths must not measure alike.
    #[kithara::test]
    fn a_longer_title_measures_wider() {
        assert!(measured("Midnight Circuit Extended Mix") > measured("Ok"));
    }

    /// And the row asks the painter, not the atom: a width the atom knows and
    /// the painter does not report is a width the layout never sees.
    #[kithara::test]
    fn a_summary_measures_through_its_painter() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());

        let size = Summary::new(DeckSummaryStyle::Default, skin)
            .measure(&mut text, &loaded("Midnight Circuit"));

        assert!(
            size.width > 0.0,
            "the layout asks the painter for the width, and it answered nothing: {size:?}"
        );
    }

    /// Height stays the row's to give: a headline fills the panel it stands
    /// in, and a painter with no opinion on an axis reports zero there.
    #[kithara::test]
    fn a_summary_leaves_its_height_to_the_row() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());

        let size = Summary::new(DeckSummaryStyle::Default, skin)
            .measure(&mut text, &loaded("Midnight Circuit"));

        assert_eq!(size.height, 0.0);
    }

    /// A title longer than its box is cut off rather than spilling over the
    /// controls beside it.
    #[kithara::test]
    fn the_words_are_clipped_to_the_box() {
        let list = drawn(DeckSummaryStyle::Default);
        let [_, clip] = list.commands() else {
            panic!("a summary must draw its panel and one clipped run of words");
        };
        assert!(matches!(clip, crate::draw::DrawCmd::Clip { .. }));
    }
}
