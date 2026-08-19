//! What an object does to the document, measured where a host would see it.
//!
//! The pose is resolved in the neutral facade, so the one place worth testing
//! is the argument every host receives: a control's transform. Neither host is
//! involved here, which is the point — if this is right, both draw it right.

mod common;

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    expand::{Binding, ControlSpec},
    geom::{Pt, Transform},
    ids::InternId,
    layout::Axis,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        Clock, InputOwner, ReadValue, Reads,
        document::{Ctx, Group, Host, Module, Popover, render},
    },
    size::SizeSpec,
    source::UiConfig,
};

/// One control, and where the document put what it draws.
#[derive(Debug, PartialEq)]
struct Placed {
    path: String,
    transform: Transform,
}

struct Spy<'a> {
    ui: &'a CompiledUi,
}

impl Spy<'_> {
    fn flatten<T>(groups: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
        groups.into_iter().flatten().collect()
    }
}

impl Host for Spy<'_> {
    type Output = Vec<Placed>;

    fn split(&mut self, _axis: Axis, children: Vec<(f32, SizeSpec, Self::Output)>) -> Self::Output {
        Self::flatten(children.into_iter().map(|(_, _, output)| output))
    }

    fn module(&mut self, _module: Module<'_>, content: Option<Self::Output>) -> Self::Output {
        content.unwrap_or_default()
    }

    fn group(
        &mut self,
        _group: Group<'_>,
        children: Vec<(Option<f32>, Self::Output)>,
    ) -> Self::Output {
        Self::flatten(children.into_iter().map(|(_, output)| output))
    }

    fn popover(
        &mut self,
        _popover: Popover,
        mut anchor: Self::Output,
        content: Option<Self::Output>,
    ) -> Self::Output {
        anchor.extend(content.unwrap_or_default());
        anchor
    }

    fn pressable(
        &mut self,
        _path: InternId,
        child: Self::Output,
        _size: Option<SizeSpec>,
    ) -> Self::Output {
        child
    }

    fn scroll(
        &mut self,
        _id: InternId,
        child: Self::Output,
        _size: Option<SizeSpec>,
    ) -> Self::Output {
        child
    }

    fn slot(&mut self, children: Vec<Self::Output>, _size: Option<SizeSpec>) -> Self::Output {
        Self::flatten(children)
    }

    fn stage(&mut self, children: Vec<Self::Output>, _size: Option<SizeSpec>) -> Self::Output {
        Self::flatten(children)
    }

    fn control(
        &mut self,
        path: InternId,
        _spec: &ControlSpec,
        _read: Option<&Binding>,
        _owner: InputOwner,
        _size: Option<SizeSpec>,
        transform: Transform,
    ) -> Self::Output {
        vec![Placed {
            path: self.ui.resolve(path).to_owned(),
            transform,
        }]
    }

    fn hosted(
        &mut self,
        _node: &kithara_ui::expand::ExpandedNode,
        child: Self::Output,
    ) -> Self::Output {
        child
    }

    fn window(
        &mut self,
        content: Self::Output,
        _dragged: Option<String>,
        _resize_edges: bool,
    ) -> Self::Output {
        content
    }
}

/// One scalar the document can be driven by, standing in for whatever the app
/// advances between frames. The same number answers both endpoints, so a
/// document that asks for seconds and one that asks for a phase differ in what
/// they asked for and in nothing else.
struct Phase(Option<f64>);

impl Reads for Phase {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "gallery.phase" | "gallery.clock" => self.0.map(ReadValue::Scalar),
            _ => None,
        }
    }
}

