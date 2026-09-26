//! What a document is refused for, asked of the compiler that refuses it.

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    source::{MemResolver, UiConfig},
    view,
};

use crate::common::registry::TestRegistry;

const LAYOUT: &str = "validate.klayout.ron";
const MODULE: &str = "m.ron";

/// The endpoints the documents below bind, and nothing else.
fn registry() -> TestRegistry {
    let mut registry = TestRegistry::default();
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
fn duplicate_instance_reports_path() {
    let error = refused(layout_root(
        r#"Split(axis: Horizontal, children: [
            (node: Module(instance: "deck-a", source: "m.ron")),
            (node: Module(instance: "deck-a", source: "m.ron")),
        ])"#,
    ));
    let message = error.to_string();
    assert!(message.contains("deck-a"), "{message}");
    assert!(message.contains("Split[1]"), "{message}");
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
fn a_measuring_split_must_declare_the_axis_it_measures() {
    for head in [
        "measure: Width,",
        "measure: Height,",
        "measure: Width, size: (w: Shrink, h: Fill),",
        "measure: Height, size: (w: Fill, h: Shrink),",
    ] {
        let error = refused(split_cell(head, ""));
        assert!(
            matches!(&error, UiDocError::UnmeasuredAxis { path, .. }
                if path == "root/Split"),
            "{head}: {error:?}",
        );
    }
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
fn duplicate_control_id_reports_path() {
    let error = refused(module_root(
        r#"Row(children: [
            Button(id: "play", label: "PLAY"),
            Button(id: "play", label: "PLAY"),
        ])"#,
    ));
    assert!(error.to_string().contains("Control(play)"), "{error}");
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
fn an_object_may_move_a_whole_row() {
    accepted(module_root(
        r#"Object(id: "shift", transform: (position: (8.0, 0.0)),
            child: Row(children: [Button(id: "play", label: "PLAY")]))"#,
    ));
}

#[kithara::test]
fn an_object_may_not_turn_a_row() {
    let error = refused(module_root(
        r#"Object(id: "spin", transform: (rotation: 30.0),
            child: Row(children: [Button(id: "play", label: "PLAY")]))"#,
    ));

    assert!(
        matches!(error, UiDocError::ObjectGroup { child: "Row", .. }),
        "{error:?}"
    );
}

#[kithara::test]
fn an_object_may_not_scale_a_row_either() {
    refused(module_root(
        r#"Object(id: "grow", transform: (scale: (2.0, 2.0)),
            child: Row(children: [Button(id: "play", label: "PLAY")]))"#,
    ));
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

/// A container the walk does not name would be validated as a leaf and its
/// children never looked at, so a duplicate under a stage is the proof the
/// walk goes in.
#[kithara::test]
fn a_stage_walks_its_children() {
    let error = refused(module_root(
        r#"Stage(id: "scene", children: [
            Button(id: "play", label: "PLAY"),
            Button(id: "play", label: "AGAIN"),
        ])"#,
    ));

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "play"),
        "{error:?}"
    );
}

/// Every child of a stage gets the whole box, so a stage is several boxes,
/// and a turn about one origin would take them apart.
#[kithara::test]
fn an_object_may_not_turn_a_stage() {
    let error = refused(module_root(
        r#"Object(id: "spin", transform: (rotation: 30.0),
            child: Stage(id: "scene", children: [Button(id: "play", label: "PLAY")]))"#,
    ));

    assert!(
        matches!(error, UiDocError::ObjectGroup { child: "Stage", .. }),
        "{error:?}"
    );
}

