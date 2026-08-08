use iced::{
    Color, Font, Point, Radians, Rectangle, Size,
    advanced::graphics::{Gradient, gradient},
    font::{Family, Stretch, Style, Weight},
    widget::canvas::{
        Fill, Frame, Path, Stroke as IcedStroke, fill,
        path::{Arc, Builder},
        stroke::{LineCap as IcedCap, LineJoin as IcedJoin},
    },
};
use skrifa::{
    FontRef, GlyphId,
    instance::{LocationRef, NormalizedCoord, Size as FontSize},
    outline::{DrawSettings, OutlineGlyphCollection, OutlinePen},
};

use crate::{
    draw::{
        Backend, Caps, DrawCmd, DrawList, FillRule, Geom, LineCap, LineJoin, Needs, Paint, Pen, Pt,
        Rect, Rgba, Transform, Verb,
    },
    skin::{FontFamily, FontWeight},
    text::{GlyphFace, GlyphRun, GlyphSegment, TextResources, select},
};

pub(crate) struct IcedBackend<'frame> {
    frame: &'frame mut Frame,
    resources: &'frame TextResources,
}

impl<'frame> IcedBackend<'frame> {
    pub(crate) const fn new(frame: &'frame mut Frame, resources: &'frame TextResources) -> Self {
        Self { frame, resources }
    }
}

/// Paints a whole list into a frame, keeping the order the list asked for.
///
/// This is the iced host's door, so it is where a list this backend cannot draw
/// is refused whole, exactly as [`replay`](crate::draw::replay) does for one
/// that walks the list itself.
///
/// `iced_wgpu` pastes a clipped sub-frame's geometry *ahead* of whatever its
/// parent drew directly, so a control that fills its own box and then draws
/// inside a clip would paint over its own clipped content. Nothing is drawn
/// directly here: every run of commands goes through a sub-frame too, and the
/// pastes then land in list order.
pub(crate) fn replay_ordered(list: &DrawList, frame: &mut Frame, resources: &TextResources) {
    if let Err(error) = <IcedBackend<'_> as Backend>::CAPS.accepts(Needs::from(list)) {
        tracing::error!(%error, "the iced backend refused a draw list it cannot draw");
        return;
    }
    let bounds = Rect {
        h: frame.height(),
        w: frame.width(),
        x: 0.0,
        y: 0.0,
    };
    ordered(list, frame, resources, bounds);
}

fn ordered(list: &DrawList, frame: &mut Frame, resources: &TextResources, bounds: Rect) {
    let mut run = Vec::new();
    for command in list.commands() {
        if let DrawCmd::Clip { region, list } = command {
            flush(&mut run, frame, resources, bounds);
            frame.with_clip(region_of(*region), |frame| {
                ordered(list, frame, resources, *region);
            });
        } else {
            run.push(command);
        }
    }
    flush(&mut run, frame, resources, bounds);
}

fn flush(run: &mut Vec<&DrawCmd>, frame: &mut Frame, resources: &TextResources, bounds: Rect) {
    if run.is_empty() {
        return;
    }
    let commands = std::mem::take(run);
    frame.with_clip(region_of(bounds), |frame| {
        let mut backend = IcedBackend::new(frame, resources);
        for command in commands {
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
    });
}

const fn region_of(region: Rect) -> Rectangle {
    Rectangle {
        height: region.h,
        width: region.w,
        x: region.x,
        y: region.y,
    }
}

impl Backend for IcedBackend<'_> {
    /// iced 0.14 has one kind of gradient, and it is linear: `iced_core`'s own
    /// `Gradient` documents the radial one as still to be decided. There is
    /// nothing below it to reach for either, so a list that asks for a radial
    /// ramp is refused rather than approximated with a flat colour.
    const CAPS: Caps = Caps {
        radial_gradient: false,
        ..Caps::EVERYTHING
    };

    fn clip(&mut self, region: Rect, list: &DrawList) {
        ordered(list, self.frame, self.resources, region);
    }

    fn fill(&mut self, geom: &Geom, paint: Paint) {
        if let (Geom::Rect(rect), Paint::Solid(color)) = (geom, paint) {
            self.frame.fill_rectangle(
                Point::new(rect.x, rect.y),
                Size::new(rect.w, rect.h),
                Color::from(color),
            );
        } else if let Some(style) = style(paint) {
            self.frame.fill(
                &path(geom),
                Fill {
                    rule: rule(geom),
                    style,
                },
            );
        } else {
            tracing::error!("a paint iced cannot spell reached it past the door");
        }
    }

    fn stroke(&mut self, geom: &Geom, color: Rgba, pen: Pen) {
        self.frame.stroke(&path(geom), stroke(color, pen));
    }

    fn text(&mut self, run: &GlyphRun, _content: &str, transform: Transform, color: Rgba) {
        let resources = self.resources;
        let path = Path::new(|builder| {
            for segment in run.segments() {
                match segment.face() {
                    GlyphFace::Embedded(font) => draw_segment(
                        builder,
                        segment,
                        resources.outlines(*font),
                        run.size(),
                        transform,
                    ),
                    GlyphFace::System(data) => {
                        let font = match FontRef::from_index(data.data.as_ref(), data.index) {
                            Ok(font) => font,
                            Err(error) => {
                                tracing::warn!(
                                    blob = data.data.id(),
                                    index = data.index,
                                    ?error,
                                    "failed to parse system font face"
                                );
                                continue;
                            }
                        };
                        let outlines = OutlineGlyphCollection::new(&font);
                        draw_segment(builder, segment, &outlines, run.size(), transform);
                    }
                }
            }
        });
        self.frame.fill(&path, Color::from(color));
    }
}

