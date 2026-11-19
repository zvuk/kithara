//! What a document is refused for, asked of the compiler that refuses it.

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    mock::MapEndpoints,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    source::{MemResolver, UiConfig},
    view,
};

const LAYOUT: &str = "validate.klayout.ron";
const MODULE: &str = "m.ron";

/// The endpoints the documents below bind, and nothing else.
fn registry() -> MapEndpoints {
    let mut registry = MapEndpoints::default();
    for (category, id, description) in [
        (
            EndpointCategory::Command,
            "deck.transport.toggle_play",
            EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        ),
        (
            EndpointCategory::Parameter,
            "player.output.volume",
            EndpointDesc::new(ValueKind::Scalar),
        ),
        (
            EndpointCategory::Parameter,
            "deck.tempo.rate",
            EndpointDesc::new(ValueKind::Scalar),
        ),
        (
            EndpointCategory::Model,
            "library.breadcrumb",
            EndpointDesc::new(ValueKind::Text),
        ),
        (
            EndpointCategory::Model,
            "ui.tint",
            EndpointDesc::new(ValueKind::Color),
        ),
        (
            EndpointCategory::Model,
            "library.visible_tracks",
            EndpointDesc::new(ValueKind::Table),
        ),
        (
            EndpointCategory::Model,
            "ui.measure",
            EndpointDesc::new(ValueKind::Scalar),
        ),
        (
            EndpointCategory::Model,
            "app.phase",
            EndpointDesc::new(ValueKind::Scalar),
        ),
        (
            EndpointCategory::Model,
            "app.time",
            EndpointDesc::new(ValueKind::Scalar),
        ),
        (
            EndpointCategory::Command,
            "ui.press",
            EndpointDesc::new(ValueKind::Trigger),
        ),
        (
            EndpointCategory::Command,
            "library.select_scope",
            EndpointDesc::new(ValueKind::Index),
        ),
    ] {
        registry.insert(category, id, description);
    }
    registry
}

/// Compiles a layout whose modules are all the one module written here.
fn compiled(layout: &str, module: &str) -> Result<CompiledUi, UiDocError> {
    let mut resolver = MemResolver::default();
    resolver.insert(LAYOUT, layout);
    resolver.insert(MODULE, module);
    compile(
        LAYOUT,
        &resolver,
        &registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::builder().build(),
        &view::EMPTY,
    )
}

/// A layout with this root, over a module with nothing to refuse.
fn layout_root(root: &str) -> Result<CompiledUi, UiDocError> {
    compiled(
        &format!(r#"(schema: "kithara.layout", version: 1, id: "l", root: {root})"#),
        r#"(schema: "kithara.module", version: 1, id: "m",
            root: Button(id: "x", label: "X"))"#,
    )
}

/// One module with this id and root, placed once as `instance`.
fn module_as(instance: &str, id: &str, root: &str) -> Result<CompiledUi, UiDocError> {
    compiled(
        &format!(
            r#"(schema: "kithara.layout", version: 1, id: "l",
                root: Module(instance: "{instance}", source: "{MODULE}"))"#
        ),
        &format!(r#"(schema: "kithara.module", version: 1, id: "{id}", root: {root})"#),
    )
}

fn module_root(root: &str) -> Result<CompiledUi, UiDocError> {
    module_as("demo", "m", root)
}

fn refused(result: Result<CompiledUi, UiDocError>) -> UiDocError {
    match result {
        Ok(_) => panic!("the document must be refused"),
        Err(error) => error,
    }
}

fn accepted(result: Result<CompiledUi, UiDocError>) {
    if let Err(error) = result {
        panic!("the document must compile: {error}");
    }
}

