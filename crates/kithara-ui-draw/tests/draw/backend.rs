use kithara_test_utils::kithara;
use kithara_ui_draw::{
    Backend, Caps, DrawCmd, DrawList, DrawListBuilder, FillRule, Geom, Image, ImageId, Paint, Path,
    Pen, Pt, Rect, Rgba, Transform, Verb, replay,
};
use kithara_ui_shaping::GlyphRun;

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

    fn image(&mut self, _image: &Image, _rect: Rect, _turn: f32) {
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
    list.image(
        Image::external(ImageId::new("shader/test"), 1, 1),
        unit_box(),
    );
    list.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 1.0, y: 1.0 }, ink(), 1.0);
    let list = list.finish();

    assert!(matches!(
        list.commands(),
        [
            DrawCmd::Fill { .. },
            DrawCmd::Image { .. },
            DrawCmd::Stroke { .. }
        ]
    ));
    let mut recorder = Recorder::default();
    replay(&list, &mut recorder);
    assert_eq!(recorder.drawn, 3);
}