/// A move carries every box by the same vector, so it reaches a stage the
/// way it reaches a row.
#[kithara::test]
fn an_object_may_move_a_whole_stage() {
    accepted(module_root(
        r#"Object(id: "shift", transform: (position: (8.0, 0.0)),
            child: Stage(id: "scene", children: [Button(id: "play", label: "PLAY")]))"#,
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
fn adaptive_steps_must_climb() {
    for (steps, at) in [
        (
            r#"(from: 4.0, node: Knob(id: "a")), (from: 2.0, node: Knob(id: "b"))"#,
            1,
        ),
        (
            r#"(from: 4.0, node: Knob(id: "a")), (from: 4.0, node: Knob(id: "b"))"#,
            1,
        ),
        (r#"(from: NaN, node: Knob(id: "a"))"#, 0),
    ] {
        let error = refused(adaptive(steps));
        assert!(
            matches!(&error, UiDocError::AdaptiveStepOrder { index, .. } if *index == at),
            "{steps}: {error:?}"
        );
    }
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
fn a_self_measured_node_must_declare_the_axis_it_measures() {
    for (measure, size) in [
        ("Width", ""),
        ("Height", ""),
        ("Width", "size: Some((w: Shrink, h: Fill)),"),
        ("Height", "size: Some((w: Fill, h: Shrink)),"),
    ] {
        let error = refused(measured(measure, size));
        assert!(
            matches!(&error, UiDocError::UnmeasuredAxis { path, .. }
                if path == "root/Adaptive(bank)"),
            "{measure} {size}: {error:?}",
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
fn a_measuring_container_must_declare_the_axis_it_measures() {
    for (measure, size) in [
        ("Width", ""),
        ("Height", ""),
        ("Width", "size: (w: Shrink, h: Fill),"),
        ("Height", "size: (w: Fill, h: Shrink),"),
    ] {
        let root =
            format!(r#"Row(id: "bar", measure: {measure}, {size} children: [Knob(id: "a")])"#);
        let error = refused(module_root(&root));
        assert!(
            matches!(&error, UiDocError::UnmeasuredAxis { path, .. }
                if path == "root/Group(bar)"),
            "{root}: {error:?}",
        );
    }
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
fn a_duplicate_id_inside_one_adaptive_branch_is_rejected() {
    let error = refused(adaptive(
        r#"(from: 4.0, node: Row(children: [Knob(id: "dup"), Knob(id: "dup")]))"#,
    ));

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "dup"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_sibling_after_an_adaptive_node_sees_every_branch_id() {
    let error = refused(module_root(
        r#"Row(children: [
            Adaptive(
                id: "bank",
                measure: Read(Model(id: "ui.measure")),
                base: Knob(id: "low"),
                steps: [(from: 4.0, node: Knob(id: "low-mid"))],
            ),
            Knob(id: "low-mid"),
        ])"#,
    ));

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "low-mid"),
        "{error:?}"
    );
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
fn tree_query_binding_must_be_text() {
    let error = refused(module_as(
        "tree",
        "tree",
        r#"Tree(id: "browser", query: Parameter(id: "player.output.volume"))"#,
    ));

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Text" && got == "Scalar" && path == "tree/browser"),
        "{error:?}"
    );
}

#[kithara::test]
fn wave_zoom_binding_must_be_scalar() {
    let error = refused(module_as(
        "deck",
        "deck",
        r#"Wave(id: "wave", zoom: Model(id: "library.breadcrumb"))"#,
    ));

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Scalar" && got == "Text" && path == "deck/wave"),
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
fn context_scope_binding_must_be_scalar() {
    let error = refused(module_as(
        "library",
        "library",
        r#"ContextBar(
            id: "context",
            scope_items: ["LOCAL"],
            scope: Model(id: "library.breadcrumb"),
            write: Parameter(id: "player.output.volume"),
        )"#,
    ));

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Scalar" && got == "Text" && path == "library/context"),
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
fn crossfader_requires_scalar_read_and_write_endpoints() {
    let error = refused(module_as(
        "mixer",
        "mixer",
        r#"Crossfader(
            id: "xfade",
            read: Model(id: "library.breadcrumb"),
            write: Parameter(id: "player.output.volume"),
        )"#,
    ));

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Scalar" && got == "Text" && path == "mixer/xfade"),
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