#[kithara::test]
#[case::split(
    r#"Split(axis: Horizontal, children: [
        (node: Module(instance: "deck-a", source: "m.ron")),
        (node: Module(instance: "deck-a", source: "m.ron")),
    ])"#,
    "root/Split[1]/Module(deck-a)"
)]
#[case::after_adaptive(
    r#"Split(axis: Horizontal, children: [
        (node: Adaptive(id: "bank", measure: Width, size: (w: Fill, h: Fill),
            base: Module(instance: "deck-base", source: "m.ron"),
            steps: [(from: 120.0, node: Module(instance: "deck-a", source: "m.ron"))])),
        (node: Module(instance: "deck-a", source: "m.ron")),
    ])"#,
    "root/Split[1]/Module(deck-a)"
)]
#[case::after_tabs(
    r#"Split(axis: Horizontal, children: [
        (node: Tabs(state: "shown", initial: "one", pages: {
            "one": Module(instance: "deck-base", source: "m.ron"),
            "two": Module(instance: "deck-a", source: "m.ron"),
        })),
        (node: Module(instance: "deck-a", source: "m.ron")),
    ])"#,
    "root/Split[1]/Module(deck-a)"
)]
fn a_duplicate_instance_in_a_layout_is_found(#[case] layout: &str, #[case] at: &str) {
    let error = refused(layout_root(layout));

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, path, .. }
            if id == "deck-a" && path == at),
        "{error:?}"
    );
}

#[kithara::test]
fn layout_instance_with_path_separator_is_rejected() {
    let error = refused(layout_root(
        r#"Module(instance: "deck/a", source: "m.ron")"#,
    ));
    assert!(
        matches!(&error, UiDocError::InvalidId { id, reason, .. }
            if id == "deck/a" && reason.contains('/')),
        "{error:?}"
    );
}

#[kithara::test]
fn empty_and_parameter_like_instances_are_rejected() {
    for id in ["", "$deck"] {
        let error = refused(layout_root(&format!(
            r#"Module(instance: "{id}", source: "m.ron")"#
        )));
        assert!(
            matches!(&error, UiDocError::InvalidId { id: invalid, .. } if invalid == id),
            "{id}: {error:?}"
        );
    }
}

#[kithara::test]
fn a_split_weight_is_positive() {
    for (weight, written) in [("-1.0", "-1"), ("0.0", "0")] {
        let error = refused(layout_root(&format!(
            r#"Split(axis: Horizontal, children: [
                (weight: {weight}, node: Module(instance: "deck-a", source: "m.ron")),
            ])"#
        )));
        assert!(
            matches!(&error, UiDocError::InvalidWeight { path, value, .. }
                if path == "root/Split[0]" && value == written),
            "{weight}: {error:?}"
        );
    }
}

fn split_cell(head: &str, tail: &str) -> Result<CompiledUi, UiDocError> {
    layout_root(&format!(
        r#"Split(axis: Horizontal, {head} children: [
            (node: Module(instance: "deck-a", source: "m.ron"){tail}),
        ])"#
    ))
}

fn measuring_split_cell(tail: &str) -> Result<CompiledUi, UiDocError> {
    split_cell("measure: Width, size: (w: Fill, h: Fixed(42.0)),", tail)
}

#[kithara::test]
fn a_band_stands_only_among_the_cells_of_a_measuring_split() {
    for band in [", from: 350.0", ", until: Some(350.0)"] {
        let error = refused(split_cell("", band));
        assert!(
            matches!(&error, UiDocError::UnmeasuredReveal { path, .. }
                if path == "root/Split[0]"),
            "{band}: {error:?}",
        );
    }
}

#[kithara::test]
fn a_measuring_split_reveals_its_own_cells() {
    accepted(layout_root(
        r#"Split(axis: Horizontal, measure: Width, size: (w: Fill, h: Fixed(42.0)), children: [
            (node: Module(instance: "menu", source: "m.ron")),
            (node: Module(instance: "strip", source: "m.ron"), until: Some(350.0)),
            (node: Module(instance: "wave", source: "m.ron"), from: 350.0),
        ])"#,
    ));
}

#[kithara::test]
fn a_split_band_is_finite_and_closes_above_the_room_it_opens_in() {
    for band in [", from: -1.0", ", from: inf", ", from: NaN"] {
        let error = refused(measuring_split_cell(band));
        assert!(
            matches!(&error, UiDocError::RevealThreshold { .. }),
            "{band}: {error:?}",
        );
    }
    for band in [
        ", from: 350.0, until: Some(350.0)",
        ", from: 350.0, until: Some(0.0)",
        ", until: Some(-inf)",
        ", until: Some(NaN)",
    ] {
        let error = refused(measuring_split_cell(band));
        assert!(
            matches!(&error, UiDocError::RevealBand { .. }),
            "{band}: {error:?}",
        );
    }
}

