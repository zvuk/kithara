use kithara_test_utils::kithara;

use super::*;
use crate::layout::Axis;

#[derive(Clone, Copy, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
struct TestItem {
    declared: Size<Length>,
    intrinsic: Size,
    #[field(with, option_set_some, vis = "")]
    main_minimum: Option<f32>,
    #[field(with, option_set_some, vis = "")]
    main_weight: Option<f32>,
}

impl TestItem {
    const fn fixed(width: f32, height: f32) -> Self {
        Self {
            declared: Size::new(Length::Fixed(width), Length::Fixed(height)),
            intrinsic: Size::new(width, height),
            main_minimum: None,
            main_weight: None,
        }
    }

    const fn new(declared: Size<Length>, intrinsic: Size) -> Self {
        Self {
            declared,
            intrinsic,
            main_minimum: None,
            main_weight: None,
        }
    }

    const fn fill(weight: u16) -> Self {
        Self::new(
            Size::new(Length::FillPortion(weight), Length::Fixed(5.0)),
            Size::new(0.0, 5.0),
        )
    }

    const fn vertical_fill(weight: u16) -> Self {
        Self::new(
            Size::new(Length::Fixed(5.0), Length::FillPortion(weight)),
            Size::new(5.0, 0.0),
        )
    }
}

struct TestMeasure<'a> {
    items: &'a [TestItem],
    calls: Vec<(usize, Limits)>,
}

impl Measure for TestMeasure<'_> {
    fn measure(&mut self, index: usize, limits: &Limits) -> Size {
        self.calls.push((index, *limits));
        let item = self.items[index];
        limits.resolve(item.declared.width, item.declared.height, item.intrinsic)
    }
}

struct Case {
    axis: Axis,
    max: Size,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    items: Vec<TestItem>,
}

impl Case {
    fn horizontal(max: Size, width: Length, height: Length, items: Vec<TestItem>) -> Self {
        Self {
            axis: Axis::Horizontal,
            max,
            width,
            height,
            padding: Padding::default(),
            spacing: 0.0,
            align_items: Alignment::Start,
            items,
        }
    }

    fn vertical(max: Size, width: Length, height: Length, items: Vec<TestItem>) -> Self {
        Self {
            axis: Axis::Vertical,
            max,
            width,
            height,
            padding: Padding::default(),
            spacing: 0.0,
            align_items: Alignment::Start,
            items,
        }
    }

    fn run(&self) -> (Distribution, Vec<(usize, Limits)>) {
        let declared = self
            .items
            .iter()
            .map(|item| {
                item.main_weight.map_or_else(
                    || Item::new(item.declared, item.main_minimum),
                    |weight| Item::weighted(item.declared, weight),
                )
            })
            .collect::<Vec<_>>();
        let limits = Limits::new(Size::ZERO, self.max);
        let mut measure = TestMeasure {
            items: &self.items,
            calls: Vec::new(),
        };
        let distribution = resolve(
            Input {
                axis: self.axis,
                limits: &limits,
                width: self.width,
                height: self.height,
                padding: self.padding,
                spacing: self.spacing,
                align_items: self.align_items,
                items: declared,
            },
            &mut measure,
        );
        (distribution, measure.calls)
    }
}

fn placements(distribution: &Distribution) -> Vec<(Point, Size)> {
    distribution
        .items
        .iter()
        .map(|item| (item.offset, item.size))
        .collect()
}

const fn item(offset: Point, size: Size) -> (Point, Size) {
    (offset, size)
}

#[kithara::test]
fn loosening_limits_drops_only_the_minimum() {
    let limits = Limits::with_compression(
        Size::new(10.0, 20.0),
        Size::new(30.0, 40.0),
        Size::new(true, false),
    )
    .loose();

    assert_eq!(limits.min(), Size::ZERO);
    assert_eq!(limits.max(), Size::new(30.0, 40.0));
    assert_eq!(limits.compression(), Size::new(true, false));
}

