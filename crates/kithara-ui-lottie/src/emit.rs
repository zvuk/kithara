use std::ops::Range;

use kithara_ui_draw::{
    DrawListBuilder, FillRule, LineCap, LineJoin, Paint, Pen, Pt, Rgba, Stop, Stops, Transform,
    Verb,
};
use num_traits::cast::AsPrimitive;
use velato::{
    Composition,
    model::{Content, Draw, Geometry, GroupTransform, Layer, Shape, fixed},
    vello::{
        kurbo::{Affine, Cap, Join, PathEl, Point},
        peniko::{
            Brush, Gradient, GradientKind,
            color::{AlphaColor, Srgb},
        },
    },
};

use crate::LottieError;

/// Draws one frame of an artwork into a list, in the neutral vocabulary.
///
/// Nothing reaches `into` unless the whole frame is drawable: an artwork that
/// half draws is a picture nobody authored, which is the refusal the capability
/// door already makes for a list a backend cannot draw whole.
///
/// # Errors
/// Returns what the artwork asks for that this vocabulary has no word for.
pub fn emit(
    composition: &Composition,
    frame: f64,
    into: &mut DrawListBuilder,
) -> Result<(), LottieError> {
    let mut painted = Vec::new();
    for layer in composition.layers.iter().rev() {
        if layer.is_mask {
            continue;
        }
        layer_at(
            &composition.layers,
            layer,
            Affine::IDENTITY,
            1.0,
            frame,
            &mut painted,
        )?;
    }
    for part in painted {
        part.draw(into);
    }
    Ok(())
}

/// One thing the emitter decided to draw, held until the whole frame is known
/// to be drawable.
///
/// The contour stays in its own space with its transform beside it, so the list
/// resolves both the points and the pen width the way it does for every other
/// drawing rather than by arithmetic written a second time here.
struct Part {
    ink: Ink,
    transform: Transform,
    verbs: Vec<Verb>,
}

/// What one draw does to the contours it claimed.
#[derive(Clone, Copy)]
enum Ink {
    Fill(Paint),
    Stroke { color: Rgba, pen: Pen },
}

impl Part {
    fn draw(self, into: &mut DrawListBuilder) {
        into.transformed(self.transform, |list| {
            let path = list.path(FillRule::NonZero, self.verbs);
            match self.ink {
                Ink::Fill(paint) => list.fill_path(path, paint),
                Ink::Stroke { color, pen } => list.stroke_path(path, color, pen),
            }
        });
    }
}

fn layer_at(
    layers: &[Layer],
    layer: &Layer,
    parent: Affine,
    alpha: f64,
    frame: f64,
    painted: &mut Vec<Part>,
) -> Result<(), LottieError> {
    if !layer.frames.contains(&frame) {
        return Ok(());
    }
    let name = || layer.name.clone();
    if layer.mask_layer.is_some() {
        return Err(LottieError::Matte { layer: name() });
    }
    if !layer.masks.is_empty() {
        return Err(LottieError::Mask { layer: name() });
    }
    if layer.blend_mode.is_some() {
        return Err(LottieError::Blend { layer: name() });
    }
    let transform = chained(layers, layer, parent, frame);
    let alpha = alpha * layer.opacity.evaluate(frame) / 100.0;
    match &layer.content {
        Content::None => Ok(()),
        Content::Instance { name: asset, .. } => Err(LottieError::Instance {
            layer: name(),
            name: asset.clone(),
        }),
        Content::Shape(shapes) => {
            let mut batch = Batch::default();
            batch.shapes(shapes, transform, alpha, frame, &layer.name)?;
            batch.paint(&layer.name, painted)
        }
    }
}

/// A layer's own transform under every parent's, guarded against a chain that
/// points back at itself — an artwork is not checked for one when it is read.
fn chained(layers: &[Layer], layer: &Layer, parent: Affine, frame: f64) -> Affine {
    let mut transform = layer.transform.evaluate(frame).into_owned();
    let mut next = layer.parent;
    let mut seen = 0_usize;
    while let Some(index) = next {
        let Some(above) = layers.get(index).filter(|_| seen < layers.len()) else {
            break;
        };
        next = above.parent;
        transform = above.transform.evaluate(frame).into_owned() * transform;
        seen += 1;
    }
    parent * transform
}