#[kithara::test]
fn empty_and_parameter_like_control_ids_are_rejected() {
    for id in ["", "$deck"] {
        let error = refused(module_root(&format!(
            r#"Button(id: "{id}", label: "PLAY")"#
        )));
        assert!(
            matches!(&error, UiDocError::InvalidId { id: invalid, .. } if invalid == id),
            "{id}: {error:?}"
        );
    }
}

#[kithara::test]
fn control_id_with_path_separator_is_rejected() {
    let error = refused(module_root(
        r#"Button(id: "transport/play", label: "PLAY")"#,
    ));
    assert!(
        matches!(&error, UiDocError::InvalidId { id, reason, .. }
            if id == "transport/play" && reason.contains('/')),
        "{error:?}"
    );
}

#[kithara::test]
#[case::row(r#"Row(children: [Button(id: "play", label: "PLAY")])"#)]
#[case::stage(r#"Stage(id: "scene", children: [Button(id: "play", label: "PLAY")])"#)]
fn an_object_may_move_a_whole_group(#[case] child: &str) {
    accepted(module_root(&format!(
        r#"Object(id: "shift", transform: (position: (8.0, 0.0)), child: {child})"#
    )));
}

#[kithara::test]
#[case::rotation_row(
    "(rotation: 30.0)",
    r#"Row(children: [Button(id: "play", label: "PLAY")])"#,
    "Row"
)]
#[case::scale_row(
    "(scale: (2.0, 2.0))",
    r#"Row(children: [Button(id: "play", label: "PLAY")])"#,
    "Row"
)]
#[case::rotation_stage(
    "(rotation: 30.0)",
    r#"Stage(id: "scene", children: [Button(id: "play", label: "PLAY")])"#,
    "Stage"
)]
fn an_object_may_not_turn_or_scale_a_group(
    #[case] transform: &str,
    #[case] child: &str,
    #[case] rejected: &str,
) {
    let error = refused(module_root(&format!(
        r#"Object(id: "pose", transform: {transform}, child: {child})"#
    )));

    assert!(
        matches!(&error, UiDocError::ObjectGroup { child, path, .. }
            if *child == rejected && path == "root/Object(pose)"),
        "{error:?}"
    );
}

/// A visualiser paints its own pass, so an object over it would move the
/// box and leave the picture. Refusing beats drawing the wrong answer.
#[kithara::test]
fn an_object_may_not_even_move_a_native_pass() {
    let error = refused(module_root(
        r#"Object(id: "shift", transform: (position: (8.0, 0.0)),
            child: Vis(id: "scope"))"#,
    ));

    assert!(
        matches!(error, UiDocError::ObjectNative { child: "Vis", .. }),
        "{error:?}"
    );
}

/// A still object is the identity, and the identity reaches everything
/// because it does nothing.
#[kithara::test]
fn a_still_object_may_wrap_anything() {
    accepted(module_root(
        r#"Object(id: "still", child: Vis(id: "scope"))"#,
    ));
}

/// A motion computes the phase, so an object carrying both leaves one pose
/// with two answers. There is no honest rule for ranking them, and inventing
/// one is what refusing here avoids.
#[kithara::test]
fn an_object_may_not_be_driven_twice() {
    let error = refused(module_root(
        r#"Object(id: "spin", to: (rotation: 360.0),
            phase: Model(id: "app.phase"),
            motion: (clock: Model(id: "app.time"), duration: 4.0),
            child: Button(id: "play", label: "PLAY"))"#,
    ));

    assert!(
        matches!(error, UiDocError::ObjectDrivenTwice { .. }),
        "{error:?}"
    );
}

#[kithara::test]
fn an_object_may_be_driven_by_a_motion_alone() {
    accepted(module_root(
        r#"Object(id: "spin", to: (rotation: 360.0),
            motion: (clock: Model(id: "app.time"), duration: 4.0, repeat: Loop),
            child: Button(id: "play", label: "PLAY"))"#,
    ));
}

