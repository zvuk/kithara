use std::collections::BTreeMap;

use kithara_test_utils::kithara;

use super::spec::*;
use crate::{
    builtin,
    compile::{CompiledNode, SplitCell},
    expand::{Binding, BindingKind, BlockSpec, ControlSpec, ExpandedNode, MeasureSpec},
    ids::{Interner, SourceUri},
    layout::Axis,
    module::{
        ChromeStyle, GlyphStyle, IconName, MeasureAxis, PopoverAlign, PopoverAt, TextAlign,
        TextStyle,
    },
};

fn control(interner: &mut Interner, id: &str, size: SizeSpec) -> ExpandedNode {
    let origin = SourceUri("size-test.ron".to_owned());
    ExpandedNode::Control {
        path: interner.intern(id, &origin).unwrap(),
        id: interner.intern(id, &origin).unwrap(),
        spec: ControlSpec::Knob { label: None },
        read: None,
        write: None,
        size: Some(size),
    }
}

fn fixed(w: f32, h: f32) -> SizeSpec {
    SizeSpec::new(Dim::Fixed(w), Dim::Fixed(h))
}

fn row(
    children: Vec<ExpandedNode>,
    size: Option<SizeSpec>,
    gap: Option<f32>,
    measure: Option<MeasureAxis>,
) -> ExpandedNode {
    ExpandedNode::Row {
        size,
        gap,
        measure,
        children,
        id: None,
        align: TextAlign::Center,
        pad: None,
        pad_x: None,
        pad_y: None,
        frame: None,
        background: None,
        background_alpha: None,
        active: None,
        active_background: None,
        frame_color: None,
        active_frame_color: None,
        surface: None,
    }
}

fn column(children: Vec<ExpandedNode>, gap: Option<f32>) -> ExpandedNode {
    ExpandedNode::Column {
        gap,
        children,
        id: None,
        size: None,
        measure: None,
        align: TextAlign::Start,
        pad: None,
        pad_x: None,
        pad_y: None,
        frame: None,
        frame_color: None,
        background: None,
        background_alpha: None,
        surface: None,
    }
}

#[kithara::test]
fn full_module_chrome_adds_header_footer_and_internal_lines() {
    let size = fixed(100.0, 40.0);
    let skin = builtin::skin_doc();

    assert_eq!(with_module_chrome(size, ChromeStyle::Frame, skin), size);
    assert_eq!(with_module_chrome(size, ChromeStyle::Plain, skin), size);
    assert_eq!(
        with_module_chrome(size, ChromeStyle::Full, skin),
        fixed(100.0, 90.0)
    );
}

#[kithara::test]
fn range_reports_its_bounds() {
    let dim = Dim::Range {
        min: 10.0,
        max: Some(20.0),
    };

    assert_eq!(dim.min(), 10.0);
    assert_eq!(dim.max(), Some(20.0));
}

#[kithara::test]
fn fill_has_no_intrinsic_bounds() {
    assert_eq!(Dim::Fill.min(), 0.0);
    assert_eq!(Dim::Fill.max(), None);
}

#[kithara::test]
fn shrink_has_no_intrinsic_bounds() {
    assert_eq!(Dim::Shrink.min(), 0.0);
    assert_eq!(Dim::Shrink.max(), None);
}

#[kithara::test]
fn a_pair_of_equal_bounds_is_one_fixed_number() {
    assert_eq!(
        Dim::from(Bounds {
            min: 12.0,
            max: Some(12.0)
        }),
        Dim::Fixed(12.0),
        "a bound that cannot move is a fixed size"
    );
    assert_eq!(
        Dim::from(Bounds {
            min: 12.0,
            max: Some(20.0)
        }),
        Dim::Range {
            min: 12.0,
            max: Some(20.0)
        },
        "a ceiling above the floor is the range between them"
    );
}