/// Contours collected top to bottom, and the draws that claim them.
///
/// A draw claims every contour opened since its own group started, and the
/// draws are painted in reverse — so the first fill an artwork writes ends up
/// on top, which is what its author sees in the tool that wrote it.
#[derive(Default)]
struct Batch {
    contours: Vec<Contour>,
    draws: Vec<Claim>,
    elements: Vec<PathEl>,
    /// How many contours the most recent draw claimed, so a run already spoken
    /// for is never merged into.
    claimed: usize,
}

struct Contour {
    transform: Affine,
    elements: Range<usize>,
}

struct Claim {
    brush: fixed::Brush,
    stroke: Option<fixed::Stroke>,
    contours: Range<usize>,
    alpha: f64,
}

impl Batch {
    /// Merges into the run before it only when that run is not yet spoken for
    /// and sits under the same transform, which is what decides how many
    /// contours one draw fills.
    fn contour(&mut self, geometry: &Geometry, transform: Affine, frame: f64) {
        let mergeable = self.claimed < self.contours.len()
            && self.contours.last().map(|last| last.transform) == Some(transform);
        let start = self.elements.len();
        geometry.evaluate(frame, &mut self.elements);
        if let Some(last) = self.contours.last_mut().filter(|_| mergeable) {
            last.elements.end = self.elements.len();
        } else {
            self.contours.push(Contour {
                transform,
                elements: start..self.elements.len(),
            });
        }
    }

    fn draw(&mut self, draw: &Draw, alpha: f64, opened: usize, frame: f64) {
        self.draws.push(Claim {
            stroke: draw
                .stroke
                .as_ref()
                .map(|stroke| stroke.evaluate(frame).into_owned()),
            brush: draw.brush.evaluate(1.0, frame).into_owned(),
            alpha: alpha * draw.opacity.evaluate(frame) / 100.0,
            contours: opened..self.contours.len(),
        });
        self.claimed = self.contours.len();
    }

    fn paint(self, layer: &str, painted: &mut Vec<Part>) -> Result<(), LottieError> {
        for draw in self.draws.iter().rev() {
            let ink = ink(draw, layer)?;
            for contour in self.contours.get(draw.contours.clone()).unwrap_or_default() {
                painted.push(Part {
                    ink,
                    verbs: verbs(
                        self.elements
                            .get(contour.elements.clone())
                            .unwrap_or_default(),
                    ),
                    transform: affine(contour.transform),
                });
            }
        }
        Ok(())
    }