#[kithara::test]
fn shrinking_limits_uses_both_padding_sides_and_preserves_compression() {
    let limits = Limits::with_compression(
        Size::new(10.0, 5.0),
        Size::new(50.0, 20.0),
        Size::new(true, false),
    )
    .shrink(Padding {
        top: 1.0,
        right: 7.0,
        bottom: 2.0,
        left: 5.0,
    });

    assert_eq!(limits.min(), Size::new(0.0, 2.0));
    assert_eq!(limits.max(), Size::new(38.0, 17.0));
    assert_eq!(limits.compression(), Size::new(true, false));
}

#[kithara::test]
fn resolving_limits_distinguishes_fill_fixed_and_compressed_fill() {
    let limits = Limits::with_compression(
        Size::new(10.0, 20.0),
        Size::new(100.0, 200.0),
        Size::new(false, true),
    );

    assert_eq!(
        limits.resolve(Length::Fill, Length::FillPortion(3), Size::new(30.0, 40.0)),
        Size::new(100.0, 40.0)
    );
    assert_eq!(
        limits.resolve(
            Length::Fixed(5.0),
            Length::Fixed(250.0),
            Size::new(30.0, 40.0),
        ),
        Size::new(10.0, 200.0)
    );
    assert_eq!(
        limits.resolve(Length::Shrink, Length::Shrink, Size::new(150.0, 5.0)),
        Size::new(100.0, 20.0)
    );
}

#[kithara::test]
fn length_constraints_set_and_clear_compression_when_pinning_an_axis() {
    let limits = Limits::new(Size::new(10.0, 20.0), Size::new(100.0, 200.0))
        .width(Length::Shrink)
        .height(Length::Shrink);

    assert_eq!(limits.compression(), Size::new(true, true));

    let limits = limits
        .width(Length::Fixed(150.0))
        .height(Length::Fixed(5.0));

    assert_eq!(limits.min(), Size::new(100.0, 20.0));
    assert_eq!(limits.max(), Size::new(100.0, 20.0));
    assert_eq!(limits.compression(), Size::new(false, false));
}

#[kithara::test]
fn all_fixed_items_use_intrinsic_main_extents_and_largest_cross_extent() {
    let mut case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Shrink,
        Length::Shrink,
        vec![TestItem::fixed(20.0, 10.0), TestItem::fixed(30.0, 15.0)],
    );
    case.align_items = Alignment::Center;

    let (distribution, _) = case.run();

    assert_eq!(distribution.size, Size::new(50.0, 15.0));
    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 2.5), Size::new(20.0, 10.0)),
            item(Point::new(20.0, 0.0), Size::new(30.0, 15.0)),
        ]
    );
}

#[kithara::test]
fn fixed_and_fill_items_split_the_available_main_extent() {
    let mut case = Case::horizontal(
        Size::new(100.0, 30.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fixed(20.0, 10.0),
            TestItem::new(
                Size::new(Length::Fill, Length::Fixed(15.0)),
                Size::new(1.0, 15.0),
            ),
        ],
    );
    case.align_items = Alignment::Center;

    let (distribution, _) = case.run();

    assert_eq!(distribution.size, Size::new(100.0, 15.0));
    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 2.5), Size::new(20.0, 10.0)),
            item(Point::new(20.0, 0.0), Size::new(80.0, 15.0)),
        ]
    );
}

#[kithara::test]
fn fills_share_a_fractional_remainder_without_sequential_rounding() {
    let fill = TestItem::new(
        Size::new(Length::Fill, Length::Fixed(1.0)),
        Size::new(0.0, 1.0),
    );
    let case = Case::horizontal(
        Size::new(10.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![fill, fill, fill],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(10.0 / 3.0, 1.0)),
            item(Point::new(10.0 / 3.0, 0.0), Size::new(10.0 / 3.0, 1.0)),
            item(
                Point::new(10.0 / 3.0 + 10.0 / 3.0, 0.0),
                Size::new(10.0 / 3.0, 1.0),
            ),
        ]
    );
}