#[kithara::test]
fn an_open_bound_from_zero_is_fill() {
    assert_eq!(
        Dim::from(Bounds {
            min: 0.0,
            max: None
        }),
        Dim::Fill,
        "nothing to ask for and no ceiling is what fill means"
    );
    assert_eq!(
        Dim::from(Bounds {
            min: 4.0,
            max: None
        }),
        Dim::Range {
            min: 4.0,
            max: None
        },
        "a floor of its own is a range, however open the ceiling"
    );
}

#[kithara::test]
fn a_dimension_grows_by_the_room_asked_for() {
    assert_eq!(grow(Dim::Fixed(10.0), 4.0), Dim::Fixed(14.0));
    assert_eq!(
        grow(
            Dim::Range {
                min: 10.0,
                max: Some(20.0)
            },
            4.0
        ),
        Dim::Range {
            min: 14.0,
            max: Some(24.0)
        },
        "both ends of a range move by the same room"
    );
    assert_eq!(
        grow(Dim::Fixed(10.0), 0.0),
        Dim::Fixed(10.0),
        "no room asked for leaves the dimension where it was"
    );
}

#[kithara::test]
fn a_declared_size_skips_composition() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![control(&mut interner, "child", fixed(10.0, 10.0))],
        Some(SizeSpec::new(Dim::Shrink, Dim::Fixed(30.0))),
        Some(0.0),
        None,
    );

    assert_eq!(
        compute_size(&node, builtin::skin_doc(), DEFAULTS),
        SizeSpec::new(Dim::Shrink, Dim::Fixed(30.0))
    );
}

#[kithara::test]
fn each_axis_grows_by_its_own_padding() {
    let mut interner = Interner::new(1024);
    let ExpandedNode::Row { children, .. } = row(
        vec![control(&mut interner, "child", fixed(10.0, 4.0))],
        None,
        Some(0.0),
        None,
    ) else {
        panic!("expected a row");
    };
    let node = ExpandedNode::Row {
        children,
        id: None,
        size: None,
        measure: None,
        gap: Some(0.0),
        align: TextAlign::Center,
        pad: None,
        pad_x: Some(11.0),
        pad_y: Some(3.0),
        frame: None,
        background: None,
        background_alpha: None,
        active: None,
        active_background: None,
        frame_color: None,
        active_frame_color: None,
        surface: None,
    };

    let size = compute_size(&node, builtin::skin_doc(), DEFAULTS);

    assert_eq!(size.w.min(), 32.0);
    assert_eq!(size.h.min(), 10.0);
}

#[kithara::test]
fn shrink_child_opens_row_width() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![
            control(&mut interner, "fixed", fixed(10.0, 10.0)),
            control(
                &mut interner,
                "shrink",
                SizeSpec::new(Dim::Shrink, Dim::Fixed(10.0)),
            ),
        ],
        None,
        Some(0.0),
        None,
    );

    let size = compute_size(&node, builtin::skin_doc(), DEFAULTS);

    assert_eq!(size.w.min(), 10.0);
    assert_eq!(size.w.max(), None);
}

#[kithara::test]
fn crossfader_uses_skin_intrinsic_size() {
    let skin = builtin::skin_doc();

    assert_eq!(
        control_size(&ControlSpec::Crossfader { ticks: true }, skin),
        skin.crossfader.size
    );
}

#[kithara::test]
fn a_menu_glyph_takes_its_width_from_the_skin_and_its_height_from_the_row() {
    let skin = builtin::skin_doc();
    let glyph = |style| {
        control_size(
            &ControlSpec::Glyph {
                style,
                icon: IconName::Menu,
                color: None,
                active_color: None,
                active: None,
                active_icon: None,
            },
            skin,
        )
    };

    for (style, size) in [
        (GlyphStyle::Menu, skin.menu.icon_size),
        (GlyphStyle::MenuBurger, skin.menu.burger_icon_size),
        (GlyphStyle::MenuSmall, skin.menu.small_icon_size),
        (GlyphStyle::MenuCell, skin.menu.cell_icon_size),
    ] {
        assert_eq!(
            glyph(style),
            SizeSpec::new(Dim::Fixed(size), Dim::Fill),
            "{style:?}"
        );
    }
}

