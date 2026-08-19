use crate::{
    atoms::design::quad::center_y,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::DeckSummaryStyle,
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, DeckSkin, FontFamily, TextRoleSkin},
    solve::{Length, Size},
};

/// The deck's headline: what is loaded, and where it came from.
///
/// The compact look puts the two beside each other; the full one stacks them.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Summary {
    #[field(get, vis = "pub(crate)", copy)]
    metrics: DeckSkin,
    panel: Rgba,
    source: Rgba,
    source_role: TextRoleSkin,
    style: DeckSummaryStyle,
    title: Rgba,
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
    /// The box a summary asks for: a weighted share of its row, and the height
    /// the skin fixes. Said once here because both hosts settle it — the
    /// retained one while it is still walking the document, before it holds a
    /// painter at all.
    pub(crate) const fn declared_length(metrics: DeckSkin) -> Size<Length> {
        Size::new(
            Length::FillPortion(metrics.summary_fill),
            Length::Fixed(metrics.summary_height),
        )
    }

    pub(crate) fn new(style: DeckSummaryStyle, skin: &Skin) -> Self {
        let metrics = skin.deck;
        let compact = style == DeckSummaryStyle::Micro;
        let role = |font: crate::skin::FontSkin, family, color| TextRoleSkin {
            color,
            font: family,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        };
        Self {
            metrics,
            panel: skin.palette.bg_panel,
            source: if compact {
                skin.palette.muted
            } else {
                skin.palette.text_dim
            },
            source_role: role(
                if compact {
                    metrics.micro_source
                } else {
                    metrics.artist
                },
                FontFamily::Sans,
                if compact {
                    ColorRole::Muted
                } else {
                    ColorRole::TextDim
                },
            ),
            style,
            title: skin.palette.text,
            title_role: role(
                if compact {
                    metrics.micro_title
                } else {
                    metrics.title
                },
                FontFamily::Display,
                ColorRole::Text,
            ),
        }
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
        let mut content = list.child();
        if self.style == DeckSummaryStyle::Micro {
            content.text(
                &title,
                &data.title,
                Transform::translate(Pt {
                    x: inner.x,
                    y: center_y(inner, &title),
                }),
                self.title,
            );
            content.text(
                &source,
                &data.source,
                Transform::translate(Pt {
                    x: inner.x + title.width() + self.metrics.micro_summary_gap,
                    y: center_y(inner, &source),
                }),
                self.source,
            );
        } else {
            let stacked = title.height() + self.metrics.readout_gap + source.height();
            let y = inner.y + (inner.h - stacked) / 2.0;
            content.text(
                &title,
                &data.title,
                Transform::translate(Pt { x: inner.x, y }),
                self.title,
            );
            content.text(
                &source,
                &data.source,
                Transform::translate(Pt {
                    x: inner.x,
                    y: y + title.height() + self.metrics.readout_gap,
                }),
                self.source,
            );
        }
        list.clip(inner, content.finish());
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DeckSummaryStyle, DrawListBuilder, Loaded, Rect, Summary, TextContext};
    use crate::{builtin, draw::DrawList};

    const BOUNDS: Rect = Rect {
        h: 44.0,
        w: 200.0,
        x: 2.0,
        y: 4.0,
    };

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
            BOUNDS,
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
