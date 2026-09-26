use kithara_ui_shaping::GlyphRun;

use super::{
    DrawBuffers, DrawCmd, FillRule, Geom, Image, Paint, Path, Pen, PoolText, Pt, Rect, Rgba,
    Transform, Verb, place, pool::Buffer,
};

/// An ordered retained list of drawing commands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList(Buffer<DrawCmd>);

impl DrawList {
    /// Returns the commands in drawing order.
    #[must_use]
    pub fn commands(&self) -> &[DrawCmd] {
        self.0.as_slice()
    }
}

/// Builds a [`DrawList`] in drawing order.
#[derive(Default)]
pub struct DrawListBuilder {
    commands: Buffer<DrawCmd>,
    pools: Option<DrawBuffers>,
    transform: Transform,
}

impl DrawListBuilder {
    /// Starts a nested list with the same allocation owner as this one.
    ///
    /// The nested list inherits the transform in force, because a clip's
    /// contents move with whatever moved the clip.
    #[must_use]
    pub fn child(&self) -> Self {
        let mut child = self
            .pools
            .as_ref()
            .map_or_else(Self::default, DrawBuffers::list);
        child.transform = self.transform;
        child
    }

    /// Adds a nested list scoped to a rectangular clip region.
    ///
    /// A clip region is a rectangle in both toolkits and in the neutral
    /// command, so under a transform that turns it the region becomes the
    /// upright box it fits in — a turned object clips to more than its own
    /// outline, and says so here rather than being discovered.
    pub fn clip(&mut self, region: Rect, list: DrawList) {
        let region = if self.transform.is_identity() {
            region
        } else {
            place::bounds(region, self.transform)
        };
        self.commands.push(DrawCmd::Clip { region, list });
    }

    pub fn fill_circle<P: Into<Paint>>(&mut self, center: Pt, radius: f32, paint: P) {
        let geom = self.placed(Geom::Circle { center, radius });
        let paint = self.painted(paint);
        self.commands.push(DrawCmd::Fill { geom, paint });
    }

    pub fn fill_path<P: Into<Paint>>(&mut self, path: Path, paint: P) {
        let path = match &self.pools {
            Some(pools) => pools.pooled_path(path),
            None => path,
        };
        let geom = self.placed(Geom::Path(path));
        let paint = self.painted(paint);
        self.commands.push(DrawCmd::Fill { geom, paint });
    }

    pub fn fill_rect<P: Into<Paint>>(&mut self, rect: Rect, paint: P) {
        let geom = self.placed(Geom::Rect(rect));
        let paint = self.painted(paint);
        self.commands.push(DrawCmd::Fill { geom, paint });
    }

    pub fn fill_rounded_rect<P: Into<Paint>>(&mut self, rect: Rect, radius: f32, paint: P) {
        let geom = self.placed(if radius == 0.0 {
            Geom::Rect(rect)
        } else {
            Geom::RoundedRect { rect, radius }
        });
        let paint = self.painted(paint);
        self.commands.push(DrawCmd::Fill { geom, paint });
    }

    /// Finishes the retained list.
    #[must_use]
    pub fn finish(self) -> DrawList {
        DrawList(self.commands)
    }

    /// Adds a picture at its destination rectangle.
    ///
    /// Whatever moved, resized or turned the drawing is read off the transform
    /// in force and carried as a box and a turn, which is the pair both
    /// rasterisers take for a picture.
    pub fn image(&mut self, image: Image, rect: Rect) {
        let (rect, turn) = if self.transform.is_identity() {
            (rect, 0.0)
        } else {
            place::turned(rect, self.transform)
        };
        self.commands.push(DrawCmd::Image { image, rect, turn });
    }

    /// The pen a stroke is drawn with once the transform in force is resolved
    /// into its width. A scaled object draws scaled lines; a line that kept
    /// its width would thin out as the object grew.
    fn inked<P: Into<Pen>>(&self, pen: P) -> Pen {
        let mut pen = pen.into();
        if !self.transform.is_identity() {
            pen.width *= self.transform.length_scale();
        }
        pen
    }

    fn outline(&self, rule: FillRule, verbs: Vec<Verb>) -> Geom {
        Geom::Path(self.path(rule, verbs))
    }

    /// The paint a fill is drawn with once the transform in force is resolved
    /// into it.
    ///
    /// A ramp's geometry is in the same pixels as the shape it fills — that is
    /// what lets every backend resolve the same colour at the same place — so a
    /// transform that moves the shape has to move the ramp with it. A radial's
    /// radius scales the way a pen's width does, because there is one number
    /// and nowhere to put a second.
    fn painted<P: Into<Paint>>(&self, paint: P) -> Paint {
        let by = self.transform;
        match paint.into() {
            paint if by.is_identity() => paint,
            Paint::Solid(color) => Paint::Solid(color),
            Paint::Linear { from, stops, to } => Paint::Linear {
                stops,
                from: by.apply(from),
                to: by.apply(to),
            },
            Paint::Radial {
                center,
                radius,
                stops,
            } => Paint::Radial {
                stops,
                center: by.apply(center),
                radius: radius * by.length_scale(),
            },
        }
    }