#[kithara::test]
fn a_shrinking_text_role_takes_its_height_from_the_row_that_holds_it() {
    let skin = builtin::skin_doc();
    let text = |style| {
        control_size(
            &ControlSpec::Text {
                style,
                label: None,
                color: None,
                active_color: None,
                active: None,
                align: TextAlign::Start,
                font: None,
                weight: None,
            },
            skin,
        )
    };

    for style in [TextStyle::Mono, TextStyle::Caption, TextStyle::BrandSmall] {
        assert_eq!(
            text(style),
            SizeSpec::new(Dim::Shrink, Dim::Fill),
            "{style:?}"
        );
    }
    assert_eq!(text(TextStyle::MicroLabel), skin.text.size);
}

#[kithara::test]
fn a_popover_is_the_size_of_its_anchor_and_its_content_never_counts() {
    let mut interner = Interner::new(1024);
    let origin = SourceUri("size-test.ron".to_owned());
    let node = ExpandedNode::Popover {
        path: interner.intern("menu", &origin).unwrap(),
        open: Binding {
            kind: BindingKind::Model,
            id: interner.intern("ui.menu.open", &origin).unwrap(),
            key: interner.intern("ui.menu.open", &origin).unwrap(),
            with: BTreeMap::new(),
        },
        at: PopoverAt::Anchor,
        align: PopoverAlign::Start,
        anchor: Box::new(control(&mut interner, "burger", fixed(36.0, 36.0))),
        content: Box::new(control(&mut interner, "pop", fixed(300.0, 400.0))),
    };

    assert_eq!(
        compute_size(&node, builtin::skin_doc(), DEFAULTS),
        fixed(36.0, 36.0)
    );
}

#[kithara::test]
fn row_sums_width_and_maximizes_height() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![
            control(&mut interner, "left", fixed(10.0, 4.0)),
            control(&mut interner, "right", fixed(6.0, 8.0)),
        ],
        None,
        Some(0.0),
        None,
    );

    let size = compute_size(&node, builtin::skin_doc(), DEFAULTS);

    assert_eq!(size.w.min(), 16.0);
    assert_eq!(size.h.min(), 8.0);
}

#[kithara::test]
fn column_maximizes_width_and_sums_height() {
    let mut interner = Interner::new(1024);
    let node = column(
        vec![
            control(&mut interner, "top", fixed(10.0, 4.0)),
            control(&mut interner, "bottom", fixed(6.0, 8.0)),
        ],
        Some(0.0),
    );

    let size = compute_size(&node, builtin::skin_doc(), DEFAULTS);

    assert_eq!(size.w.min(), 10.0);
    assert_eq!(size.h.min(), 12.0);
}

#[kithara::test]
fn skin_supplies_unspecified_gap_and_padding() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![
            control(&mut interner, "left", fixed(10.0, 4.0)),
            control(&mut interner, "right", fixed(6.0, 8.0)),
        ],
        None,
        None,
        None,
    );
    let mut skin = builtin::skin_doc().clone();
    skin.layout.grid_gap = 3.0;
    skin.layout.grid_pad = 2.0;

    let size = compute_size(&node, &skin, DEFAULTS);

    assert_eq!(size.w.min(), 23.0);
    assert_eq!(size.h.min(), 12.0);
}

#[kithara::test]
fn node_override_wins_over_composed_size() {
    let mut interner = Interner::new(1024);
    let override_size = fixed(100.0, 50.0);
    let node = row(
        vec![control(&mut interner, "child", fixed(10.0, 10.0))],
        Some(override_size),
        None,
        None,
    );

    assert_eq!(
        compute_size(&node, builtin::skin_doc(), DEFAULTS),
        override_size
    );
}

#[kithara::test]
fn fill_child_opens_row_width() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![
            control(&mut interner, "fixed", fixed(10.0, 10.0)),
            control(
                &mut interner,
                "fill",
                SizeSpec::new(Dim::Fill, Dim::Fixed(10.0)),
            ),
        ],
        None,
        Some(0.0),
        None,
    );

    let size = compute_size(&node, builtin::skin_doc(), DEFAULTS);

    assert_eq!(size.w.min(), 10.0);
    assert_eq!(size.w.max(), None);
}