    fn shapes(
        &mut self,
        shapes: &[Shape],
        transform: Affine,
        alpha: f64,
        frame: f64,
        layer: &str,
    ) -> Result<(), LottieError> {
        let opened = self.contours.len();
        for shape in shapes {
            match shape {
                Shape::Group(inner, group) => {
                    let (pose, opacity) = group.as_ref().map_or(
                        (Affine::IDENTITY, 1.0),
                        |GroupTransform { transform, opacity }| {
                            (
                                transform.evaluate(frame).into_owned(),
                                opacity.evaluate(frame) / 100.0,
                            )
                        },
                    );
                    self.shapes(inner, transform * pose, alpha * opacity, frame, layer)?;
                }
                Shape::Geometry(geometry) => self.contour(geometry, transform, frame),
                Shape::Draw(draw) => self.draw(draw, alpha, opened, frame),
                Shape::Repeater(_) | Shape::Trim(_) => {
                    return Err(LottieError::Modifier {
                        layer: layer.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// What one draw puts on the contours it claimed, with its own opacity folded
/// into every colour.
///
/// velato folds alpha into the brush rather than opening a layer for it — it
/// passes a literal one to both of its own — so this list needs no word for a
/// transparency group to draw the same picture.
fn ink(draw: &Claim, layer: &str) -> Result<Ink, LottieError> {
    let Some(stroke) = draw.stroke.as_ref() else {
        return Ok(Ink::Fill(paint(&draw.brush, draw.alpha, layer)?));
    };
    let pen = pen(stroke, layer)?;
    match paint(&draw.brush, draw.alpha, layer)? {
        Paint::Solid(color) => Ok(Ink::Stroke { color, pen }),
        Paint::Linear { .. } | Paint::Radial { .. } => Err(LottieError::RampedStroke {
            layer: layer.to_owned(),
        }),
    }
}

/// A pen for one stroke.
///
/// A dash pattern is not asked about because velato never reads one: the stroke
/// it hands over is `kurbo::Stroke::new` with a cap and a join set on it, so
/// this list's pen having no pattern is not a gap an artwork can fall into.
fn pen(stroke: &fixed::Stroke, layer: &str) -> Result<Pen, LottieError> {
    /// What a miter join carries when an artwork does not say, which is the only
    /// limit this list's pen has a word for.
    const MITER: f64 = 4.0;

    if stroke.miter_limit != MITER {
        return Err(LottieError::MiteredStroke {
            layer: layer.to_owned(),
        });
    }
    Ok(Pen {
        cap: cap(stroke.start_cap),
        join: join(stroke.join),
        width: stroke.width.as_(),
    })
}

const fn cap(cap: Cap) -> LineCap {
    match cap {
        Cap::Butt => LineCap::Butt,
        Cap::Round => LineCap::Round,
        Cap::Square => LineCap::Square,
    }
}

const fn join(join: Join) -> LineJoin {
    match join {
        Join::Bevel => LineJoin::Bevel,
        Join::Miter => LineJoin::Miter,
        Join::Round => LineJoin::Round,
    }
}

fn paint(brush: &Brush, alpha: f64, layer: &str) -> Result<Paint, LottieError> {
    match brush {
        Brush::Solid(color) => Ok(Paint::Solid(faded(*color, alpha))),
        Brush::Image(_) => Err(LottieError::Brush {
            layer: layer.to_owned(),
        }),
        Brush::Gradient(gradient) => ramp(gradient, alpha, layer),
    }
}

fn ramp(gradient: &Gradient, alpha: f64, layer: &str) -> Result<Paint, LottieError> {
    let stops = gradient
        .stops
        .iter()
        .map(|stop| Stop {
            color: faded(stop.color.to_alpha_color::<Srgb>(), alpha),
            offset: stop.offset,
        })
        .collect::<Vec<Stop>>();
    let stops = Stops::new(&stops).map_err(|source| LottieError::Ramp {
        source,
        layer: layer.to_owned(),
    })?;
    match gradient.kind {
        GradientKind::Linear(line) => Ok(Paint::Linear {
            stops,
            from: point(line.start),
            to: point(line.end),
        }),
        GradientKind::Radial(circles)
            if circles.start_radius == 0.0 && circles.start_center == circles.end_center =>
        {
            Ok(Paint::Radial {
                stops,
                center: point(circles.end_center),
                radius: circles.end_radius,
            })
        }
        GradientKind::Radial(_) | GradientKind::Sweep(_) => Err(LottieError::Brush {
            layer: layer.to_owned(),
        }),
    }
}

fn faded(color: AlphaColor<Srgb>, alpha: f64) -> Rgba {
    let [r, g, b, a] = color.components;
    let alpha: f32 = alpha.as_();
    Rgba {
        b,
        g,
        r,
        a: a * alpha,
    }
}

fn verbs(elements: &[PathEl]) -> Vec<Verb> {
    elements
        .iter()
        .map(|element| match *element {
            PathEl::MoveTo(to) => Verb::MoveTo(point(to)),
            PathEl::LineTo(to) => Verb::LineTo(point(to)),
            PathEl::QuadTo(control, to) => Verb::QuadTo {
                control: point(control),
                to: point(to),
            },
            PathEl::CurveTo(first, second, to) => Verb::CurveTo {
                first: point(first),
                second: point(second),
                to: point(to),
            },
            PathEl::ClosePath => Verb::Close,
        })
        .collect()
}

fn affine(affine: Affine) -> Transform {
    let [xx, yx, xy, yy, dx, dy] = affine.as_coeffs();
    Transform {
        xx: xx.as_(),
        xy: xy.as_(),
        yx: yx.as_(),
        yy: yy.as_(),
        dx: dx.as_(),
        dy: dy.as_(),
    }
}

fn point(point: Point) -> Pt {
    Pt {
        x: point.x.as_(),
        y: point.y.as_(),
    }
}
