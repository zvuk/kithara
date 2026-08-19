use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::{ScalarRange, Skin},
    skin::RangeSkin,
};

/// A rail with a handle at each end, marking the interval between them.
pub(crate) struct Range {
    metrics: RangeSkin,
    rail: Rgba,
    selection: Rgba,
    thumb: Rgba,
}

impl Range {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.range;
        Self {
            metrics,
            rail: skin.rgba(metrics.rail_background),
            selection: skin.rgba(metrics.selection_color),
            thumb: skin.rgba(metrics.thumb_color),
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, value: ScalarRange, bounds: Rect) {
        let rail = Rect {
            h: self.metrics.rail_height,
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + (bounds.h - self.metrics.rail_height) / 2.0,
        };
        list.fill_rect(rail, self.rail);
        let min_x = bounds.x + value.min.clamp(0.0, 1.0) * bounds.w;
        let max_x = bounds.x + value.max.clamp(0.0, 1.0) * bounds.w;
        list.fill_rect(
            Rect {
                w: (max_x - min_x).max(0.0),
                x: min_x,
                ..rail
            },
            self.selection,
        );
        let y = bounds.y + (bounds.h - self.metrics.thumb_height) / 2.0;
        for x in [min_x, max_x] {
            list.fill_rect(
                Rect {
                    h: self.metrics.thumb_height,
                    w: self.metrics.thumb_width,
                    x: x - self.metrics.thumb_width / 2.0,
                    y,
                },
                self.thumb,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Paint},
    };

    const BOX: Rect = Rect {
        h: 24.0,
        w: 200.0,
        x: 10.0,
        y: 4.0,
    };

    fn painted(value: ScalarRange) -> Vec<DrawCmd> {
        let mut list = DrawListBuilder::default();
        Range::new(builtin::skin()).paint(&mut list, value, BOX);
        list.finish().commands().to_vec()
    }

    /// The selected interval is what the control is for: it spans exactly the
    /// two values, in the box's own coordinates.
    #[kithara::test]
    fn the_selection_spans_the_two_values() {
        let commands = painted(ScalarRange {
            min: 0.25,
            max: 0.75,
        });

        let [
            _,
            DrawCmd::Fill {
                geom: Geom::Rect(selection),
                ..
            },
            ..,
        ] = commands.as_slice()
        else {
            panic!("a range must draw its selection over its rail");
        };
        assert_eq!(selection.x, BOX.x + 50.0);
        assert_eq!(selection.w, 100.0);
    }

    /// An inverted interval draws no selection rather than a rectangle running
    /// backwards, which a fill would paint across the whole rail.
    #[kithara::test]
    fn an_inverted_interval_selects_nothing() {
        let commands = painted(ScalarRange { min: 0.8, max: 0.2 });

        let [
            _,
            DrawCmd::Fill {
                geom: Geom::Rect(selection),
                ..
            },
            ..,
        ] = commands.as_slice()
        else {
            panic!("a range must draw its selection over its rail");
        };
        assert_eq!(selection.w, 0.0);
    }

    /// Both handles are drawn, and neither is the selection: a control showing
    /// one thumb cannot say which end the hand is about to take.
    #[kithara::test]
    fn both_handles_are_drawn_in_the_thumb_colour() {
        let skin = builtin::skin();
        let thumb = skin.rgba(skin.range.thumb_color);
        let commands = painted(ScalarRange {
            min: 0.25,
            max: 0.75,
        });

        let thumbs = commands
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    DrawCmd::Fill {
                        geom: Geom::Rect(rect),
                        paint: Paint::Solid(color),
                    } if *color == thumb && rect.w == skin.range.thumb_width
                )
            })
            .count();

        assert_eq!(thumbs, 2);
    }
}