#[kithara::test]
fn a_self_measured_node_is_opaque_to_its_parent() {
    let mut interner = Interner::new(1024);
    let declared = SizeSpec::new(Dim::Fill, Dim::Fixed(120.0));
    let node = ExpandedNode::Adaptive {
        measure: MeasureSpec::Width,
        size: Some(declared),
        base: Box::new(control(&mut interner, "narrow", fixed(40.0, 40.0))),
        steps: vec![(1000.0, control(&mut interner, "wide", fixed(900.0, 600.0)))],
    };

    assert_eq!(compute_size(&node, builtin::skin_doc(), DEFAULTS), declared);
    assert_eq!(
        compute_size(
            &node,
            builtin::skin_doc(),
            &SnapshotFixture::measured(Some(5000.0)),
        ),
        declared,
    );
    assert!(
        !has_blocks(&node),
        "an opaque node keeps a constant size, so the renderer may memoise it",
    );
}

fn reveal(from: f32, until: Option<f32>, child: ExpandedNode) -> ExpandedNode {
    ExpandedNode::Reveal {
        from,
        until,
        child: Box::new(child),
    }
}

fn optional(interner: &mut Interner, id: &str, child: ExpandedNode) -> ExpandedNode {
    let origin = SourceUri("size-test.ron".to_owned());
    let path = interner.intern(id, &origin).unwrap();
    ExpandedNode::Optional {
        block: BlockSpec {
            path,
            hidden: Binding {
                kind: BindingKind::Model,
                id: path,
                key: path,
                with: BTreeMap::new(),
            },
        },
        child: Box::new(child),
    }
}

#[kithara::test]
fn a_measuring_container_is_opaque_to_its_parent() {
    let mut interner = Interner::new(1024);
    let declared = SizeSpec::new(Dim::Fill, Dim::Fixed(42.0));
    let mut cell = |id| control(&mut interner, id, fixed(80.0, 20.0));
    let (wave, volume, quality) = (cell("wave"), cell("volume"), cell("cell"));
    let node = row(
        vec![
            reveal(350.0, None, wave),
            reveal(440.0, None, volume),
            optional(&mut interner, "quality", quality),
        ],
        Some(declared),
        Some(0.0),
        Some(MeasureAxis::Width),
    );

    assert_eq!(compute_size(&node, builtin::skin_doc(), DEFAULTS), declared);
    assert_eq!(
        compute_size(&node, builtin::skin_doc(), &SnapshotFixture::all_hidden()),
        declared
    );
    assert!(
        !has_blocks(&node),
        "an opaque container keeps a constant size, so the renderer may memoise it",
    );
}

#[kithara::test]
fn a_declared_box_leaves_the_cells_their_room() {
    let mut interner = Interner::new(1024);
    let node = row(
        vec![
            control(&mut interner, "left", fixed(10.0, 4.0)),
            control(&mut interner, "right", fixed(6.0, 8.0)),
        ],
        Some(SizeSpec::FILL),
        None,
        None,
    );
    let mut skin = builtin::skin_doc().clone();
    skin.layout.grid_gap = 3.0;
    skin.layout.grid_pad = 2.0;

    assert_eq!(compute_size(&node, &skin, DEFAULTS), SizeSpec::FILL);
    assert_eq!(min_size(&node, &skin), fixed(23.0, 12.0));
}

