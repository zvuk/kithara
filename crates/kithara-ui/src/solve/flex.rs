use num_traits::cast::AsPrimitive;

use super::{Alignment, Length, Limits, Padding, Point, Size, fluid};
use crate::layout::Axis;

pub(crate) struct Input<'a> {
    pub(crate) axis: Axis,
    pub(crate) limits: &'a Limits,
    pub(crate) width: Length,
    pub(crate) height: Length,
    pub(crate) padding: Padding,
    pub(crate) spacing: f32,
    pub(crate) align_items: Alignment,
    pub(crate) items: Vec<Item>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Item {
    pub(crate) declared: Size<Length>,
    pub(super) main_minimum: Option<f32>,
    main_weight: Option<f32>,
    pub(crate) offset: Point,
    pub(crate) size: Size,
}

impl Item {
    pub(crate) const fn new(declared: Size<Length>, main_minimum: Option<f32>) -> Self {
        Self {
            declared,
            main_minimum,
            main_weight: None,
            offset: Point::ORIGIN,
            size: Size::ZERO,
        }
    }

    pub(crate) const fn weighted(declared: Size<Length>, main_weight: f32) -> Self {
        Self {
            declared,
            main_minimum: None,
            main_weight: Some(main_weight),
            offset: Point::ORIGIN,
            size: Size::ZERO,
        }
    }

