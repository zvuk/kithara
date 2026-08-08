use super::ir::{Pt, Rect};

/// One move a vector outline is made of, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Verb {
    /// Closes the current subpath back to where it started.
    Close,
    /// A cubic curve through two control points.
    CurveTo {
        first: Pt,
        second: Pt,
        to: Pt,
    },
    LineTo(Pt),
    /// Starts a new subpath.
    MoveTo(Pt),
    /// A quadratic curve through one control point.
    QuadTo {
        control: Pt,
        to: Pt,
    },
}

/// How the inside of an outline that crosses itself is decided.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum FillRule {
    /// Inside where the winding number is not zero.
    #[default]
    NonZero,
    /// Inside where a ray crosses the outline an odd number of times, which is
    /// how a shape punches a hole in itself.
    EvenOdd,
}

/// A vector outline: the moves that draw it, and the rule that fills it.
///
/// The named shapes cover what a control's own skin asks for. This covers what
/// a control brings with it — an authored icon, a component's radial cell —
/// which no fixed set of shapes can express, and which would otherwise have to
/// leave the draw list and reach a toolkit directly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    rule: FillRule,
    verbs: Vec<Verb>,
}

impl Path {
    #[must_use]
    pub fn new(rule: FillRule, verbs: Vec<Verb>) -> Self {
        Self { rule, verbs }
    }

    #[must_use]
    pub const fn rule(&self) -> FillRule {
        self.rule
    }

    #[must_use]
    pub fn verbs(&self) -> &[Verb] {
        &self.verbs
    }
}

/// An outline drawn in the unit square, waiting to be told how big it is.
///
/// Authored art arrives at whatever size its author drew it, while a control
/// asks for it at whatever size its skin says. Keeping the art in one square
/// means the two never have to agree.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline(Path);

impl Outline {
    /// Keeps `path`, whose points must already lie in `0..=1` on both axes.
    #[must_use]
    pub const fn new(path: Path) -> Self {
        Self(path)
    }

    /// The same outline drawn to fill `into`.
    #[must_use]
    pub fn placed(&self, into: Rect) -> Path {
        let place = |point: Pt| Pt {
            x: into.x + point.x * into.w,
            y: into.y + point.y * into.h,
        };
        Path::new(
            self.0.rule,
            self.0
                .verbs
                .iter()
                .map(|verb| match *verb {
                    Verb::Close => Verb::Close,
                    Verb::CurveTo { first, second, to } => Verb::CurveTo {
                        first: place(first),
                        second: place(second),
                        to: place(to),
                    },
                    Verb::LineTo(to) => Verb::LineTo(place(to)),
                    Verb::MoveTo(to) => Verb::MoveTo(place(to)),
                    Verb::QuadTo { control, to } => Verb::QuadTo {
                        control: place(control),
                        to: place(to),
                    },
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{FillRule, Outline, Path, Pt, Rect, Verb};

    /// The unit square lands exactly on the box it was placed in, so art drawn
    /// corner to corner still touches the corners at any size.
    #[kithara::test]
    fn an_outline_placed_in_a_box_spans_it() {
        let outline = Outline::new(Path::new(
            FillRule::NonZero,
            vec![
                Verb::MoveTo(Pt { x: 0.0, y: 0.0 }),
                Verb::LineTo(Pt { x: 1.0, y: 1.0 }),
                Verb::Close,
            ],
        ));

        let placed = outline.placed(Rect {
            h: 8.0,
            w: 16.0,
            x: 3.0,
            y: 5.0,
        });

        assert_eq!(
            placed.verbs(),
            [
                Verb::MoveTo(Pt { x: 3.0, y: 5.0 }),
                Verb::LineTo(Pt { x: 19.0, y: 13.0 }),
                Verb::Close,
            ]
        );
        assert_eq!(placed.rule(), FillRule::NonZero);
    }
}