#[kithara::test]
fn a_measuring_container_needs_the_room_its_standing_cells_settle_on() {
    let mut interner = Interner::new(1024);
    let skin = builtin::skin_doc();
    let bar = |always: SizeSpec, interner: &mut Interner| {
        let wave = control(interner, "wave", fixed(40.0, 20.0));
        let remain = control(interner, "remain", fixed(40.0, 20.0));
        row(
            vec![
                control(interner, "menu", always),
                reveal(150.0, None, wave),
                reveal(500.0, None, remain),
            ],
            Some(SizeSpec::new(Dim::Fill, Dim::Fixed(42.0))),
            Some(0.0),
            Some(MeasureAxis::Width),
        )
    };

    let narrow = bar(fixed(100.0, 20.0), &mut interner);
    let wide = bar(fixed(200.0, 20.0), &mut interner);
    let chained = row(
        vec![
            control(&mut interner, "menu", fixed(100.0, 20.0)),
            reveal(
                100.0,
                None,
                control(&mut interner, "wave", fixed(40.0, 20.0)),
            ),
            reveal(
                140.0,
                None,
                control(&mut interner, "remain", fixed(40.0, 20.0)),
            ),
            reveal(
                180.0,
                None,
                control(&mut interner, "quality", fixed(40.0, 20.0)),
            ),
        ],
        Some(SizeSpec::new(Dim::Fill, Dim::Fixed(42.0))),
        Some(0.0),
        Some(MeasureAxis::Width),
    );

    assert_eq!(min_size(&narrow, skin), fixed(100.0, 42.0));
    assert_eq!(min_size(&wide, skin), fixed(240.0, 42.0));
    assert_eq!(min_size(&chained, skin), fixed(220.0, 42.0));
}

#[kithara::test]
fn a_chain_of_reveals_settles_on_the_last_one_it_opens() {
    // Each cell opens just inside the width the ones before it need, so the
    // container only reaches its final width by following the whole chain.
    let mut interner = Interner::new(1024);
    let mut cell = |id, width| control(&mut interner, id, fixed(width, 20.0));
    let base = cell("menu", 40.0);
    let steps = [
        (40.0, cell("a", 30.0)),
        (70.0, cell("b", 30.0)),
        (100.0, cell("c", 30.0)),
        (130.0, cell("d", 30.0)),
        (160.0, cell("e", 30.0)),
    ];
    let mut children = vec![base];
    children.extend(steps.map(|(from, child)| reveal(from, None, child)));
    let bar = row(
        children,
        Some(SizeSpec::new(Dim::Fill, Dim::Fixed(42.0))),
        Some(0.0),
        Some(MeasureAxis::Width),
    );

    assert_eq!(
        min_size(&bar, builtin::skin_doc()),
        fixed(190.0, 42.0),
        "the chain settles on the width where nothing further opens"
    );
}

#[kithara::test]
fn the_rooms_a_bar_answers_are_its_own_openings() {
    let mut interner = Interner::new(1024);
    let mut cell = |id, width| control(&mut interner, id, fixed(width, 20.0));
    let bar = row(
        vec![
            cell("menu", 40.0),
            reveal(40.0, None, cell("wave", 30.0)),
            reveal(120.0, None, cell("remain", 30.0)),
        ],
        Some(SizeSpec::new(Dim::Fill, Dim::Fixed(42.0))),
        Some(0.0),
        Some(MeasureAxis::Width),
    );

    assert_eq!(
        rooms(&bar, MeasureAxis::Width, builtin::skin_doc()),
        vec![(70.0, 70.0), (120.0, 100.0)],
        "the narrowest room the bar stands in, then each opening above it"
    );
}

#[kithara::test]
fn a_measuring_column_sums_the_cells_standing_in_its_room() {
    let mut interner = Interner::new(1024);
    let cells = vec![
        control(&mut interner, "head", fixed(20.0, 10.0)),
        reveal(
            10.0,
            None,
            control(&mut interner, "body", fixed(20.0, 25.0)),
        ),
    ];
    let ExpandedNode::Column { children, .. } = column(cells, Some(4.0)) else {
        panic!("expected a column");
    };
    let node = ExpandedNode::Column {
        children,
        gap: Some(4.0),
        measure: Some(MeasureAxis::Height),
        size: Some(SizeSpec::new(Dim::Fixed(20.0), Dim::Fill)),
        id: None,
        align: TextAlign::Start,
        pad: None,
        pad_x: None,
        pad_y: None,
        frame: None,
        frame_color: None,
        background: None,
        background_alpha: None,
        surface: None,
    };

    assert_eq!(
        min_size(&node, builtin::skin_doc()),
        fixed(20.0, 39.0),
        "a column stacks the cells its room opens, gap included"
    );
}