fn registry() -> common::TestRegistry {
    let mut registry = common::player_registry();
    for id in ["gallery.phase", "gallery.clock"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    registry
}

fn document(root: &str) -> CompiledUi {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "object.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "object-document",
            root: Module(instance: "page", source: "modules/object.kmodule.ron"))"#,
    );
    resolver.insert(
        "modules/object.kmodule.ron",
        &format!(r#"(schema: "kithara.module", version: 1, id: "object", root: {root})"#),
    );
    compile(
        "object.klayout.ron",
        &resolver,
        &registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_or_else(|error| panic!("the fixture must compile: {error}"))
}

fn placed(root: &str, reads: &Phase) -> Vec<Placed> {
    let ui = document(root);

    render(
        &ui.root,
        Ctx::new(&ui, reads, builtin::skin_doc(), Clock::default()),
        Spy { ui: &ui },
    )
}

fn only(root: &str, reads: &Phase) -> Transform {
    let placed = placed(root, reads);
    let [one] = placed.as_slice() else {
        panic!("the fixture mounts one control, not {placed:?}");
    };
    one.transform
}

const STILL: Phase = Phase(None);

#[kithara::test]
fn a_control_no_object_wraps_is_left_where_it_was() {
    let alone = only(r#"Text(id: "leaf")"#, &STILL);

    assert!(alone.is_identity());
}

#[kithara::test]
fn an_object_offsets_the_control_it_wraps() {
    let moved = only(
        r#"Object(id: "shift", transform: (position: (10.0, 4.0)), child: Text(id: "leaf"))"#,
        &STILL,
    );

    assert_eq!(moved, Transform::translate(Pt { x: 10.0, y: 4.0 }));
}

#[kithara::test]
fn nested_objects_compose_into_one_offset() {
    let moved = only(
        r#"Object(id: "outer", transform: (position: (10.0, 0.0)),
            child: Object(id: "inner", transform: (position: (0.0, 4.0)),
                child: Text(id: "leaf")))"#,
        &STILL,
    );

    assert_eq!(moved, Transform::translate(Pt { x: 10.0, y: 4.0 }));
}

/// A move applies to a whole subtree because every box in it shifts by the same
/// vector, so two siblings under one object carry the same offset rather than a
/// share of it.
#[kithara::test]
fn two_controls_under_one_moved_object_carry_the_same_offset() {
    let placed = placed(
        r#"Object(id: "shift", transform: (position: (10.0, 0.0)),
            child: Row(children: [Text(id: "one"), Text(id: "two")]))"#,
        &STILL,
    );

    let [first, second] = placed.as_slice() else {
        panic!("the fixture mounts two controls, not {placed:?}");
    };
    assert_eq!(first.transform, second.transform);
}

const TRACK: &str = r#"Object(id: "travel",
    to: (position: (100.0, 0.0)),
    phase: Model(id: "gallery.phase"),
    child: Text(id: "leaf"))"#;

#[kithara::test]
fn the_start_of_a_track_leaves_the_control_alone() {
    assert!(only(TRACK, &Phase(Some(0.0))).is_identity());
}

#[kithara::test]
fn the_end_of_a_track_puts_the_control_at_the_far_pose() {
    assert_eq!(
        only(TRACK, &Phase(Some(1.0))),
        Transform::translate(Pt { x: 100.0, y: 0.0 })
    );
}

/// The whole point of resolving the pose per frame: the same compiled document
/// draws in a different place when the endpoint behind it moves.
#[kithara::test]
fn moving_the_endpoint_moves_the_control() {
    assert_ne!(
        only(TRACK, &Phase(Some(0.25))),
        only(TRACK, &Phase(Some(0.75)))
    );
}

/// A track nobody drives is not half-applied: the object sits at the pose the
/// document wrote down, which is where it would sit with no track at all.
#[kithara::test]
fn a_track_with_no_answer_sits_at_its_written_pose() {
    assert!(only(TRACK, &STILL).is_identity());
}

/// The same journey the phase above makes, said the other way: four seconds
/// end to end, so the endpoint hands over a time and the document works out
/// how far along that puts it.
const RUN: &str = r#"Object(id: "travel",
    to: (position: (100.0, 0.0)),
    motion: (clock: Model(id: "gallery.clock"), duration: 4.0),
    child: Text(id: "leaf"))"#;

#[kithara::test]
fn a_motion_starts_where_the_document_wrote_it() {
    assert!(only(RUN, &Phase(Some(0.0))).is_identity());
}

/// The whole of what a motion adds: the endpoint says one second and the
/// document — not the application — decides that is a quarter of the way.
#[kithara::test]
fn a_motion_turns_seconds_into_the_distance_travelled() {
    assert_eq!(
        only(RUN, &Phase(Some(1.0))),
        Transform::translate(Pt { x: 25.0, y: 0.0 })
    );
}

#[kithara::test]
fn a_motion_that_runs_once_arrives_at_its_duration() {
    assert_eq!(
        only(RUN, &Phase(Some(4.0))),
        Transform::translate(Pt { x: 100.0, y: 0.0 })
    );
}

/// The settle, measured where it matters rather than in the arithmetic: a
/// clock that runs on for an hour leaves the object exactly where it arrived,
/// so a host redrawing it draws the same picture it drew at four seconds.
#[kithara::test]
fn a_motion_that_arrived_never_moves_again() {
    assert_eq!(
        only(RUN, &Phase(Some(4.0))),
        only(RUN, &Phase(Some(3600.0)))
    );
}

