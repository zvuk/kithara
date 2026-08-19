use std::borrow::Cow;

use kithara_platform::sync;
use vello::{
    Glyph, Scene,
    kurbo::{
        Affine, Arc, BezPath, Cap, Circle, Join, Line, Point, Rect as KurboRect, RoundedRect,
        Shape, Stroke as KurboStroke, Vec2,
    },
    peniko::{
        Blob, Brush, Color, ColorStop, Fill, FontData, Gradient, ImageAlphaType, ImageBrush,
        ImageData, ImageFormat,
    },
};

use crate::{
    draw::{
        Backend, Caps, DrawCmd, DrawList, FillRule, Geom, Image, LineCap, LineJoin, Paint, Path,
        Pen, Pt, Rect, Rgba, Stops, Transform, Verb, replay,
    },
    shaping::{GlyphFace, GlyphRun},
};

/// A backend that encodes drawing commands into a Vello [`Scene`].
pub struct VelloBackend<'scene> {
    scene: &'scene mut Scene,
}

impl<'scene> VelloBackend<'scene> {
    /// Creates a backend for `scene`.
    pub const fn new(scene: &'scene mut Scene) -> Self {
        Self { scene }
    }

    fn fill_shape(&mut self, shape: &impl Shape, paint: Paint) {
        self.fill_with(Fill::NonZero, shape, paint);
    }

    fn fill_with(&mut self, rule: Fill, shape: &impl Shape, paint: Paint) {
        self.scene
            .fill(rule, Affine::IDENTITY, &brush(paint), None, shape);
    }

    fn stroke_shape(&mut self, shape: &impl Shape, color: Rgba, pen: Pen) {
        self.scene.stroke(
            &stroke(pen),
            Affine::IDENTITY,
            Color::from(color),
            None,
            shape,
        );
    }
}

/// The picture's own pixels, shared rather than copied: `Blob` keeps the
/// allocation this points at, and Vello keys its texture cache on that
/// identity, so redrawing the same picture re-uses the upload.
fn image_data(image: &Image) -> Option<ImageData> {
    Some(ImageData {
        alpha_type: ImageAlphaType::Alpha,
        // `Arc` here is kurbo's geometric arc, so the shared pointer is
        // named through its module.
        data: Blob::new(sync::Arc::new(image.rgba()?.clone())),
        format: ImageFormat::Rgba8,
        height: image.height(),
        width: image.width(),
    })
}

/// Where the picture lands: scaled from its natural size to the box, then
/// turned about that box's centre.
fn placed(image: &Image, rect: Rect, turn: f32) -> Affine {
    let centre = (
        f64::from(rect.x + rect.w / 2.0),
        f64::from(rect.y + rect.h / 2.0),
    );
    Affine::translate((f64::from(rect.x), f64::from(rect.y)))
        .pre_scale_non_uniform(
            f64::from(rect.w) / f64::from(image.width()),
            f64::from(rect.h) / f64::from(image.height()),
        )
        .then_translate(Vec2::new(-centre.0, -centre.1))
        .then_rotate(f64::from(turn))
        .then_translate(Vec2::new(centre.0, centre.1))
}

pub(super) fn has_system_text(list: &DrawList) -> bool {
    list.commands().iter().any(|command| match command {
        DrawCmd::Clip { list, .. } => has_system_text(list),
        DrawCmd::Text { run, .. } => run
            .segments()
            .iter()
            .any(|segment| matches!(segment.face(), GlyphFace::System(_))),
        DrawCmd::Fill { .. } | DrawCmd::Image { .. } | DrawCmd::Stroke { .. } => false,
    })
}