#[kithara::test]
fn module_id_with_an_address_separator_is_rejected() {
    let error = refused(module_as(
        "demo",
        "studio.strip",
        r#"Button(id: "play", label: "PLAY")"#,
    ));
    assert!(
        matches!(&error, UiDocError::InvalidId { id, reason, .. }
            if id == "studio.strip" && reason.contains("'.'")),
        "{error:?}"
    );
}

#[kithara::test]
fn a_container_that_writes_without_an_id_is_rejected() {
    let error = refused(module_root(
        r#"Row(write: Parameter(id: "deck.tempo.rate"), children: [
            Button(id: "play", label: "PLAY"),
        ])"#,
    ));
    assert!(
        matches!(&error, UiDocError::UnaddressedSurface { path, .. } if path == "root"),
        "{error:?}"
    );
}

fn adaptive(steps: &str) -> Result<CompiledUi, UiDocError> {
    module_root(&format!(
        r#"Adaptive(
            id: "bank",
            measure: Read(Model(id: "ui.measure")),
            base: Knob(id: "low"),
            steps: [{steps}],
        )"#
    ))
}

fn layout_adaptive(steps: &str) -> Result<CompiledUi, UiDocError> {
    layout_root(&format!(
        r#"Adaptive(id: "bank", measure: Width, size: (w: Fill, h: Fill),
            base: Module(instance: "base", source: "m.ron"),
            steps: [{steps}])"#
    ))
}

#[kithara::test]
fn an_adaptive_node_without_steps_is_rejected() {
    let error = refused(adaptive(""));

    assert!(
        matches!(&error, UiDocError::AdaptiveWithoutSteps { id, path, .. }
            if id == "bank" && path == "root/Adaptive(bank)"),
        "{error:?}"
    );
}

#[kithara::test]
#[case::module_descending(
    adaptive,
    r#"(from: 4.0, node: Knob(id: "a")), (from: 2.0, node: Knob(id: "b"))"#,
    1
)]
#[case::module_equal(
    adaptive,
    r#"(from: 4.0, node: Knob(id: "a")), (from: 4.0, node: Knob(id: "b"))"#,
    1
)]
#[case::module_non_finite(adaptive, r#"(from: NaN, node: Knob(id: "a"))"#, 0)]
#[case::layout_descending(
    layout_adaptive,
    r#"(from: 4.0, node: Module(instance: "a", source: "m.ron")),
        (from: 2.0, node: Module(instance: "b", source: "m.ron"))"#,
    1
)]
#[case::layout_equal(
    layout_adaptive,
    r#"(from: 4.0, node: Module(instance: "a", source: "m.ron")),
        (from: 4.0, node: Module(instance: "b", source: "m.ron"))"#,
    1
)]
#[case::layout_non_finite(
    layout_adaptive,
    r#"(from: NaN, node: Module(instance: "a", source: "m.ron"))"#,
    0
)]
fn adaptive_steps_must_climb(
    #[case] document: fn(&str) -> Result<CompiledUi, UiDocError>,
    #[case] steps: &str,
    #[case] at: usize,
) {
    let error = refused(document(steps));

    assert!(
        matches!(&error, UiDocError::AdaptiveStepOrder { index, path, .. }
            if *index == at && path == "root/Adaptive(bank)"),
        "{error:?}"
    );
}

fn measured(measure: &str, size: &str) -> Result<CompiledUi, UiDocError> {
    module_root(&format!(
        r#"Adaptive(
            id: "bank",
            measure: {measure},
            {size}
            base: Knob(id: "low"),
            steps: [(from: 120.0, node: Knob(id: "high"))],
        )"#
    ))
}