fn draw_segment(
    builder: &mut Builder,
    segment: &GlyphSegment,
    outlines: &OutlineGlyphCollection<'_>,
    size: f32,
    transform: Transform,
) {
    let normalized_coords = segment
        .normalized_coords()
        .iter()
        .map(|coord| NormalizedCoord::from_bits(*coord))
        .collect::<Vec<_>>();
    let location = LocationRef::new(&normalized_coords);
    for glyph in segment.glyphs() {
        let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
            continue;
        };
        let mut pen = IcedOutline {
            builder,
            glyph: Pt {
                x: glyph.x,
                y: glyph.y,
            },
            transform,
        };
        let settings = DrawSettings::unhinted(FontSize::new(size), location);
        if let Err(error) = outline.draw(settings, &mut pen) {
            tracing::warn!(
                face = ?segment.face(),
                glyph_id = glyph.id,
                ?error,
                "failed to draw glyph outline"
            );
        }
    }
}

pub(crate) const fn font(family: FontFamily, weight: FontWeight) -> Font {
    let face = select(family, weight);
    let weight = match weight {
        FontWeight::Normal => Weight::Normal,
        FontWeight::Medium => Weight::Medium,
        FontWeight::Semibold => Weight::Semibold,
        FontWeight::Bold => Weight::Bold,
    };
    Font {
        family: Family::Name(face.family_name()),
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}

/// One paint as iced spells it, or nothing when iced has no way to spell it. A
/// ramp's ends are already in the same pixels as the shape, which is the space
/// an iced linear gradient reads them in.
fn style(paint: Paint) -> Option<fill::Style> {
    match paint {
        Paint::Linear { from, stops, to } => {
            let mut gradient = gradient::Linear::new(from.into(), to.into());
            for stop in stops.as_slice() {
                gradient = gradient.add_stop(stop.offset, Color::from(stop.color));
            }
            Some(fill::Style::Gradient(Gradient::Linear(gradient)))
        }
        Paint::Radial { .. } => None,
        Paint::Solid(color) => Some(fill::Style::Solid(Color::from(color))),
    }
}

/// How a shape decides its own interior. Only an authored outline can ask for
/// anything but the default.
fn rule(geom: &Geom) -> fill::Rule {
    match geom {
        Geom::Path(path) => match path.rule() {
            FillRule::EvenOdd => fill::Rule::EvenOdd,
            FillRule::NonZero => fill::Rule::NonZero,
        },
        Geom::Arc { .. }
        | Geom::Circle { .. }
        | Geom::Line { .. }
        | Geom::Rect(_)
        | Geom::RoundedRect { .. } => fill::Rule::NonZero,
    }
}

fn path(geom: &Geom) -> Path {
    match geom {
        Geom::Arc {
            center,
            radius,
            start,
            end,
        } => Path::new(|builder| {
            builder.arc(Arc {
                radius: *radius,
                center: (*center).into(),
                start_angle: Radians(*start),
                end_angle: Radians(*end),
            });
        }),
        Geom::Circle { center, radius } => Path::circle((*center).into(), *radius),
        Geom::Line { from, to } => Path::line((*from).into(), (*to).into()),
        Geom::Path(outline) => Path::new(|builder| {
            for verb in outline.verbs() {
                match *verb {
                    Verb::Close => builder.close(),
                    Verb::CurveTo { first, second, to } => {
                        builder.bezier_curve_to(first.into(), second.into(), to.into());
                    }
                    Verb::LineTo(to) => builder.line_to(to.into()),
                    Verb::MoveTo(to) => builder.move_to(to.into()),
                    Verb::QuadTo { control, to } => {
                        builder.quadratic_curve_to(control.into(), to.into());
                    }
                }
            }
        }),
        Geom::Rect(rect) => Path::rectangle(Point::new(rect.x, rect.y), Size::new(rect.w, rect.h)),
        Geom::RoundedRect { rect, radius } => Path::rounded_rectangle(
            Point::new(rect.x, rect.y),
            Size::new(rect.w, rect.h),
            (*radius).into(),
        ),
    }
}

struct IcedOutline<'builder> {
    builder: &'builder mut Builder,
    glyph: Pt,
    transform: Transform,
}

impl IcedOutline<'_> {
    fn point(&self, x: f32, y: f32) -> Point {
        self.transform
            .apply(Pt {
                x: self.glyph.x + x,
                y: self.glyph.y - y,
            })
            .into()
    }
}

impl OutlinePen for IcedOutline<'_> {
    fn close(&mut self) {
        self.builder.close();
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder
            .bezier_curve_to(self.point(cx0, cy0), self.point(cx1, cy1), self.point(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.point(x, y));
    }

    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.point(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder
            .quadratic_curve_to(self.point(cx0, cy0), self.point(x, y));
    }
}

impl From<Rgba> for Color {
    fn from(color: Rgba) -> Self {
        Self {
            a: color.a,
            b: color.b,
            g: color.g,
            r: color.r,
        }
    }
}

impl From<Color> for Rgba {
    fn from(color: Color) -> Self {
        Self {
            a: color.a,
            b: color.b,
            g: color.g,
            r: color.r,
        }
    }
}

impl From<Pt> for Point {
    fn from(point: Pt) -> Self {
        Self::new(point.x, point.y)
    }
}

fn stroke(color: Rgba, pen: Pen) -> IcedStroke<'static> {
    IcedStroke::default()
        .with_color(Color::from(color))
        .with_width(pen.width)
        .with_line_cap(match pen.cap {
            LineCap::Butt => IcedCap::Butt,
            LineCap::Round => IcedCap::Round,
            LineCap::Square => IcedCap::Square,
        })
        .with_line_join(match pen.join {
            LineJoin::Bevel => IcedJoin::Bevel,
            LineJoin::Miter => IcedJoin::Miter,
            LineJoin::Round => IcedJoin::Round,
        })
}
