use super::Hit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CursorShape {
    None,
    Grab,
    Grabbing,
    Pointer,
    ResizeDiagonalDown,
    ResizeDiagonalUp,
    ResizeH,
    ResizeV,
    Text,
}

#[derive(Clone, Copy)]
pub(crate) struct Hover {
    shape: CursorShape,
}

impl Hover {
    pub(crate) const fn new(shape: CursorShape) -> Self {
        Self { shape }
    }

    pub(crate) fn cursor(self, active: bool, hit: &Hit) -> CursorShape {
        if active || hit.over() {
            self.shape
        } else {
            CursorShape::None
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::draw::{Pt, Rect};

    fn hit(at: Option<Pt>) -> Hit {
        Hit::new(
            at,
            Rect {
                h: 34.0,
                w: 34.0,
                x: 0.0,
                y: 0.0,
            },
        )
    }

    #[kithara::test]
    fn the_cursor_shape_follows_hover_or_an_active_gesture() {
        let hover = Hover::new(CursorShape::ResizeV);

        assert_eq!(
            hover.cursor(false, &hit(Some(Pt { x: 17.0, y: 17.0 }))),
            CursorShape::ResizeV
        );
        assert_eq!(
            hover.cursor(false, &hit(Some(Pt { x: 200.0, y: 200.0 }))),
            CursorShape::None
        );
        assert_eq!(
            hover.cursor(true, &hit(Some(Pt { x: 200.0, y: 200.0 }))),
            CursorShape::ResizeV,
            "an active gesture keeps its shape once the pointer leaves"
        );
    }
}