#[kithara::test]
#[case::width_without_size("Width", "")]
#[case::height_without_size("Height", "")]
#[case::width_with_shrink("Width", "size: (w: Shrink, h: Fill),")]
#[case::height_with_shrink("Height", "size: (w: Fill, h: Shrink),")]
fn a_measuring_node_must_declare_the_axis_it_measures(#[case] measure: &str, #[case] size: &str) {
    for (at, result) in [
        (
            "root/Group(bar)",
            module_root(&format!(
                r#"Row(id: "bar", measure: {measure}, {size} children: [Knob(id: "a")])"#
            )),
        ),
        ("root/Adaptive(bank)", measured(measure, size)),
        (
            "root/Split",
            split_cell(&format!("measure: {measure}, {size}"), ""),
        ),
    ] {
        let error = refused(result);
        assert!(
            matches!(&error, UiDocError::UnmeasuredAxis { path, .. }
                if path == at),
            "{at}: {error:?}",
        );
    }
}

#[kithara::test]
fn a_declared_axis_carries_a_self_measured_node() {
    accepted(measured("Width", "size: Some((w: Fill, h: Shrink)),"));
    accepted(measured(
        "Height",
        "size: Some((w: Shrink, h: Fixed(80.0))),",
    ));
}

#[kithara::test]
fn a_read_measure_declares_no_box() {
    let error = refused(measured(
        r#"Read(Model(id: "ui.measure"))"#,
        "size: Some((w: Fill, h: Fill)),",
    ));

    assert!(
        matches!(&error, UiDocError::MeasuredBoxWithoutAxis { id, .. } if id == "bank"),
        "{error:?}",
    );
}

const BAR: &str = r#"id: "bar", measure: Width, size: (w: Fill, h: Fixed(42.0)),"#;

#[kithara::test]
fn a_reveal_stands_only_among_the_children_of_a_measuring_container() {
    for root in [
        r#"Reveal(from: 1.0, child: Knob(id: "low"))"#.to_owned(),
        r#"Row(children: [Reveal(from: 1.0, child: Knob(id: "low"))])"#.to_owned(),
        format!(
            r#"Row({BAR} children: [Pressable(id: "press", press: Command(id: "ui.press"),
                child: Reveal(from: 1.0, child: Knob(id: "low")))])"#
        ),
        format!(
            r#"Row({BAR} children: [Reveal(from: 1.0,
                child: Reveal(from: 2.0, child: Knob(id: "low")))])"#
        ),
    ] {
        let error = refused(module_root(&root));
        assert!(
            matches!(&error, UiDocError::UnmeasuredReveal { .. }),
            "{root}: {error:?}",
        );
    }
}

#[kithara::test]
fn a_measuring_container_reveals_its_own_children() {
    accepted(module_root(&format!(
        r#"Row({BAR} children: [
            Knob(id: "low"),
            Reveal(from: 0.0, child: Knob(id: "mid")),
            Reveal(from: 350.0, child: Knob(id: "high")),
        ])"#
    )));
}

#[kithara::test]
fn a_threshold_is_finite_and_not_negative() {
    for from in ["-1.0", "inf", "-inf", "NaN"] {
        let root = format!(r#"Row({BAR} children: [Reveal(from: {from}, child: Knob(id: "a"))])"#);
        let error = refused(module_root(&root));
        assert!(
            matches!(&error, UiDocError::RevealThreshold { .. }),
            "{from}: {error:?}",
        );
    }
}

#[kithara::test]
fn a_band_closes_above_the_room_it_opens_in() {
    for until in ["0.0", "350.0", "inf", "-inf", "NaN"] {
        let root = format!(
            r#"Row({BAR} children: [Reveal(from: 350.0, until: Some({until}),
                child: Knob(id: "a"))])"#
        );
        let error = refused(module_root(&root));
        assert!(
            matches!(&error, UiDocError::RevealBand { .. }),
            "{until}: {error:?}",
        );
    }
}

#[kithara::test]
fn bands_meeting_at_one_number_stand_in_one_line() {
    accepted(module_root(&format!(
        r#"Row({BAR} children: [
            Reveal(from: 0.0, until: Some(350.0), child: Knob(id: "strip")),
            Reveal(from: 350.0, child: Knob(id: "wave")),
        ])"#
    )));
}

#[kithara::test]
fn thresholds_need_not_climb() {
    accepted(module_root(&format!(
        r#"Row({BAR} children: [
            Reveal(from: 520.0, child: Knob(id: "a")),
            Reveal(from: 350.0, child: Knob(id: "b")),
        ])"#
    )));
}