#[kithara::test]
fn a_motion_with_no_clock_answer_sits_at_its_written_pose() {
    assert!(only(RUN, &STILL).is_identity());
}

/// Frame stability, stated where a host would see it: a document with nothing
/// in motion mounts to the same offsets however far the clock has run, so a
/// page that declares no motion cannot be the reason a host keeps repainting.
#[kithara::test]
fn a_document_with_no_motion_mounts_the_same_at_any_time() {
    const STILL_PAGE: &str = r#"Row(children: [
        Text(id: "plain"),
        Object(id: "shift", transform: (position: (10.0, 4.0)), child: Text(id: "posed")),
    ])"#;

    assert_eq!(
        placed(STILL_PAGE, &Phase(Some(0.0))),
        placed(STILL_PAGE, &Phase(Some(3600.0)))
    );
}

/// And the same page with a motion in it does not, which is what makes the
/// test above a measurement rather than a tautology about the harness.
#[kithara::test]
fn a_document_with_a_motion_mounts_differently_as_the_clock_runs() {
    assert_ne!(
        placed(RUN, &Phase(Some(1.0))),
        placed(RUN, &Phase(Some(2.0)))
    );
}

/// What a host that keeps its tree across frames asks the compiled document,
/// so it can leave the poses alone entirely on a page that has none to move.
#[kithara::test]
fn a_document_that_places_nothing_off_an_endpoint_is_not_driven() {
    assert!(
        !document(
            r#"Object(id: "shift", transform: (position: (10.0, 4.0)), child: Text(id: "leaf"))"#
        )
        .driven
    );
}

/// A far pose nobody travels towards is not motion either: both ends and a
/// driver are what it takes to move, and two of the three leave the object
/// exactly where the document wrote it.
#[kithara::test]
fn a_far_pose_with_nothing_driving_it_is_not_driven() {
    assert!(
        !document(r#"Object(id: "half", to: (position: (100.0, 0.0)), child: Text(id: "leaf"))"#)
            .driven
    );
}

#[kithara::test]
fn a_document_with_a_phase_is_driven() {
    assert!(document(TRACK).driven);
}

#[kithara::test]
fn a_document_with_a_motion_is_driven() {
    assert!(document(RUN).driven);
}

/// The host's own clock, which no application declares and none has to answer.
const HOSTED: &str = r#"Object(id: "travel",
    to: (position: (100.0, 0.0)),
    motion: (clock: Model(id: "ui.clock.seconds"), duration: 4.0),
    child: Text(id: "leaf"))"#;

/// An application that answers nothing at all, so whatever moves below is the
/// host's clock rather than something the test quietly supplied.
struct Silent;

impl Reads for Silent {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

fn at(clock: Clock) -> Transform {
    let ui = document(HOSTED);
    let placed = render(
        &ui.root,
        Ctx::new(&ui, &Silent, builtin::skin_doc(), clock),
        Spy { ui: &ui },
    );
    let [one] = placed.as_slice() else {
        panic!("the fixture mounts one control, not {placed:?}");
    };
    one.transform
}

/// `registry()` declares only the gallery's own endpoints. Compiling a document
/// that binds to the host's clock is where the host's own declaration is proved.
#[kithara::test]
fn a_document_binds_to_the_host_clock_without_the_application_declaring_it() {
    let _ = document(HOSTED);
}

#[kithara::test]
fn an_application_that_answers_nothing_still_sees_the_motion_start_where_it_was_written() {
    assert!(at(Clock::default()).is_identity());
}

#[kithara::test]
fn running_the_host_clock_moves_an_object_the_application_knows_nothing_about() {
    assert_ne!(
        at(Clock::new(60, Duration::from_secs(1))),
        Transform::IDENTITY
    );
}

/// The determinism claim, stated as a test: the pose is a function of the clock
/// and nothing else, so the same reading twice draws the same frame.
#[kithara::test]
fn the_same_clock_twice_puts_the_object_at_the_same_pose() {
    let clock = Clock::new(90, Duration::from_millis(1500));
    assert_eq!(at(clock), at(clock));
}

/// Two hosts counting frames differently must still agree, because what the
/// document reads is the elapsed time and not the count beside it.
#[kithara::test]
fn the_frame_count_does_not_reach_the_document() {
    let elapsed = Duration::from_millis(1500);
    assert_eq!(at(Clock::new(90, elapsed)), at(Clock::new(37, elapsed)));
}