impl Backend for VelloBackend<'_> {
    const CAPS: Caps = Caps::EVERYTHING;

    fn clip(&mut self, region: Rect, list: &DrawList) {
        if has_system_text(list) {
            tracing::warn!(
                issue = "vello#1198",
                ?region,
                "system-backed text inside a Vello clip may paint incorrectly"
            );
        }
        self.scene.push_clip_layer(
            Affine::IDENTITY,
            &KurboRect::new(
                f64::from(region.x),
                f64::from(region.y),
                f64::from(region.x + region.w),
                f64::from(region.y + region.h),
            ),
        );
        replay(list, self);
        self.scene.pop_layer();
    }

    fn fill(&mut self, geom: &Geom, paint: Paint) {
        match geom {
            Geom::Arc {
                center,
                radius,
                start,
                end,
            } => self.fill_shape(
                &Arc::new(
                    *center,
                    Vec2::splat(f64::from(*radius)),
                    f64::from(*start),
                    f64::from(*end - *start),
                    0.0,
                ),
                paint,
            ),
            Geom::Circle { center, radius } => {
                self.fill_shape(&Circle::new(*center, f64::from(*radius)), paint);
            }
            Geom::Line { from, to } => self.fill_shape(&Line::new(*from, *to), paint),
            Geom::Path(outline) => {
                let rule = match outline.rule() {
                    FillRule::EvenOdd => Fill::EvenOdd,
                    FillRule::NonZero => Fill::NonZero,
                };
                self.fill_with(rule, &bez(outline), paint);
            }
            Geom::Rect(rect) => self.fill_shape(
                &KurboRect::new(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.x + rect.w),
                    f64::from(rect.y + rect.h),
                ),
                paint,
            ),
            Geom::RoundedRect { rect, radius } => self.fill_shape(
                &RoundedRect::new(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.x + rect.w),
                    f64::from(rect.y + rect.h),
                    f64::from(*radius),
                ),
                paint,
            ),
        }
    }

    fn image(&mut self, image: &Image, rect: Rect, turn: f32) {
        let Some(data) = image_data(image) else {
            tracing::error!(
                id = image.id().as_str(),
                "a picture rendered on the device reached the plain Vello backend, \
                 which holds no binding for it"
            );
            return;
        };
        self.scene
            .draw_image(&ImageBrush::new(data), placed(image, rect, turn));
    }

    fn stroke(&mut self, geom: &Geom, color: Rgba, pen: Pen) {
        match geom {
            Geom::Arc {
                center,
                radius,
                start,
                end,
            } => self.stroke_shape(
                &Arc::new(
                    *center,
                    Vec2::splat(f64::from(*radius)),
                    f64::from(*start),
                    f64::from(*end - *start),
                    0.0,
                ),
                color,
                pen,
            ),
            Geom::Circle { center, radius } => {
                self.stroke_shape(&Circle::new(*center, f64::from(*radius)), color, pen);
            }
            Geom::Line { from, to } => self.stroke_shape(&Line::new(*from, *to), color, pen),
            Geom::Path(outline) => self.stroke_shape(&bez(outline), color, pen),
            Geom::Rect(rect) => self.stroke_shape(
                &KurboRect::new(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.x + rect.w),
                    f64::from(rect.y + rect.h),
                ),
                color,
                pen,
            ),
            Geom::RoundedRect { rect, radius } => self.stroke_shape(
                &RoundedRect::new(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.x + rect.w),
                    f64::from(rect.y + rect.h),
                    f64::from(*radius),
                ),
                color,
                pen,
            ),
        }
    }

    fn text(&mut self, run: &GlyphRun, _content: &str, transform: Transform, color: Rgba) {
        for segment in run.segments() {
            let data: Cow<'_, FontData> = segment.face().into();
            let glyphs = segment.glyphs().iter().map(|glyph| Glyph {
                id: glyph.id,
                x: glyph.x,
                y: glyph.y,
            });
            self.scene
                .draw_glyphs(data.as_ref())
                .transform(Affine::new([
                    f64::from(transform.xx),
                    f64::from(transform.yx),
                    f64::from(transform.xy),
                    f64::from(transform.yy),
                    f64::from(transform.dx),
                    f64::from(transform.dy),
                ]))
                .font_size(run.size())
                .normalized_coords(segment.normalized_coords())
                .brush(Color::from(color))
                .draw(Fill::NonZero, glyphs);
        }
    }
}

/// One paint as Vello spells it. A ramp's geometry is already in the same pixels
/// as the shape, so the brush needs no transform of its own.
fn brush(paint: Paint) -> Brush {
    match paint {
        Paint::Linear { from, stops, to } => {
            ramp(Gradient::new_linear((from.x, from.y), (to.x, to.y)), stops)
        }
        Paint::Radial {
            center,
            radius,
            stops,
        } => ramp(Gradient::new_radial((center.x, center.y), radius), stops),
        Paint::Solid(color) => Brush::Solid(Color::from(color)),
    }
}

fn ramp(gradient: Gradient, stops: Stops) -> Brush {
    Brush::Gradient(
        gradient.with_stops(
            stops
                .as_slice()
                .iter()
                .map(|stop| ColorStop::from((stop.offset, Color::from(stop.color))))
                .collect::<Vec<_>>()
                .as_slice(),
        ),
    )
}

/// Rebuilds one of our outlines as the curve type Vello draws.
fn bez(outline: &Path) -> BezPath {
    let mut path = BezPath::new();
    for verb in outline.verbs() {
        match *verb {
            Verb::Close => path.close_path(),
            Verb::CurveTo { first, second, to } => path.curve_to(first, second, to),
            Verb::LineTo(to) => path.line_to(to),
            Verb::MoveTo(to) => path.move_to(to),
            Verb::QuadTo { control, to } => path.quad_to(control, to),
        }
    }
    path
}

