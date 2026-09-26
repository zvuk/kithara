use crate::{
    draw::{DrawListBuilder, FillRule, Pt, Rect, Rgba, Verb},
    render::Skin,
};

/// The collapse chevron at the right end of a module header, together with the
/// line that cuts its cell off from the rest of the header.
///
/// The cell is measured from the right edge of whatever box this is given, so
/// a host that hands it the whole header and a host that hands it just the
/// cell draw the same mark in the same place.
#[derive(Clone, PartialEq, kithara_derive::ControlPainter)]
#[control_painter(
    data = bool,
    draw = self.paint(list, bounds, *data)
)]
#[derive(kithara_derive::Retained)]
pub(crate) struct ChromeChevron {
    color: Rgba,
    line_color: Rgba,
    cell_width: f32,
    icon_size: f32,
    line_width: f32,
    stroke_width: f32,
}

impl ChromeChevron {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.chrome;
        Self {
            cell_width: metrics.chevron_size,
            color: skin.rgba(metrics.chevron_color),
            icon_size: metrics.chevron_icon_size,
            line_color: skin.rgba(metrics.inner_line),
            line_width: metrics.inner_line_width,
            stroke_width: metrics.chevron_stroke_width,
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, bounds: Rect, collapsed: bool) {
        let cell_x = bounds.x + (bounds.w - self.cell_width).max(0.0);
        list.fill_rect(
            Rect {
                h: bounds.h,
                w: self.line_width,
                x: cell_x,
                y: bounds.y,
            },
            self.line_color,
        );
        let center = Pt {
            x: cell_x + self.cell_width / 2.0,
            y: bounds.y + bounds.h / 2.0,
        };
        let half = self.icon_size / 2.0;
        let rise = self.icon_size / 4.0;
        let direction = if collapsed { 1.0 } else { -1.0 };
        let path = list.path(
            FillRule::NonZero,
            [
                Verb::MoveTo(Pt {
                    x: center.x - half,
                    y: center.y - rise * direction,
                }),
                Verb::LineTo(Pt {
                    x: center.x,
                    y: center.y + rise * direction,
                }),
                Verb::LineTo(Pt {
                    x: center.x + half,
                    y: center.y - rise * direction,
                }),
            ],
        );
        list.stroke_path(path, self.color, self.stroke_width);
    }
}

/// The chevron takes the cell the header gives it and marks the middle of it.

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
        shaping::TextContext,
    };

    mod consts {
        use super::*;

        /// A header the whole width of a module, and the cell the chevron sits in
        /// at the end of it.
        pub(super) const HEADER: Rect = Rect {
            h: 26.0,
            w: 200.0,
            x: 12.0,
            y: 4.0,
        };
    }

    fn drawn(paint: impl FnOnce(&mut DrawListBuilder, &mut TextContext)) -> Vec<DrawCmd> {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        paint(&mut list, &mut text);
        list.finish().commands().to_vec()
    }

    /// What the chevron draws in `bounds`.
    fn chevron(bounds: Rect, collapsed: bool) -> Vec<DrawCmd> {
        drawn(|list, _| ChromeChevron::new(builtin::skin()).paint(list, bounds, collapsed))
    }

    /// The chevron's own cell, at the end of a header.
    fn cell(header: Rect) -> Rect {
        let width = builtin::skin().chrome.chevron_size;
        Rect {
            h: header.h,
            w: width,
            x: header.x + header.w - width,
            y: header.y,
        }
    }

    /// One host hands the chevron the whole header and one hands it just the
    /// cell at the end of that header. Both must put the same mark in the same
    /// place, or the two hosts draw a different module shell.
    #[kithara::test]
    fn the_chevron_marks_the_same_place_from_the_header_and_from_its_cell() {
        assert_eq!(
            chevron(consts::HEADER, false),
            chevron(cell(consts::HEADER), false)
        );
    }

    #[kithara::test]
    fn the_chevron_points_down_when_the_module_is_folded() {
        let commands = chevron(consts::HEADER, true);

        assert!(
            elbow(&commands) > ends(&commands),
            "a folded module's chevron must dip below its ends: {commands:?}"
        );
    }

    #[kithara::test]
    fn the_chevron_points_up_when_the_module_is_open() {
        let commands = chevron(consts::HEADER, false);

        assert!(
            elbow(&commands) < ends(&commands),
            "an open module's chevron must rise above its ends: {commands:?}"
        );
    }

    #[kithara::test]
    fn the_chevron_cuts_its_cell_off_at_the_line_the_skin_gives_it() {
        let commands = chevron(consts::HEADER, false);

        let Some(DrawCmd::Fill {
            geom: Geom::Rect(line),
            ..
        }) = commands.first()
        else {
            panic!("the chevron must cut its cell off first: {commands:?}");
        };
        assert_eq!(line.x, cell(consts::HEADER).x);
    }

    /// Where the chevron's elbow sits, and where the two ends it joins sit.
    fn marks(commands: &[DrawCmd]) -> Vec<Pt> {
        let Some(DrawCmd::Stroke {
            geom: Geom::Path(path),
            ..
        }) = commands.last()
        else {
            panic!("the chevron must stroke a mark: {commands:?}");
        };
        path.verbs()
            .iter()
            .map(|verb| match verb {
                Verb::MoveTo(point) | Verb::LineTo(point) => *point,
                other => panic!("a chevron is two straight strokes, not {other:?}"),
            })
            .collect()
    }

    fn elbow(commands: &[DrawCmd]) -> f32 {
        marks(commands)[1].y
    }

    fn ends(commands: &[DrawCmd]) -> f32 {
        marks(commands)[0].y
    }
}
