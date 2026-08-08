use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::Skin,
};

/// A horizontal bar filled from the left to show one fraction.
pub(crate) struct Meter {
    background: Rgba,
    border: Rgba,
    border_width: f32,
    fill: Rgba,
}

impl Meter {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.meter;
        Self {
            background: skin.rgba(metrics.background),
            border: skin.rgba(metrics.frame.border),
            border_width: metrics.frame.border_width,
            fill: skin.rgba(metrics.fill),
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, level: f32, bounds: Rect) {
        list.fill_rect(bounds, self.background);

        // The bar sits inside the hairline, so the track is inset by the whole
        // border width rather than half of it.
        let width = self.border_width;
        let track = Rect {
            h: (bounds.h - width * 2.0).max(0.0),
            w: (bounds.w - width * 2.0).max(0.0),
            x: bounds.x + width,
            y: bounds.y + width,
        };
        list.fill_rect(
            Rect {
                w: track.w * level.clamp(0.0, 1.0),
                ..track
            },
            self.fill,
        );

        if width <= 0.0 {
            return;
        }
        let inset = width / 2.0;
        list.stroke_rounded_rect(
            Rect {
                h: (bounds.h - width).max(0.0),
                w: (bounds.w - width).max(0.0),
                x: bounds.x + inset,
                y: bounds.y + inset,
            },
            0.0,
            self.border,
            width,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Meter, Rect};
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
    };

    /// The bar is a straight proportion of the track, and the track is what is
    /// left inside the frame — so a full meter stops at the hairline, not on it.
    #[kithara::test]
    fn the_bar_is_the_fraction_of_the_track_inside_the_frame() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 10.0,
            w: 100.0,
            x: 4.0,
            y: 6.0,
        };
        let meter = Meter::new(skin);
        let draw = |level| {
            let mut list = DrawListBuilder::default();
            meter.paint(&mut list, level, bounds);
            list.finish()
        };
        let bar = |level| {
            let list = draw(level);
            let [
                _,
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                },
                ..,
            ] = list.commands()
            else {
                panic!("a meter must draw its background, then its bar");
            };
            *rect
        };

        let inset = skin.meter.frame.border_width;
        let full = bar(1.0);
        assert_eq!(full.w, bounds.w - inset * 2.0);
        assert_eq!(full.x, bounds.x + inset);
        assert_eq!(full.h, bounds.h - inset * 2.0);
        assert_eq!(bar(0.5).w, full.w / 2.0);
    }

    /// An empty meter still draws its bar, so the two hosts never disagree
    /// about how many commands a meter is.
    #[kithara::test]
    fn an_empty_meter_draws_the_same_commands_as_a_full_one() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 10.0,
            w: 100.0,
            x: 0.0,
            y: 0.0,
        };
        let meter = Meter::new(skin);
        let draw = |level| {
            let mut list = DrawListBuilder::default();
            meter.paint(&mut list, level, bounds);
            list.finish()
        };

        assert_eq!(draw(0.0).commands().len(), draw(1.0).commands().len());
        assert_ne!(draw(0.0), draw(1.0));
    }
}
