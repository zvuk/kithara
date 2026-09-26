use super::pool::{Buffer, VecGuard};
use crate::geom::{Pt, Rect};

/// One move a vector outline is made of, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
pub struct PoolPath {
    verbs: Buffer<Verb>,
    rule: FillRule,
}

impl PoolPath {
    #[must_use]
    pub fn new(rule: FillRule, verbs: Vec<Verb>) -> Self {
        Self {
            rule,
            verbs: Buffer::owned(verbs),
        }
    }

    pub(super) fn extend(&mut self, verbs: impl IntoIterator<Item = Verb>) {
        for verb in verbs {
            self.verbs.push(verb);
        }
    }

    pub(super) fn into_pooled(mut self, acquire: impl FnOnce() -> VecGuard<Verb>) -> Self {
        self.verbs = self.verbs.into_pooled(acquire);
        self
    }

    pub(super) fn pooled(rule: FillRule, guard: VecGuard<Verb>) -> Self {
        Self {
            rule,
            verbs: Buffer::pooled(guard),
        }
    }

    #[must_use]
    pub const fn rule(&self) -> FillRule {
        self.rule
    }

    #[must_use]
    pub fn verbs(&self) -> &[Verb] {
        self.verbs.as_slice()
    }
}

pub type Path = PoolPath;

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
        self.placed_with(&crate::draw::DrawListBuilder::default(), into)
    }

    /// The same outline placed through the allocation owner of `list`.
    #[must_use]
    pub fn placed_with(&self, list: &crate::draw::DrawListBuilder, into: Rect) -> Path {
        let place = |point: Pt| Pt {
            x: into.x + point.x * into.w,
            y: into.y + point.y * into.h,
        };
        list.path(
            self.0.rule,
            self.0.verbs().iter().map(|verb| match *verb {
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
            }),
        )
    }
}
