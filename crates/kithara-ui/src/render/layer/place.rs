use crate::{
    draw::{Pt, Rect},
    module::PopoverAlign,
    solve,
};

const FRAME_OVERHANG: f32 = 1.0;

pub(crate) fn place_popover(
    anchor: Rect,
    pointer: Option<Pt>,
    surface: solve::Size,
    viewport: solve::Size,
    align: PopoverAlign,
) -> Pt {
    let from = pointer.map_or(anchor, |point| Rect {
        x: point.x,
        y: point.y,
        w: 0.0,
        h: 0.0,
    });
    let below = from.y + from.h;
    let y = if surface.height <= viewport.height - below {
        below
    } else {
        from.y - surface.height
    };
    let x = match align {
        PopoverAlign::Start => from.x - FRAME_OVERHANG,
        PopoverAlign::End => from.x + from.w + FRAME_OVERHANG - surface.width,
    };
    Pt {
        x: inside(x, surface.width, viewport.width),
        y: inside(y, surface.height, viewport.height),
    }
}

fn inside(start: f32, length: f32, extent: f32) -> f32 {
    start.clamp(0.0, (extent - length).max(0.0))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn placement_flips_aligns_and_clamps_for_both_hosts() {
        let viewport = solve::Size::new(200.0, 140.0);
        assert_eq!(
            place_popover(
                Rect {
                    x: 40.0,
                    y: 30.0,
                    w: 50.0,
                    h: 20.0,
                },
                None,
                solve::Size::new(100.0, 60.0),
                viewport,
                PopoverAlign::Start,
            ),
            Pt { x: 39.0, y: 50.0 },
        );
        assert_eq!(
            place_popover(
                Rect {
                    x: 180.0,
                    y: 120.0,
                    w: 15.0,
                    h: 15.0,
                },
                None,
                solve::Size::new(100.0, 60.0),
                viewport,
                PopoverAlign::End,
            ),
            Pt { x: 96.0, y: 60.0 },
        );
    }
}