#[kithara::test]
fn zero_children_resolve_to_zero_when_both_axes_shrink() {
    let case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Shrink,
        Length::Shrink,
        Vec::new(),
    );

    let (distribution, calls) = case.run();

    assert_eq!(distribution, Distribution::default());
    assert!(calls.is_empty());
}

#[kithara::test]
fn one_child_ignores_spacing() {
    let mut case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Shrink,
        Length::Shrink,
        vec![TestItem::fixed(7.0, 9.0)],
    );
    case.spacing = 40.0;

    let (distribution, _) = case.run();

    assert_eq!(distribution.size, Size::new(7.0, 9.0));
    assert_eq!(
        placements(&distribution),
        vec![item(Point::ORIGIN, Size::new(7.0, 9.0))]
    );
}

#[kithara::test]
fn spacing_accumulates_between_children_only() {
    let mut case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Shrink,
        Length::Shrink,
        vec![
            TestItem::fixed(10.0, 5.0),
            TestItem::fixed(20.0, 5.0),
            TestItem::fixed(30.0, 5.0),
        ],
    );
    case.spacing = 3.0;

    let (distribution, _) = case.run();

    assert_eq!(distribution.size, Size::new(66.0, 5.0));
    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(10.0, 5.0)),
            item(Point::new(13.0, 0.0), Size::new(20.0, 5.0)),
            item(Point::new(36.0, 0.0), Size::new(30.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn spacing_is_reserved_before_fill_items_share_the_remainder() {
    let fill = TestItem::new(
        Size::new(Length::Fill, Length::Fixed(5.0)),
        Size::new(0.0, 5.0),
    );
    let mut case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Fill,
        Length::Shrink,
        vec![TestItem::fixed(20.0, 5.0), fill, fill],
    );
    case.spacing = 10.0;

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(20.0, 5.0)),
            item(Point::new(30.0, 0.0), Size::new(30.0, 5.0)),
            item(Point::new(70.0, 0.0), Size::new(30.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fill_portions_weight_the_remaining_extent() {
    let case = Case::horizontal(
        Size::new(120.0, 50.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fixed(20.0, 5.0),
            TestItem::new(
                Size::new(Length::FillPortion(1), Length::Fixed(5.0)),
                Size::new(0.0, 5.0),
            ),
            TestItem::new(
                Size::new(Length::FillPortion(3), Length::Fixed(5.0)),
                Size::new(0.0, 5.0),
            ),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(20.0, 5.0)),
            item(Point::new(20.0, 0.0), Size::new(25.0, 5.0)),
            item(Point::new(45.0, 0.0), Size::new(75.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn finite_f32_weights_are_distributed_without_overflow() {
    let fill = TestItem::new(
        Size::new(Length::Fill, Length::Fixed(5.0)),
        Size::new(0.0, 5.0),
    )
    .with_main_weight(f32::MAX);
    let case = Case::horizontal(
        Size::new(100.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![fill, fill],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::ORIGIN, Size::new(50.0, 5.0)),
            item(Point::new(50.0, 0.0), Size::new(50.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fixed_main_extent_ignores_an_explicit_weight() {
    let case = Case::horizontal(
        Size::new(100.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fixed(20.0, 5.0).with_main_weight(f32::MAX),
            TestItem::fill(1),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::ORIGIN, Size::new(20.0, 5.0)),
            item(Point::new(20.0, 0.0), Size::new(80.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fluid_minimum_pins_item_and_reshares_remaining_extent() {
    let case = Case::horizontal(
        Size::new(100.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(40.0),
            TestItem::fill(1),
            TestItem::fill(2),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(40.0, 5.0)),
            item(Point::new(40.0, 0.0), Size::new(20.0, 5.0)),
            item(Point::new(60.0, 0.0), Size::new(40.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fluid_minimums_below_weighted_shares_keep_proportions() {
    let case = Case::horizontal(
        Size::new(100.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(10.0),
            TestItem::fill(3).with_main_minimum(40.0),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(25.0, 5.0)),
            item(Point::new(25.0, 0.0), Size::new(75.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fluid_minimums_cascade_when_an_earlier_pin_reduces_later_shares() {
    let case = Case::horizontal(
        Size::new(100.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(30.0),
            TestItem::fill(1).with_main_minimum(24.0),
            TestItem::fill(2),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(30.0, 5.0)),
            item(Point::new(30.0, 0.0), Size::new(24.0, 5.0)),
            item(Point::new(54.0, 0.0), Size::new(46.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn infeasible_fluid_minimums_scale_by_one_common_factor() {
    let case = Case::horizontal(
        Size::new(60.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(80.0),
            TestItem::fill(3).with_main_minimum(40.0),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(40.0, 5.0)),
            item(Point::new(40.0, 0.0), Size::new(20.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn infeasible_negative_fluid_minimum_does_not_create_negative_extent() {
    let case = Case::horizontal(
        Size::new(10.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(-10.0),
            TestItem::fill(1).with_main_minimum(30.0),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(0.0, 5.0)),
            item(Point::new(0.0, 0.0), Size::new(10.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn zero_fluid_minimum_reshares_with_constrained_items() {
    let case = Case::horizontal(
        Size::new(90.0, 10.0),
        Length::Fill,
        Length::Shrink,
        vec![
            TestItem::fill(1).with_main_minimum(0.0),
            TestItem::fill(1).with_main_minimum(50.0),
            TestItem::fill(1).with_main_minimum(10.0),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(20.0, 5.0)),
            item(Point::new(20.0, 0.0), Size::new(50.0, 5.0)),
            item(Point::new(70.0, 0.0), Size::new(20.0, 5.0)),
        ]
    );
}

#[kithara::test]
fn fluid_minimums_equal_to_remaining_extent_are_satisfied_exactly() {
    let case = Case::vertical(
        Size::new(10.0, 100.0),
        Length::Shrink,
        Length::Fill,
        vec![
            TestItem::vertical_fill(1).with_main_minimum(30.0),
            TestItem::vertical_fill(1).with_main_minimum(70.0),
        ],
    );

    let (distribution, _) = case.run();

    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(5.0, 30.0)),
            item(Point::new(0.0, 30.0), Size::new(5.0, 70.0)),
        ]
    );
}

#[kithara::test]
fn padding_shrinks_vertical_measurement_limits_and_offsets_the_child() {
    let mut case = Case {
        axis: Axis::Vertical,
        max: Size::new(40.0, 100.0),
        width: Length::Fill,
        height: Length::Fill,
        padding: Padding {
            top: 10.0,
            right: 7.0,
            bottom: 20.0,
            left: 3.0,
        },
        spacing: 0.0,
        align_items: Alignment::Start,
        items: vec![TestItem::new(
            Size::new(Length::Fill, Length::Fill),
            Size::ZERO,
        )],
    };
    case.align_items = Alignment::Center;

    let (distribution, calls) = case.run();

    assert_eq!(distribution.size, Size::new(40.0, 100.0));
    assert_eq!(
        placements(&distribution),
        vec![item(Point::new(3.0, 10.0), Size::new(30.0, 70.0))]
    );
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.max(), Size::new(30.0, 70.0));
}

#[kithara::test]
fn cross_fill_with_fixed_main_is_measured_after_cross_compression() {
    let case = Case::horizontal(
        Size::new(100.0, 50.0),
        Length::Shrink,
        Length::Shrink,
        vec![
            TestItem::new(
                Size::new(Length::Fixed(20.0), Length::Fill),
                Size::new(20.0, 1.0),
            ),
            TestItem::fixed(10.0, 12.0),
        ],
    );

    let (distribution, calls) = case.run();

    assert_eq!(distribution.size, Size::new(30.0, 12.0));
    assert_eq!(
        placements(&distribution),
        vec![
            item(Point::new(0.0, 0.0), Size::new(20.0, 12.0)),
            item(Point::new(20.0, 0.0), Size::new(10.0, 12.0)),
        ]
    );
    assert_eq!(
        calls.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        vec![1, 0]
    );
    assert_eq!(calls[1].1.compression(), Size::new(false, false));
}
