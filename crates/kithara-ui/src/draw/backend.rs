use super::{Caps, DrawCmd, DrawList, Geom, Needs, Paint, Pen, Rect, Rgba, Transform};
use crate::text::GlyphRun;

/// Consumes toolkit-neutral retained drawing commands.
pub trait Backend {
    /// What this backend is able to draw. A list that asks for more is refused
    /// whole — see [`Caps::accepts`] — rather than drawn
    /// with the parts it understands.
    const CAPS: Caps;

    /// Encodes a nested list inside a rectangular clip region.
    fn clip(&mut self, region: Rect, list: &DrawList);

    fn fill(&mut self, geom: &Geom, paint: Paint);

    fn stroke(&mut self, geom: &Geom, color: Rgba, pen: Pen);

    fn text(&mut self, run: &GlyphRun, content: &str, transform: Transform, color: Rgba);
}

/// Replays a retained list into a rendering backend.
///
/// A backend that cannot draw part of the list is given none of it. Painting
/// the rest would put a picture on the screen that nobody described: a clip-less
/// backend handed a clipped document would spill its contents, and one without
/// gradients would flatten a ramp to a colour that appears nowhere in the
/// document. Refusing whole is the only answer that stays true to the list.
pub fn replay<B: Backend>(list: &DrawList, backend: &mut B) {
    if let Err(error) = B::CAPS.accepts(Needs::from(list)) {
        tracing::error!(%error, "a backend refused a draw list it cannot draw");
        return;
    }
    draw(list, backend);
}

fn draw<B: Backend>(list: &DrawList, backend: &mut B) {
    for command in list.commands() {
        match command {
            DrawCmd::Clip { region, list } => backend.clip(*region, list),
            DrawCmd::Fill { geom, paint } => backend.fill(geom, *paint),
            DrawCmd::Stroke { geom, color, pen } => backend.stroke(geom, *color, *pen),
            DrawCmd::Text {
                run,
                content,
                transform,
                color,
            } => backend.text(run, content, *transform, *color),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        Backend, Caps, DrawCmd, DrawList, Geom, Paint, Pen, Rect, Rgba, Transform, replay,
    };
    use crate::{
        draw::{DrawListBuilder, FillRule, Path, Pt, Verb},
        text::GlyphRun,
    };

    /// A backend that records what it was asked to draw and nothing else.
    #[derive(Default)]
    struct Recorder {
        drawn: usize,
    }

    impl Backend for Recorder {
        const CAPS: Caps = Caps {
            outline: false,
            ..Caps::EVERYTHING
        };

        fn clip(&mut self, _region: Rect, list: &DrawList) {
            self.drawn += list.commands().len();
        }

        fn fill(&mut self, _geom: &Geom, _paint: Paint) {
            self.drawn += 1;
        }

        fn stroke(&mut self, _geom: &Geom, _color: Rgba, _pen: Pen) {
            self.drawn += 1;
        }

        fn text(&mut self, _run: &GlyphRun, _content: &str, _transform: Transform, _color: Rgba) {
            self.drawn += 1;
        }
    }

    fn ink() -> Rgba {
        Rgba {
            a: 1.0,
            b: 1.0,
            g: 1.0,
            r: 1.0,
        }
    }

    fn unit_box() -> Rect {
        Rect {
            h: 10.0,
            w: 10.0,
            x: 0.0,
            y: 0.0,
        }
    }

    #[kithara::test]
    fn a_list_a_backend_can_draw_reaches_it_whole() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(unit_box(), ink());
        list.fill_rect(unit_box(), ink());
        let mut recorder = Recorder::default();

        replay(&list.finish(), &mut recorder);

        assert_eq!(recorder.drawn, 2);
    }

    /// One command it cannot draw costs the list the whole frame. Drawing the
    /// rest would put a picture on the screen that the list never described.
    #[kithara::test]
    fn one_command_too_many_costs_the_whole_list() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(unit_box(), ink());
        list.fill_path(
            Path::new(
                FillRule::NonZero,
                vec![Verb::MoveTo(Pt { x: 0.0, y: 0.0 }), Verb::Close],
            ),
            ink(),
        );
        let mut recorder = Recorder::default();

        replay(&list.finish(), &mut recorder);

        assert_eq!(recorder.drawn, 0, "a refused list is not drawn in part");
    }

    /// The refusal reads what is actually drawn, so a need buried in a clip is
    /// still a need.
    #[kithara::test]
    fn a_need_inside_a_clip_is_refused_too() {
        let mut inner = DrawListBuilder::default();
        inner.fill_path(
            Path::new(
                FillRule::NonZero,
                vec![Verb::MoveTo(Pt { x: 0.0, y: 0.0 }), Verb::Close],
            ),
            ink(),
        );
        let mut list = DrawListBuilder::default();
        list.clip(unit_box(), inner.finish());
        let mut recorder = Recorder::default();

        replay(&list.finish(), &mut recorder);

        assert_eq!(recorder.drawn, 0);
    }

    #[kithara::test]
    fn every_command_reaches_the_backend_in_order() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(unit_box(), ink());
        list.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 1.0, y: 1.0 }, ink(), 1.0);
        let list = list.finish();

        assert!(matches!(
            list.commands(),
            [DrawCmd::Fill { .. }, DrawCmd::Stroke { .. }]
        ));
    }
}