#[kithara::test]
fn adaptive_branches_may_name_the_same_place() {
    accepted(adaptive(r#"(from: 4.0, node: Knob(id: "low"))"#));
}

#[kithara::test]
fn unique_ids_pass() {
    accepted(module_root(
        r#"Row(children: [
            Button(id: "play", label: "PLAY"),
            Slot(id: "extra"),
        ])"#,
    ));
}

#[kithara::test]
fn valid_command_binding_passes() {
    accepted(module_root(
        r#"Button(id: "play", label: "PLAY",
            write: Command(id: "deck.transport.toggle_play", with: { "deck": "a" }))"#,
    ));
}

#[kithara::test]
#[case::query(
    "tree",
    r#"Tree(id: "browser", query: Parameter(id: "player.output.volume"))"#,
    "Text",
    "Scalar",
    "tree/browser"
)]
#[case::zoom(
    "deck",
    r#"Wave(id: "wave", zoom: Model(id: "library.breadcrumb"))"#,
    "Scalar",
    "Text",
    "deck/wave"
)]
#[case::scope(
    "library",
    r#"ContextBar(
        id: "context",
        scope_items: ["LOCAL"],
        scope: Model(id: "library.breadcrumb"),
        write: Parameter(id: "player.output.volume"),
    )"#,
    "Scalar",
    "Text",
    "library/context"
)]
#[case::read(
    "mixer",
    r#"Crossfader(
        id: "xfade",
        read: Model(id: "library.breadcrumb"),
        write: Parameter(id: "player.output.volume"),
    )"#,
    "Scalar",
    "Text",
    "mixer/xfade"
)]
#[case::status(
    "library",
    r#"Table(id: "tracks", status: Parameter(id: "player.output.volume"))"#,
    "Text",
    "Scalar",
    "library/tracks"
)]
fn a_read_slot_refuses_a_binding_of_another_kind(
    #[case] instance: &str,
    #[case] root: &str,
    #[case] expected_kind: &str,
    #[case] actual_kind: &str,
    #[case] at: &str,
) {
    let error = refused(module_as(instance, instance, root));

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == expected_kind && got == actual_kind && path == at),
        "{error:?}"
    );
}

#[kithara::test]
fn context_scope_items_require_scope_binding() {
    let error = refused(module_as(
        "library",
        "library",
        r#"ContextBar(id: "context", scope_items: ["LOCAL"])"#,
    ));

    assert!(
        matches!(&error, UiDocError::InvalidContextScope { path, .. } if path == "library/context"),
        "{error:?}"
    );
}

#[kithara::test]
fn missing_scope_is_reported() {
    let error = refused(module_root(
        r#"Button(id: "play", label: "PLAY",
            write: Command(id: "deck.transport.toggle_play"))"#,
    ));
    assert!(
        matches!(&error, UiDocError::MissingScope { scope, .. } if scope == "deck"),
        "{error:?}"
    );
}

#[kithara::test]
fn undeclared_command_scope_is_reported() {
    let error = refused(module_root(
        r#"Button(id: "play", label: "PLAY",
            write: Command(id: "deck.transport.toggle_play",
                with: { "deck": "a", "sidechain": "1" }))"#,
    ));
    assert!(
        matches!(&error, UiDocError::UnknownScope { id, scope, path, .. }
            if id == "deck.transport.toggle_play" && scope == "sidechain" && path == "demo/play"),
        "{error:?}"
    );
}

#[kithara::test]
fn scope_on_unscoped_parameter_is_reported() {
    let error = refused(module_root(
        r#"Fader(id: "volume",
            write: Parameter(id: "player.output.volume", with: { "deck": "a" }))"#,
    ));
    assert!(
        matches!(&error, UiDocError::UnknownScope { id, scope, path, .. }
            if id == "player.output.volume" && scope == "deck" && path == "demo/volume"),
        "{error:?}"
    );
}

#[kithara::test]
fn model_binding_on_write_side_is_direction_error() {
    let error = refused(module_root(
        r#"Button(id: "play", label: "PLAY", write: Model(id: "library.visible_tracks"))"#,
    ));
    assert!(
        matches!(error, UiDocError::BindingDirection { .. }),
        "{error:?}"
    );
}