fn stroke(pen: Pen) -> KurboStroke {
    KurboStroke::new(f64::from(pen.width))
        .with_caps(match pen.cap {
            LineCap::Butt => Cap::Butt,
            LineCap::Round => Cap::Round,
            LineCap::Square => Cap::Square,
        })
        .with_join(match pen.join {
            LineJoin::Bevel => Join::Bevel,
            LineJoin::Miter => Join::Miter,
            LineJoin::Round => Join::Round,
        })
}

impl From<Rgba> for Color {
    fn from(color: Rgba) -> Self {
        Self::new([color.r, color.g, color.b, color.a])
    }
}

impl From<Pt> for Point {
    fn from(point: Pt) -> Self {
        Self::new(f64::from(point.x), f64::from(point.y))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{DrawCmd, DrawListBuilder, Rect},
        shaping::{FontPolicy, GlyphRun, TextContext, TextResources},
        skin::{ColorRole, FontFamily, FontWeight, TextRoleSkin},
    };

    #[kithara::test]
    fn every_draw_operation_adds_to_the_encoding() {
        let run = TextContext::new()
            .unwrap()
            .shape("GAIN", FIXTURE.role, Some(FIXTURE.bounds.w));
        let mut builder = DrawListBuilder::default();
        builder.fill_circle(FIXTURE.point, 5.0, FIXTURE.color);
        builder.stroke_arc(FIXTURE.point, 5.0, 0.0, 1.0, FIXTURE.color, 1.0);
        builder.stroke_circle(FIXTURE.point, 5.0, FIXTURE.color, 1.0);
        builder.stroke_line(FIXTURE.point, Pt { x: 8.0, y: 8.0 }, FIXTURE.color, 1.0);
        builder.fill_rect(FIXTURE.bounds, FIXTURE.color);
        builder.fill_rounded_rect(FIXTURE.bounds, 3.0, FIXTURE.color);
        builder.stroke_rounded_rect(FIXTURE.bounds, 3.0, FIXTURE.color, 1.0);
        builder.text(&run, "GAIN", Transform::IDENTITY, FIXTURE.color);
        let list = builder.finish();
        let mut scene = Scene::new();

        replay(&list, &mut VelloBackend::new(&mut scene));

        assert_eq!(scene.encoding().n_paths, 7);
        assert!(!scene.encoding().resources.glyphs.is_empty());
    }

    #[kithara::test]
    fn clip_replay_balances_the_layer_and_encodes_nested_commands() {
        let mut nested = DrawListBuilder::default();
        nested.fill_rect(FIXTURE.bounds, FIXTURE.color);
        let mut builder = DrawListBuilder::default();
        builder.clip(FIXTURE.bounds, nested.finish());
        let mut scene = Scene::new();

        replay(&builder.finish(), &mut VelloBackend::new(&mut scene));

        assert_eq!(scene.encoding().n_clips, 2);
        assert_eq!(scene.encoding().n_open_clips, 0);
        assert_eq!(scene.encoding().n_paths, 3);
    }

    #[kithara::test]
    fn system_text_detection_covers_direct_and_nested_clips_only() {
        let system = system_run();
        let mut direct = DrawListBuilder::default();
        direct.text(&system, "fallback", Transform::IDENTITY, FIXTURE.color);
        let direct = direct.finish();
        assert!(has_system_text(&direct));

        let mut nested = DrawListBuilder::default();
        nested.clip(FIXTURE.bounds, direct);
        assert!(has_system_text(&nested.finish()));

        let embedded = TextContext::new()
            .unwrap_or_else(|error| panic!("embedded text context must build: {error}"))
            .shape("GAIN", FIXTURE.role, None);
        let mut embedded_list = DrawListBuilder::default();
        embedded_list.text(&embedded, "GAIN", Transform::IDENTITY, FIXTURE.color);
        assert!(!has_system_text(&embedded_list.finish()));
    }

    #[kithara::test]
    fn stroke_width_changes_the_encoding() {
        let thin = line_scene(1.0);
        let thick = line_scene(3.0);

        assert_ne!(thin.encoding().styles, thick.encoding().styles);
    }

    #[kithara::test]
    fn a_plain_pen_cuts_its_ends_flush_and_its_corners_to_a_point() {
        let stroke = stroke(Pen::new(2.0));

        assert_eq!(stroke.start_cap, Cap::Butt);
        assert_eq!(stroke.end_cap, Cap::Butt);
        assert_eq!(stroke.join, Join::Miter);
    }