#[kithara::test]
fn a_band_leaves_the_room_to_the_cell_opening_where_it_closes() {
    let mut interner = Interner::new(1024);
    let skin = builtin::skin_doc();
    let mut cell = |id, width| control(&mut interner, id, fixed(width, 20.0));
    let (menu, play, window) = (cell("menu", 40.0), cell("play", 38.0), cell("window", 80.0));
    let (strip, wave) = (cell("strip", 36.0), cell("wave", 60.0));
    let bar = row(
        vec![
            menu,
            play,
            reveal(0.0, Some(350.0), strip),
            reveal(350.0, None, wave),
            window,
        ],
        Some(SizeSpec::new(Dim::Fill, Dim::Fixed(42.0))),
        Some(0.0),
        Some(MeasureAxis::Width),
    );

    assert_eq!(
        min_size(&bar, skin).w.min(),
        194.0,
        "the strip stands at the narrowest the bar gets",
    );
    assert_eq!(
        rooms(&bar, MeasureAxis::Width, skin),
        vec![(194.0, 194.0), (350.0, 218.0)],
        "350 takes the strip out and stands the wave in its room",
    );
}

#[kithara::test]
fn cells_laid_edge_to_edge_settle_where_the_bar_does() {
    let cell = |from, until, width| Cell::new(from, until, fixed(width, 20.0));
    let bar = Cells::new(
        Axis::Horizontal,
        vec![
            cell(0.0, None, 40.0),
            cell(0.0, None, 38.0),
            cell(0.0, Some(350.0), 36.0),
            cell(350.0, None, 60.0),
            cell(0.0, None, 80.0),
        ],
    );

    assert_eq!(
        bar.settled(Some(MeasureAxis::Width)).w.min(),
        194.0,
        "the strip stands at the narrowest the bar gets",
    );
    assert_eq!(
        bar.rooms(MeasureAxis::Width, 194.0),
        vec![(194.0, 194.0), (350.0, 218.0)],
        "350 takes the strip out and stands the wave in its room",
    );
    assert_eq!(
        bar.settled(None).w.min(),
        254.0,
        "a split reading no room stands every cell it holds",
    );
    let edge = Cells::new(
        Axis::Horizontal,
        vec![cell(0.0, None, 40.0), cell(194.0, None, 60.0)],
    );
    assert_eq!(
        edge.rooms(MeasureAxis::Width, 194.0),
        vec![(194.0, 100.0)],
        "a cell opening at the narrowest room is that room, not a second answer beside it",
    );
}

#[kithara::test]
fn a_block_takes_its_room_whether_the_host_shows_it_or_not() {
    let mut interner = Interner::new(1024);
    let skin = builtin::skin_doc();
    let quality = control(&mut interner, "quality", fixed(80.0, 20.0));
    let node = row(
        vec![
            control(&mut interner, "menu", fixed(100.0, 20.0)),
            optional(&mut interner, "quality", quality),
        ],
        None,
        Some(0.0),
        None,
    );

    assert_eq!(min_size(&node, skin), fixed(180.0, 20.0));
    assert_eq!(
        compute_size(&node, skin, &SnapshotFixture::all_hidden())
            .w
            .min(),
        100.0
    );
}

#[kithara::test]
fn a_control_with_no_box_of_its_own_offers_the_one_its_skin_gives_it() {
    let mut interner = Interner::new(1024);
    let origin = SourceUri("size-test.ron".to_owned());
    let skin = builtin::skin_doc();
    let id = interner.intern("knob", &origin).unwrap();
    let bare = |spec| ExpandedNode::Control {
        path: id,
        id,
        spec,
        read: None,
        write: None,
        size: None,
    };

    assert_eq!(
        effective_size(&bare(ControlSpec::Knob { label: None }), skin, DEFAULTS),
        Some(skin.knob.size),
        "a knob composes, so a parent may size itself on it",
    );
    assert_eq!(
        effective_size(&bare(ControlSpec::TabLarge { label: id }), skin, DEFAULTS),
        None,
        "a tab fills the strip it sits in, so it offers no box to compose with",
    );
}

