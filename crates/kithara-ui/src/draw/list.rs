use super::{
    DrawCmd, DrawPools, FillRule, Geom, Image, Paint, Path, Pen, PoolText, Pt, Rect, Rgba,
    Transform, Verb, buffer::Buffer, place,
};
use crate::shaping::GlyphRun;

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
    pools: Option<DrawPools>,
    transform: Transform,
}

impl DrawListBuilder {
    pub(super) fn pooled(pools: &DrawPools) -> Self {
        Self {
            commands: pools.commands(),
            pools: Some(pools.clone()),
            transform: Transform::IDENTITY,
        }
    }

    /// Starts a nested list with the same allocation owner as this one.
    ///
    /// The nested list inherits the transform in force, because a clip's
    /// contents move with whatever moved the clip.
    #[must_use]
    pub fn child(&self) -> Self {
        let mut child = self
            .pools
            .as_ref()
            .map_or_else(Self::default, DrawPools::list);
        child.transform = self.transform;
        child
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

    /// The geometry a command carries once the transform in force is resolved
    /// into it.
    ///
    /// A named shape is kept while it stays nameable: a rectangle while the
    /// axes stay put, a corner radius and a circle while lengths scale
    /// equally. Anything else becomes an outline, which is the one shape that
    /// can hold the result.
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
                // An arc names the direction it sweeps in, so a turn or a
                // mirror would have to rewrite its angles. Only the transform
                // that leaves those angles alone keeps it an arc.
                Some(scale) if upright => Geom::Arc {
                    center: by.apply(center),
                    radius: radius * scale,
                    start,
                    end,
                },
                _ => self.outline(
                    FillRule::NonZero,
                    place::arc_verbs(center, radius, start, end, by),
                ),
            },
            Geom::Path(path) => self.outline(path.rule(), place::path_verbs(path.verbs(), by)),
        }
    }

    fn outline(&self, rule: FillRule, verbs: Vec<Verb>) -> Geom {
        Geom::Path(self.path(rule, verbs))
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
                from: by.apply(from),
                stops,
                to: by.apply(to),
            },
            Paint::Radial {
                center,
                radius,
                stops,
            } => Paint::Radial {
                center: by.apply(center),
                radius: radius * by.length_scale(),
                stops,
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

    pub fn stroke_circle<P: Into<Pen>>(&mut self, center: Pt, radius: f32, color: Rgba, pen: P) {
        let geom = self.placed(Geom::Circle { center, radius });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
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

    pub fn stroke_line<P: Into<Pen>>(&mut self, from: Pt, to: Pt, color: Rgba, pen: P) {
        let geom = self.placed(Geom::Line { from, to });
        let pen = self.inked(pen);
        self.commands.push(DrawCmd::Stroke { geom, color, pen });
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

    pub fn fill_path<P: Into<Paint>>(&mut self, path: Path, paint: P) {
        let path = match &self.pools {
            Some(pools) => pools.pooled_path(path),
            None => path,
        };
        let geom = self.placed(Geom::Path(path));
        let paint = self.painted(paint);
        self.commands.push(DrawCmd::Fill { geom, paint });
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

    pub fn text(&mut self, run: &GlyphRun, content: &str, transform: Transform, color: Rgba) {
        let transform = transform.then(self.transform);
        self.commands.push(DrawCmd::Text {
            run: run.clone(),
            content: self
                .pools
                .as_ref()
                .map_or_else(|| PoolText::from(content), |pools| pools.text(content)),
            transform,
            color,
        });
    }

    /// Finishes the retained list.
    #[must_use]
    pub fn finish(self) -> DrawList {
        DrawList(self.commands)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::draw::{LineCap, LineJoin, Stop, Stops};

    const fn ink() -> Rgba {
        Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        }
    }

    const fn shape() -> Rect {
        Rect {
            h: 10.0,
            w: 20.0,
            x: 4.0,
            y: 8.0,
        }
    }

    fn ramp() -> Paint {
        let stop = |offset| Stop {
            color: ink(),
            offset,
        };
        Paint::Linear {
            from: Pt { x: 0.0, y: 0.0 },
            stops: Stops::new(&[stop(0.0), stop(1.0)])
                .unwrap_or_else(|error| panic!("two stops in order are a ramp: {error}")),
            to: Pt { x: 10.0, y: 0.0 },
        }
    }

    /// Where a ramp starts and ends, once a list has drawn it.
    fn ramp_ends(list: &DrawList) -> (Pt, Pt) {
        match only(list) {
            DrawCmd::Fill {
                paint: Paint::Linear { from, to, .. },
                ..
            } => (*from, *to),
            other => panic!("a ramp was drawn, not {other:?}"),
        }
    }

    fn only(list: &DrawList) -> &DrawCmd {
        match list.commands() {
            [command] => command,
            other => panic!("one command was drawn, not {}", other.len()),
        }
    }

    #[kithara::test]
    fn a_transformed_run_moves_what_it_draws() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::translate(Pt { x: 5.0, y: -3.0 }), |list| {
            list.fill_rect(shape(), ink());
        });

        assert_eq!(
            only(&list.finish()),
            &DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    h: 10.0,
                    w: 20.0,
                    x: 9.0,
                    y: 5.0,
                }),
                paint: ink().into(),
            }
        );
    }

    /// An object inside an object is one matrix by the time a point is
    /// written down, which is what makes a nested scene exact rather than
    /// approximately placed by two toolkits in turn.
    #[kithara::test]
    fn nested_runs_compose_into_one_transform() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::scale(Pt { x: 2.0, y: 2.0 }), |list| {
            list.transformed(Transform::translate(Pt { x: 1.0, y: 1.0 }), |list| {
                list.fill_rect(shape(), ink());
            });
        });

        assert_eq!(
            only(&list.finish()),
            &DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    h: 20.0,
                    w: 40.0,
                    x: 10.0,
                    y: 18.0,
                }),
                paint: ink().into(),
            }
        );
    }

    #[kithara::test]
    fn the_transform_is_put_back_when_the_run_ends() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::translate(Pt { x: 100.0, y: 100.0 }), |_| {});
        list.fill_rect(shape(), ink());

        assert_eq!(
            only(&list.finish()),
            &DrawCmd::Fill {
                geom: Geom::Rect(shape()),
                paint: ink().into(),
            }
        );
    }

    /// A turned rectangle is not a rectangle, and the neutral list says so by
    /// carrying the outline rather than by handing a backend a rectangle and a
    /// matrix it may or may not honour.
    #[kithara::test]
    fn a_turned_rectangle_becomes_an_outline() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::rotate(0.4), |list| {
            list.fill_rect(shape(), ink());
        });

        assert!(matches!(
            only(&list.finish()),
            DrawCmd::Fill {
                geom: Geom::Path(_),
                ..
            }
        ));
    }

    #[kithara::test]
    fn a_squashed_circle_becomes_an_outline() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::scale(Pt { x: 2.0, y: 1.0 }), |list| {
            list.fill_circle(Pt { x: 0.0, y: 0.0 }, 4.0, ink());
        });

        assert!(matches!(
            only(&list.finish()),
            DrawCmd::Fill {
                geom: Geom::Path(_),
                ..
            }
        ));
    }

    #[kithara::test]
    fn an_evenly_scaled_circle_stays_a_circle() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::scale(Pt { x: 3.0, y: 3.0 }), |list| {
            list.fill_circle(Pt { x: 1.0, y: 2.0 }, 4.0, ink());
        });

        assert_eq!(
            only(&list.finish()),
            &DrawCmd::Fill {
                geom: Geom::Circle {
                    center: Pt { x: 3.0, y: 6.0 },
                    radius: 12.0,
                },
                paint: ink().into(),
            }
        );
    }

    #[kithara::test]
    fn a_scaled_stroke_scales_its_pen() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::scale(Pt { x: 2.0, y: 2.0 }), |list| {
            list.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 1.0, y: 0.0 }, ink(), 1.5);
        });
        let drawn = list.finish();

        let DrawCmd::Stroke { pen, .. } = only(&drawn) else {
            panic!("a stroked line is a stroke");
        };
        assert_eq!(pen.width, 3.0);
    }

    #[kithara::test]
    fn a_zero_radius_rounded_fill_is_the_existing_rect_list() {
        let rect = Rect {
            h: 12.0,
            w: 24.0,
            x: 3.0,
            y: 6.0,
        };
        let color = Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        };
        let mut expected = DrawListBuilder::default();
        expected.fill_rect(rect, color);
        let mut rounded = DrawListBuilder::default();
        rounded.fill_rounded_rect(rect, 0.0, color);

        assert_eq!(rounded.finish(), expected.finish());
    }

    /// A ramp's geometry is in the same pixels as the shape it fills, which is
    /// what lets every backend resolve the same colour at the same place. A
    /// transform that moves the shape has to move the ramp with it, or the
    /// shape lands one place and its colours another — which is what a Lottie
    /// artwork drew before this: a two-colour ramp painted flat, because the
    /// ramp stayed at the origin while its rectangle moved 150 across.
    #[kithara::test]
    fn a_ramp_moves_with_the_shape_it_fills() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::translate(Pt { x: 30.0, y: 4.0 }), |list| {
            list.fill_rect(shape(), ramp());
        });

        assert_eq!(
            ramp_ends(&list.finish()),
            (Pt { x: 30.0, y: 4.0 }, Pt { x: 40.0, y: 4.0 })
        );
    }

    /// The list resolves a transform into the points it draws, so a ramp under
    /// no transform is the one the caller wrote.
    #[kithara::test]
    fn a_ramp_under_no_transform_is_left_alone() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(shape(), ramp());

        assert_eq!(
            ramp_ends(&list.finish()),
            (Pt { x: 0.0, y: 0.0 }, Pt { x: 10.0, y: 0.0 })
        );
    }

    #[kithara::test]
    fn rounded_strokes_retain_native_geometry_and_canonicalize_zero() {
        let rect = Rect {
            h: 12.0,
            w: 24.0,
            x: 3.0,
            y: 6.0,
        };
        let color = Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        };
        let mut builder = DrawListBuilder::default();
        builder.stroke_rounded_rect(rect, 4.0, color, 1.5);
        builder.stroke_rounded_rect(rect, 0.0, color, 1.5);

        assert_eq!(
            builder.finish().commands(),
            [
                DrawCmd::Stroke {
                    geom: Geom::RoundedRect { rect, radius: 4.0 },
                    color,
                    pen: Pen::new(1.5),
                },
                DrawCmd::Stroke {
                    geom: Geom::Rect(rect),
                    color,
                    pen: Pen::new(1.5),
                },
            ]
        );
    }

    /// A pen the caller shaped travels to the command untouched.
    #[kithara::test]
    fn a_shaped_pen_reaches_the_command_it_was_given_to() {
        let color = Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        };
        let pen = Pen::new(3.0)
            .with_cap(LineCap::Round)
            .with_join(LineJoin::Round);
        let mut builder = DrawListBuilder::default();
        builder.stroke_line(Pt { x: 0.0, y: 0.0 }, Pt { x: 8.0, y: 0.0 }, color, pen);

        assert_eq!(
            builder.finish().commands(),
            [DrawCmd::Stroke {
                geom: Geom::Line {
                    from: Pt { x: 0.0, y: 0.0 },
                    to: Pt { x: 8.0, y: 0.0 },
                },
                color,
                pen,
            }]
        );
    }

    #[kithara::test]
    fn a_clip_retains_its_region_and_nested_list() {
        let region = Rect {
            h: 20.0,
            w: 40.0,
            x: 3.0,
            y: 6.0,
        };
        let color = Rgba {
            a: 1.0,
            b: 0.25,
            g: 0.5,
            r: 0.75,
        };
        let mut nested = DrawListBuilder::default();
        nested.fill_rect(
            Rect {
                h: 40.0,
                w: 80.0,
                x: -10.0,
                y: -20.0,
            },
            color,
        );
        let nested = nested.finish();
        let mut builder = DrawListBuilder::default();

        builder.clip(region, nested.clone());

        assert_eq!(
            builder.finish().commands(),
            [DrawCmd::Clip {
                region,
                list: nested,
            }]
        );
    }
}