    /// Builds a path with the same allocation owner as this list.
    #[must_use]
    pub fn path<Verbs>(&self, rule: FillRule, verbs: Verbs) -> Path
    where
        Verbs: IntoIterator<Item = Verb>,
    {
        match &self.pools {
            Some(pools) => pools.path(rule, verbs),
            None => Path::new(rule, verbs.into_iter().collect()),
        }
    }

    /// The geometry a command carries once the transform in force is resolved
    /// into it.
    ///
    /// A named shape is kept while it stays nameable: a rectangle while the
    /// axes stay put, a corner radius and a circle while lengths scale
    /// equally. Anything else becomes an outline, which is the one shape that
    /// can hold the result.
    ///
    /// An arc names the direction it sweeps in, so only a transform that leaves its angles
    /// unchanged can keep it an arc; anything else becomes an outline.
    fn placed(&self, geom: Geom) -> Geom {
        let by = self.transform;
        if by.is_identity() {
            return geom;
        }
        let upright = by.is_axis_aligned() && by.xx > 0.0 && by.yy > 0.0;
        match geom {
            Geom::Line { from, to } => Geom::Line {
                from: by.apply(from),
                to: by.apply(to),
            },
            Geom::Rect(shape) if by.is_axis_aligned() => Geom::Rect(place::bounds(shape, by)),
            Geom::Rect(shape) => self.outline(FillRule::NonZero, place::rect_verbs(shape, by)),
            Geom::RoundedRect { rect, radius } => match by.similarity() {
                Some(scale) if by.is_axis_aligned() => Geom::RoundedRect {
                    rect: place::bounds(rect, by),
                    radius: radius * scale,
                },
                _ => self.outline(FillRule::NonZero, place::rounded_verbs(rect, radius, by)),
            },
            Geom::Circle { center, radius } => by.similarity().map_or_else(
                || self.outline(FillRule::NonZero, place::circle_verbs(center, radius, by)),
                |scale| Geom::Circle {
                    center: by.apply(center),
                    radius: radius * scale,
                },
            ),
            Geom::Arc {
                center,
                radius,
                start,
                end,
            } => match by.similarity() {
                Some(scale) if upright => Geom::Arc {
                    start,
                    end,
                    center: by.apply(center),
                    radius: radius * scale,
                },
                _ => self.outline(
                    FillRule::NonZero,
                    place::arc_verbs(center, radius, start, end, by),
                ),
            },
            Geom::Path(path) => self.outline(path.rule(), place::path_verbs(path.verbs(), by)),
        }
    }

    pub(super) fn pooled(pools: &DrawBuffers) -> Self {
        Self {
            commands: pools.commands(),
            pools: Some(pools.clone()),
            transform: Transform::IDENTITY,
        }
    }

    /// Strokes an arc whose angles are expressed in radians.
    pub fn stroke_arc<P: Into<Pen>>(
        &mut self,
        center: Pt,
        radius: f32,
        start: f32,
        end: f32,
        color: Rgba,
        pen: P,
    ) {
        let geom = self.placed(Geom::Arc {
            center,
            radius,
            start,
            end,
        });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
    }

    pub fn stroke_circle<P: Into<Pen>>(&mut self, center: Pt, radius: f32, color: Rgba, pen: P) {
        let geom = self.placed(Geom::Circle { center, radius });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
    }

    pub fn stroke_line<P: Into<Pen>>(&mut self, from: Pt, to: Pt, color: Rgba, pen: P) {
        let geom = self.placed(Geom::Line { from, to });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
    }

    /// Strokes an outline no named shape covers: a curve open at both ends,
    /// which a fill would close behind the pen.
    pub fn stroke_path<P: Into<Pen>>(&mut self, path: Path, color: Rgba, pen: P) {
        let path = match &self.pools {
            Some(pools) => pools.pooled_path(path),
            None => path,
        };
        let geom = self.placed(Geom::Path(path));
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
    }

    pub fn stroke_rounded_rect<P: Into<Pen>>(
        &mut self,
        rect: Rect,
        radius: f32,
        color: Rgba,
        pen: P,
    ) {
        let geom = self.placed(if radius == 0.0 {
            Geom::Rect(rect)
        } else {
            Geom::RoundedRect { rect, radius }
        });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
    }

    pub fn text(&mut self, run: &GlyphRun, content: &str, transform: Transform, color: Rgba) {
        let transform = transform.then(self.transform);
        self.commands.push(DrawCmd::Text {
            transform,
            color,
            run: run.clone(),
            content: self
                .pools
                .as_ref()
                .map_or_else(|| PoolText::from(content), |pools| pools.text(content)),
        });
    }

    /// Draws under `by`, composed onto whatever transform is already in force.
    ///
    /// The offset is resolved here, into the points each command carries,
    /// rather than handed to a toolkit. Neither host can be trusted with it:
    /// iced's canvas frame exposes similarity transforms only, and the clip it
    /// opens for every command run is a fresh frame whose transform stack
    /// starts at the identity. Resolving it in the neutral list is also what
    /// makes a nested object exact — two transforms compose into one matrix
    /// before a single point is rasterised.
    pub fn transformed<R, Draw>(&mut self, by: Transform, draw: Draw) -> R
    where
        Draw: FnOnce(&mut Self) -> R,
    {
        let outer = self.transform;
        self.transform = by.then(outer);
        let drawn = draw(self);
        self.transform = outer;
        drawn
    }
}