    /// Every shape a pen can take reaches Vello as the shape it named.
    #[kithara::test]
    fn a_shaped_pen_keeps_its_shape_through_the_backend() {
        for (cap, expected) in [
            (LineCap::Butt, Cap::Butt),
            (LineCap::Round, Cap::Round),
            (LineCap::Square, Cap::Square),
        ] {
            let stroke = stroke(Pen::new(2.0).with_cap(cap));
            assert_eq!(stroke.start_cap, expected, "cap {cap:?}");
            assert_eq!(stroke.end_cap, expected, "cap {cap:?}");
        }
        for (join, expected) in [
            (LineJoin::Bevel, Join::Bevel),
            (LineJoin::Miter, Join::Miter),
            (LineJoin::Round, Join::Round),
        ] {
            assert_eq!(
                stroke(Pen::new(2.0).with_join(join)).join,
                expected,
                "join {join:?}"
            );
        }
    }

    #[kithara::test]
    fn text_content_adds_glyphs() {
        let no_text = text_scene("");
        let text = text_scene("GAIN");

        assert!(no_text.encoding().resources.glyphs.is_empty());
        assert!(!text.encoding().resources.glyphs.is_empty());
    }

    #[kithara::test]
    fn missing_character_does_not_drop_any_positioned_glyphs() {
        let scene = text_scene("A\u{10ffff}B");

        assert_eq!(scene.encoding().resources.glyphs.len(), 3);
    }

    fn line_scene(width: f32) -> Scene {
        let mut builder = DrawListBuilder::default();
        builder.stroke_line(FIXTURE.point, Pt { x: 8.0, y: 8.0 }, FIXTURE.color, width);
        let mut scene = Scene::new();
        replay(&builder.finish(), &mut VelloBackend::new(&mut scene));
        scene
    }

    fn text_scene(content: &str) -> Scene {
        let run = TextContext::new()
            .unwrap()
            .shape(content, FIXTURE.role, Some(FIXTURE.bounds.w));
        let mut builder = DrawListBuilder::default();
        builder.text(
            &run,
            content,
            Transform::translate(Pt {
                x: FIXTURE.bounds.x + (FIXTURE.bounds.w - run.width()) / 2.0,
                y: FIXTURE.bounds.y,
            }),
            FIXTURE.color,
        );
        let mut scene = Scene::new();
        replay(&builder.finish(), &mut VelloBackend::new(&mut scene));
        scene
    }

    fn system_run() -> GlyphRun {
        let resources = TextResources::new(FontPolicy::System)
            .unwrap_or_else(|error| panic!("system text resources must build: {error}"));
        let mut text = TextContext::from(&resources);
        for content in ["曲名", "שלום", "مرحبا", "ಜಗ", "ชื่อ"] {
            let run = text.shape(content, FIXTURE.role, None);
            if run
                .segments()
                .iter()
                .any(|segment| matches!(segment.face(), GlyphFace::System(_)))
            {
                return run;
            }
        }
        panic!("a system face must answer at least one script outside the embedded catalog");
    }

    #[derive(Clone, Copy)]
    struct DrawFixture {
        bounds: Rect,
        color: Rgba,
        point: Pt,
        role: TextRoleSkin,
    }

    const FIXTURE: DrawFixture = {
        let color = Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        };
        DrawFixture {
            bounds: Rect {
                h: 12.0,
                w: 40.0,
                x: 0.0,
                y: 0.0,
            },
            color,
            point: Pt { x: 4.0, y: 4.0 },
            role: TextRoleSkin {
                color: ColorRole::Text,
                font: FontFamily::Sans,
                size: 12.0,
                spacing: 0.0,
                weight: FontWeight::Normal,
            },
        }
    };

    #[cfg(feature = "render")]
    #[kithara::test]
    fn replaying_a_knob_list_encodes_one_path_per_geometry_command() {
        const BOUNDS: Rect = Rect {
            h: 39.0,
            w: 28.0,
            x: 0.0,
            y: 0.0,
        };

        let mut builder = DrawListBuilder::default();
        crate::atoms::knob::Knob::new(crate::builtin::skin()).paint(
            &mut builder,
            &mut TextContext::new().unwrap(),
            0.25,
            Some("GAIN"),
            BOUNDS,
        );
        let list = builder.finish();
        let geometry = list
            .commands()
            .iter()
            .filter(|command| !matches!(command, DrawCmd::Text { .. }))
            .fold(0_u32, |count, _| count + 1);

        let mut scene = Scene::new();
        replay(&list, &mut VelloBackend::new(&mut scene));

        assert_eq!(scene.encoding().n_paths, geometry);
        assert!(!scene.encoding().resources.glyphs.is_empty());
    }
}
