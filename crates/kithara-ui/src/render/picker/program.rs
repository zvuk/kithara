use iced::{Rectangle, mouse::Cursor};

use super::picker_hits;
use crate::{engine::Target, interact::iced as iced_interact};

pub(super) fn targets<'a>(
    path: &'a str,
    anchor: Rectangle,
    cursor: Cursor,
    open: bool,
    item_count: usize,
    item_height: f32,
) -> Vec<Target<'a>> {
    let mut targets = vec![Target::new(path, iced_interact::hit(anchor, cursor))];
    if !open {
        return targets;
    }
    for region in picker_hits(anchor.into(), item_height, item_count) {
        let bounds = region.area();
        targets.push(Target::item(
            path,
            iced_interact::hit(
                Rectangle {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.w,
                    height: bounds.h,
                },
                cursor,
            ),
            *region.action(),
        ));
    }
    targets
}