#[kithara::test]
fn the_default_snapshot_answers_no_measurement() {
    let mut interner = Interner::new(1024);
    let origin = SourceUri("size-test.ron".to_owned());
    let id = interner.intern("width", &origin).unwrap();
    let binding = Binding {
        kind: BindingKind::Model,
        id,
        key: id,
        with: BTreeMap::new(),
    };

    assert_eq!(
        DEFAULTS.measure(&binding),
        None,
        "a tree measured outside a window takes its base branch, not a room of zero",
    );
}

#[kithara::test]
fn a_split_that_blocks_nothing_keeps_the_box_it_was_compiled_with() {
    let declared = fixed(300.0, 42.0);
    let cell = |width| SplitCell {
        node: CompiledNode::Split {
            axis: Axis::Horizontal,
            measure: None,
            children: Vec::new(),
            size: Some(fixed(width, 20.0)),
            composed: fixed(width, 20.0),
            blocks: false,
        },
        until: None,
        from: 0.0,
        weight: 1.0,
    };
    let split = CompiledNode::Split {
        axis: Axis::Horizontal,
        measure: None,
        children: vec![cell(40.0), cell(60.0)],
        size: Some(declared),
        composed: fixed(100.0, 20.0),
        blocks: false,
    };

    assert_eq!(
        compiled_node_size_with_hidden(&split, builtin::skin_doc(), DEFAULTS),
        declared,
        "with no block under it there is nothing to recompute, so its own box stands",
    );
}

struct SnapshotFixture {
    measured: Option<f32>,
    hidden: bool,
}

impl SnapshotFixture {
    const fn all_hidden() -> Self {
        Self {
            hidden: true,
            measured: None,
        }
    }

    const fn measured(measured: Option<f32>) -> Self {
        Self {
            measured,
            hidden: false,
        }
    }
}

impl Snapshot for SnapshotFixture {
    fn hidden(&self, _: &BlockSpec) -> bool {
        self.hidden
    }

    fn measure(&self, _: &Binding) -> Option<f32> {
        self.measured
    }
}