    pub(super) fn main_weight(self, axis: Axis) -> f32 {
        let fill_factor = match axis {
            Axis::Horizontal => self.declared.width.fill_factor(),
            Axis::Vertical => self.declared.height.fill_factor(),
        };
        if fill_factor == 0 {
            0.0
        } else {
            self.main_weight.unwrap_or_else(|| f32::from(fill_factor))
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Distribution {
    pub(crate) size: Size,
    pub(crate) items: Vec<Item>,
}

pub(crate) trait Measure {
    fn measure(&mut self, index: usize, limits: &Limits) -> Size;
}

pub(crate) fn resolve(input: Input<'_>, measure: &mut impl Measure) -> Distribution {
    let Input {
        axis,
        limits,
        width,
        height,
        padding,
        spacing,
        align_items,
        mut items,
    } = input;
    let limits = limits.width(width).height(height).shrink(padding);
    let gap_count: f32 = items.len().saturating_sub(1).as_();
    let total_spacing = spacing * gap_count;
    let max_cross = cross(axis, limits.max());
    let compression = limits.compression();
    let (main_compress, cross_compress) = pack(axis, compression.width, compression.height);
    let (compress_x, compress_y) = pack(axis, main_compress, false);
    let child_compression = Size::new(compress_x, compress_y);
    let mut some_fill_cross = false;
    let mut cross_extent = if cross_compress { 0.0 } else { max_cross };
    let mut available = main(axis, limits.max()) - total_spacing;
    for (index, item) in items.iter_mut().enumerate() {
        let declared = item.declared;
        let fill_main_weight = item.main_weight(axis);
        let fill_cross_factor = cross_fill_factor(axis, declared);

        if (main_compress || fill_main_weight == 0.0) && (!cross_compress || fill_cross_factor == 0)
        {
            let (max_width, max_height) = pack(
                axis,
                available,
                if fill_cross_factor == 0 {
                    max_cross
                } else {
                    cross_extent
                },
            );
            let child_limits = Limits::with_compression(
                Size::ZERO,
                Size::new(max_width, max_height),
                child_compression,
            );
            let size = measure.measure(index, &child_limits);

            available -= main(axis, size);
            cross_extent = cross_extent.max(cross(axis, size));
            item.size = size;
        } else {
            some_fill_cross = some_fill_cross || fill_cross_factor != 0;
        }
    }

    if cross_compress && some_fill_cross {
        for (index, item) in items.iter_mut().enumerate() {
            let declared = item.declared;
            let (main_size, cross_size) = pack(axis, declared.width, declared.height);

            if (main_compress || item.main_weight(axis) == 0.0) && cross_size.fill_factor() != 0 {
                if let Length::Fixed(main_extent) = main_size {
                    available -= main_extent;
                    continue;
                }

                let (max_width, max_height) = pack(axis, available, cross_extent);
                let child_limits = Limits::with_compression(
                    Size::ZERO,
                    Size::new(max_width, max_height),
                    child_compression,
                );
                let size = measure.measure(index, &child_limits);

                available -= main(axis, size);
                cross_extent = cross_extent.max(cross(axis, size));
                item.size = size;
            }
        }
    }

    let remaining = available.max(0.0);

    if !main_compress {
        let allocated_main = fluid::allocate(&items, axis, remaining);
        for (index, (item, allocation)) in items.iter_mut().zip(allocated_main).enumerate() {
            let declared = item.declared;
            let fill_main_weight = item.main_weight(axis);
            let fill_cross_factor = cross_fill_factor(axis, declared);

            if fill_main_weight != 0.0 {
                let max_main = if allocation.extent().is_nan() {
                    f32::INFINITY
                } else {
                    allocation.extent()
                };
                let min_main = if max_main.is_infinite() {
                    0.0
                } else {
                    max_main
                };
                let (min_width, min_height) = pack(axis, min_main, 0.0);
                let (max_width, max_height) = pack(
                    axis,
                    max_main,
                    if fill_cross_factor == 0 {
                        max_cross
                    } else {
                        cross_extent
                    },
                );
                let child_limits = Limits::with_compression(
                    Size::new(min_width, min_height),
                    Size::new(max_width, max_height),
                    child_compression,
                );
                let size = measure.measure(index, &child_limits);

                cross_extent = cross_extent.max(cross(axis, size));
                item.size = size;
            }
        }
    }

    if cross_compress && some_fill_cross {
        for (index, item) in items.iter_mut().enumerate() {
            let declared = item.declared;
            let (main_size, cross_size) = pack(axis, declared.width, declared.height);

            if cross_size.fill_factor() == 0 {
                continue;
            }

            let Length::Fixed(main_extent) = main_size else {
                continue;
            };
            let (max_width, max_height) = pack(axis, main_extent, cross_extent);
            let child_limits = Limits::new(Size::ZERO, Size::new(max_width, max_height));
            let size = measure.measure(index, &child_limits);

            cross_extent = cross_extent.max(cross(axis, size));
            item.size = size;
        }
    }

    let pad = pack(axis, padding.left, padding.top);
    let mut main_offset = pad.0;

    for (index, item) in items.iter_mut().enumerate() {
        if index > 0 {
            main_offset += spacing;
        }

        let (x, y) = pack(axis, main_offset, pad.1);
        item.offset = Point::new(x, y);
        align(item, axis, align_items, cross_extent);
        main_offset += main(axis, item.size);
    }

    let (intrinsic_width, intrinsic_height) = pack(axis, main_offset - pad.0, cross_extent);
    let size = limits
        .resolve(width, height, Size::new(intrinsic_width, intrinsic_height))
        .expand(padding);

    Distribution { size, items }
}

fn cross_fill_factor(axis: Axis, size: Size<Length>) -> u16 {
    match axis {
        Axis::Horizontal => size.height.fill_factor(),
        Axis::Vertical => size.width.fill_factor(),
    }
}

fn main(axis: Axis, size: Size) -> f32 {
    match axis {
        Axis::Horizontal => size.width,
        Axis::Vertical => size.height,
    }
}

fn cross(axis: Axis, size: Size) -> f32 {
    match axis {
        Axis::Horizontal => size.height,
        Axis::Vertical => size.width,
    }
}

fn pack<T>(axis: Axis, main: T, cross: T) -> (T, T) {
    match axis {
        Axis::Horizontal => (main, cross),
        Axis::Vertical => (cross, main),
    }
}

fn align(item: &mut Item, axis: Axis, alignment: Alignment, cross_extent: f32) {
    let free = cross_extent - cross(axis, item.size);
    let offset = match alignment {
        Alignment::Start => 0.0,
        Alignment::Center => free / 2.0,
        Alignment::End => free,
    };

    match axis {
        Axis::Horizontal => item.offset.y += offset,
        Axis::Vertical => item.offset.x += offset,
    }
}
