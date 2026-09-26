use std::borrow::Cow;

use kithara_platform::sync;
use vello::{
    Glyph, Scene,
    kurbo::{
        Affine, Arc, BezPath, Cap, Circle, Join, Line, Rect as KurboRect, RoundedRect, Shape,
        Stroke as KurboStroke, Vec2,
    },
    peniko::{
        Blob, Brush, Color, ColorStop, Fill, FontData, Gradient, ImageAlphaType, ImageBrush,
        ImageData, ImageFormat,
    },
};

use crate::{
    draw::{
        Backend, Caps, DrawCmd, DrawList, FillRule, Geom, Image, LineCap, LineJoin, Paint, Path,
        Pen, Rect, Rgba, Stops, Transform, Verb, replay,
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
            paint_color(color),
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

pub(in crate::backends) fn has_system_text(list: &DrawList) -> bool {
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
                .brush(paint_color(color))
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
        Paint::Solid(color) => Brush::Solid(paint_color(color)),
    }
}

fn ramp(gradient: Gradient, stops: Stops) -> Brush {
    Brush::Gradient(
        gradient.with_stops(
            stops
                .as_slice()
                .iter()
                .map(|stop| ColorStop::from((stop.offset, paint_color(stop.color))))
                .collect::<Vec<ColorStop>>()
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

pub(super) fn stroke(pen: Pen) -> KurboStroke {
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

/// The colour vello paints for a toolkit-neutral one.
#[must_use]
pub const fn paint_color(color: Rgba) -> Color {
    Color::new([color.r, color.g, color.b, color.a])
}