/// Sizes read off a compiled document, where hidden blocks and measured
/// branches come from what the snapshot answers.
mod compiled {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        compile::{CompiledUi, compile},
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        size::compiled_node_size_with_hidden as node_size,
        source::{MemResolver, UiConfig},
        view,
    };

    struct Registry {
        flag: EndpointDesc,
        scalar: EndpointDesc,
        trigger: EndpointDesc,
    }

    impl Default for Registry {
        fn default() -> Self {
            Self {
                flag: EndpointDesc::new(ValueKind::Bool),
                scalar: EndpointDesc::new(ValueKind::Scalar),
                trigger: EndpointDesc::new(ValueKind::Trigger),
            }
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (EndpointCategory::Model, "ui.block.hidden") => Some(&self.flag),
                (EndpointCategory::Model, "ui.measure") => Some(&self.scalar),
                (EndpointCategory::Command, "ui.press") => Some(&self.trigger),
                _ => None,
            }
        }
    }

    fn compiled(module: &str) -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "blocks.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "blocks",
                root: Module(instance: "mixer", source: "blocks.kmodule.ron"))"#,
        );
        resolver.insert("blocks.kmodule.ron", module);
        compile(
            "blocks.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .unwrap()
    }

    fn size_of(ui: &CompiledUi, snapshot: &dyn Snapshot) -> SizeSpec {
        node_size(&ui.root, builtin::skin_doc(), snapshot)
    }

    const HIDDEN: &dyn Snapshot = &SnapshotFixture::all_hidden();

    #[kithara::test]
    fn an_adaptive_bank_is_the_size_of_the_branch_its_measure_selects() {
        let ui = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Popover(
                    id: "menu",
                    open: Model(id: "ui.block.hidden"),
                    anchor: Pressable(
                        id: "press",
                        press: Command(id: "ui.press"),
                        child: Adaptive(
                            id: "bank",
                            measure: Read(Model(id: "ui.measure")),
                            base: Row(id: "narrow", gap: 0.0, pad: 0.0, children: [
                                Knob(id: "low"),
                            ]),
                            steps: [
                                (from: 4.0, node: Row(id: "wide", gap: 0.0, pad: 0.0, children: [
                                    Knob(id: "low-4"),
                                    Knob(id: "high-4"),
                                ])),
                            ],
                        ),
                    ),
                    content: Knob(id: "pop"),
                ))"#,
        );

        let three = size_of(&ui, &SnapshotFixture::measured(Some(3.0)));
        let four = size_of(&ui, &SnapshotFixture::measured(Some(4.0)));

        assert_ne!(three, four, "each branch measures for itself");
        assert_eq!(
            size_of(&ui, DEFAULTS),
            three,
            "an unread measure takes base"
        );
        assert!(four.w.min() > three.w.min());
    }

    #[kithara::test]
    fn a_hidden_block_leaves_the_module_the_size_it_has_without_it() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 0.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let trimmed = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 0.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                ]))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, DEFAULTS));
        assert_ne!(
            size_of(&full, DEFAULTS),
            size_of(&trimmed, DEFAULTS),
            "a visible block takes space",
        );
    }

    #[kithara::test]
    fn a_hidden_block_takes_its_gap_with_it() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 9.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let trimmed = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 9.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                ]))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, DEFAULTS));
    }

    #[kithara::test]
    fn a_slot_whose_only_child_is_hidden_fills_like_an_empty_one() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Slot(id: "extra", default: [
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let empty = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Slot(id: "extra"))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&empty, DEFAULTS));
        assert_ne!(size_of(&full, DEFAULTS), size_of(&empty, DEFAULTS));
    }

    /// Both hosts measure a stack's first layer and hand every layer that box:
    /// `Stack` in iced, and `NodeLayout::Stack` on masonry. The document has to
    /// say the same thing, or a stage would ask for a box neither host gives it.
    #[kithara::test]
    fn a_stage_is_the_size_of_its_first_child() {
        let stage = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Stage(id: "scene", children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                ]))"#,
        );
        let alone = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Stage(id: "scene", children: [Knob(id: "volume")]))"#,
        );

        assert_eq!(size_of(&stage, DEFAULTS), size_of(&alone, DEFAULTS));
    }

    /// The same two knobs in a column do add up, which is what makes the
    /// previous assertion a statement about stacking rather than about knobs.
    #[kithara::test]
    fn a_column_of_the_same_two_children_is_taller_than_the_stage() {
        let stage = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Stage(id: "scene", children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                ]))"#,
        );
        let column = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Column(gap: 0.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                ]))"#,
        );

        assert_ne!(size_of(&stage, DEFAULTS), size_of(&column, DEFAULTS));
    }

    #[kithara::test]
    fn a_split_whose_children_are_all_hidden_folds_to_nothing() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "blocks.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Knob(id: "low"))"#,
        );
        resolver.insert(
            "split.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "split",
                root: Split(axis: Horizontal, children: [
                    (node: Optional(id: "left", hidden: Model(id: "ui.block.hidden"),
                        node: Module(instance: "a", source: "blocks.kmodule.ron"))),
                    (node: Optional(id: "right", hidden: Model(id: "ui.block.hidden"),
                        node: Module(instance: "b", source: "blocks.kmodule.ron"))),
                ]))"#,
        );
        let ui = compile(
            "split.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .unwrap();

        assert_eq!(
            size_of(&ui, HIDDEN),
            SizeSpec::new(Dim::Fixed(0.0), Dim::Fixed(0.0)),
        );
        assert_ne!(size_of(&ui, DEFAULTS), size_of(&ui, HIDDEN));
    }
}
