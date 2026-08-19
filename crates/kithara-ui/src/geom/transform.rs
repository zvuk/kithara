use super::point::Pt;

/// A toolkit-neutral affine transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub xx: f32,
    pub xy: f32,
    pub yx: f32,
    pub yy: f32,
    pub dx: f32,
    pub dy: f32,
}

impl Transform {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        xx: 1.0,
        xy: 0.0,
        yx: 0.0,
        yy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    /// Creates a translation transform.
    #[must_use]
    pub const fn translate(offset: Pt) -> Self {
        Self {
            dx: offset.x,
            dy: offset.y,
            ..Self::IDENTITY
        }
    }

    /// Creates a scale about the origin.
    #[must_use]
    pub const fn scale(factor: Pt) -> Self {
        Self {
            xx: factor.x,
            yy: factor.y,
            ..Self::IDENTITY
        }
    }

    /// Creates a rotation about the origin, in radians.
    ///
    /// Positive turns clockwise, because y grows downward on a screen.
    #[must_use]
    pub fn rotate(radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self {
            xx: cos,
            xy: -sin,
            yx: sin,
            yy: cos,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Applies this transform, then `next`.
    ///
    /// Written in the order a reader follows a point through, rather than the
    /// order a matrix product is written in: `a.then(b)` is `b * a`.
    #[must_use]
    pub const fn then(self, next: Self) -> Self {
        Self {
            xx: next.xx * self.xx + next.xy * self.yx,
            xy: next.xx * self.xy + next.xy * self.yy,
            yx: next.yx * self.xx + next.yy * self.yx,
            yy: next.yx * self.xy + next.yy * self.yy,
            dx: next.xx * self.dx + next.xy * self.dy + next.dx,
            dy: next.yx * self.dx + next.yy * self.dy + next.dy,
        }
    }

    /// Sends a point through this transform.
    #[must_use]
    pub const fn apply(self, point: Pt) -> Pt {
        Pt {
            x: self.xx * point.x + self.xy * point.y + self.dx,
            y: self.yx * point.x + self.yy * point.y + self.dy,
        }
    }

    /// Whether this moves nothing, so a caller can skip the work entirely.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self == Self::IDENTITY
    }

    /// Whether this keeps the axes where they are, so an axis-aligned
    /// rectangle stays one.
    #[cfg(any(feature = "render", feature = "vello"))]
    pub(crate) fn is_axis_aligned(self) -> bool {
        self.xy == 0.0 && self.yx == 0.0
    }

    /// The single factor this scales lengths by, when it scales them equally
    /// in every direction.
    ///
    /// A circle stays a circle and a corner radius stays one radius only under
    /// such a transform; anything else has to become an outline.
    #[cfg(any(feature = "render", feature = "vello"))]
    pub(crate) fn similarity(self) -> Option<f32> {
        let scale = self.xx.hypot(self.yx);
        // A similarity is a rotation times one scale, so the two columns are
        // the same length and at right angles. Comparing against the scale
        // rather than against zero keeps the test meaningful for a large one.
        let square = (self.xy.hypot(self.yy) - scale).abs() <= scale * Self::SQUARE
            && self.xx.mul_add(self.xy, self.yx * self.yy).abs() <= scale * scale * Self::SQUARE;
        (square && scale > 0.0).then_some(scale)
    }

    /// How much this scales a length: the square root of how much it scales an
    /// area.
    ///
    /// Equal to the single factor of a similarity, and the honest stand-in for
    /// a pen width where the two axes disagree — a stroke has one width and
    /// nowhere to put a second.
    #[cfg(any(feature = "render", feature = "vello"))]
    pub(crate) fn length_scale(self) -> f32 {
        self.xx.mul_add(self.yy, -(self.xy * self.yx)).abs().sqrt()
    }

    /// How far from square a transform's axes may sit and still be treated as
    /// a similarity. Loose enough to survive `sin_cos` on a degree that has no
    /// exact float, tight enough that a deliberate squash is never mistaken
    /// for one.
    #[cfg(any(feature = "render", feature = "vello"))]
    const SQUARE: f32 = 1.0e-4;
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::Transform;
    use crate::geom::Pt;

    #[kithara::test]
    fn composing_reads_in_the_order_a_point_travels() {
        let turn = Transform::rotate(core::f32::consts::FRAC_PI_2);
        let shift = Transform::translate(Pt { x: 10.0, y: 0.0 });

        let landed = turn.then(shift).apply(Pt { x: 1.0, y: 0.0 });

        assert_eq!(
            Pt {
                x: landed.x.round(),
                y: landed.y.round(),
            },
            Pt { x: 10.0, y: 1.0 }
        );
    }

    /// The other order is a different transform, which is the whole reason the
    /// order is written down rather than left to a matrix convention.
    #[kithara::test]
    fn the_other_order_lands_somewhere_else() {
        let turn = Transform::rotate(core::f32::consts::FRAC_PI_2);
        let shift = Transform::translate(Pt { x: 10.0, y: 0.0 });

        let landed = shift.then(turn).apply(Pt { x: 1.0, y: 0.0 });

        assert_eq!(
            Pt {
                x: landed.x.round(),
                y: landed.y.round(),
            },
            Pt { x: 0.0, y: 11.0 }
        );
    }

    #[kithara::test]
    fn the_identity_is_what_default_gives() {
        assert!(Transform::default().is_identity());
    }
}