/// A modal over a quiet surface, opening and shutting on state the view
/// keeps; `close` is what Escape writes.
fn modal(id: &str, close: &str, content: &str) -> String {
    format!(
        r#"Modal(id: "{id}", open: View(id: "settings"), close: {close},
            content: {content})"#
    )
}

const SHUT: &str = r#"View(id: "settings", set: Off)"#;
const QUIET: &str = r#"Spacer(id: "quiet", size: Some((w: Fixed(100.0), h: Fixed(60.0))))"#;

#[kithara::test]
fn a_modal_that_shuts_its_own_flag_compiles() {
    accepted(module_root(&modal("settings", SHUT, QUIET)));
}

#[kithara::test]
fn a_modal_must_close_through_a_binding_it_can_write() {
    let error = refused(module_root(&modal(
        "settings",
        r#"Model(id: "library.breadcrumb")"#,
        QUIET,
    )));

    assert!(
        matches!(&error, UiDocError::BindingDirection { path, .. } if path == "demo/settings"),
        "{error:?}"
    );
}

/// A popover opening on the same view flag the modal reads.
fn popover(id: &str, content: &str) -> String {
    format!(
        r#"Popover(id: "{id}", open: View(id: "settings"),
            anchor: Row(id: "{id}-anchor", children: []),
            content: {content})"#
    )
}

/// A modal or a popover nested in a modal, or a modal nested in a popover, is
/// refused at the inner one.
#[kithara::test]
fn a_surface_nested_in_a_modal_or_a_modal_in_a_popover_is_refused() {
    for (inner_id, outer) in [
        ("inner", modal("outer", SHUT, &modal("inner", SHUT, QUIET))),
        ("menu", modal("settings", SHUT, &popover("menu", QUIET))),
        ("settings", popover("menu", &modal("settings", SHUT, QUIET))),
    ] {
        let error = refused(module_root(&outer));

        assert!(
            matches!(&error, UiDocError::InvalidId { id, .. } if *id == format!("demo/{inner_id}")),
            "{inner_id}: {error:?}"
        );
    }
}

#[kithara::test]
fn a_node_the_hosts_draw_outside_the_flow_is_refused_inside_a_modal() {
    for (id, node) in [
        ("drag", r#"WindowDrag(id: "drag")"#),
        ("title", r#"TitleBar(id: "title", label: "SETTINGS")"#),
        ("controls", r#"WindowControls(id: "controls")"#),
        (
            "scope",
            r#"ContextBar(id: "scope",
                read: Model(id: "library.breadcrumb"),
                write: Command(id: "library.select_scope"),
                scope_items: ["ZVUK", "LOCAL"],
                scope: Model(id: "ui.measure"))"#,
        ),
    ] {
        accepted(module_root(node));
        let error = refused(module_root(&modal("settings", SHUT, node)));

        assert!(
            matches!(&error, UiDocError::InvalidId { id: path, .. } if *path == format!("demo/{id}")),
            "{id}: {error:?}"
        );
    }
}

#[kithara::test]
fn a_bound_colour_requires_a_palette_role() {
    accepted(module_root(
        r#"Text(id: "sample", label: "Sample", color: FromRead(read: Model(id: "ui.tint"), base: Accent))"#,
    ));
    for node in [
        r#"Text(
            id: "key",
            label: "4m",
            color: FromRead(read: Model(id: "ui.measure"), base: Accent),
        )"#,
        r#"Glyph(
            id: "icon",
            icon: Music2,
            color: FromRead(read: Model(id: "ui.measure"), base: Accent),
        )"#,
        r#"Row(
            id: "chip",
            active_background: FromRead(read: Model(id: "ui.measure"), base: Accent),
            children: [],
        )"#,
    ] {
        let error = refused(module_root(node));
        assert!(
            matches!(
                &error,
                UiDocError::BindingType { expected, got, .. }
                    if expected == "Color" && got == "Scalar"
            ),
            "{node}: {error:?}"
        );
    }
}
