//! What a placement hands its host, measured where a host would see it.
//!
//! The point and the magnet are resolved in the neutral facade, so the one
//! place worth testing is the argument every host receives. Neither host is
//! involved here, which is the point - if this is right, both put it in the
//! same place and both pull it to the same target.
mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    expand::{Binding, ControlSpec},
    geom::{Pt, Transform},
    ids::InternId,
    layout::Axis,
    module::MeasureAxis,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        Clock, InputOwner, ReadValue, Reads,
        document::{
            Ctx, Group, GroupMount, Host, Measured, Module, PlacedMount, Popover, SplitMount,
            render,
        },
    },
    size::SizeSpec,
    source::UiConfig,
    view,
};

/// One placement, as the host was handed it.
#[derive(Debug, PartialEq)]
struct Mounted {
    /// The magnet as the host receives it: where it may pull to, and how near
    /// the drag has to come. `Snap` is not built outside the toolkit, so the
    /// test reads the two things it carries.
    snap: Option<(Vec<Pt>, f32)>,
    at: Pt,
    path: String,
    carried: bool,
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
    type Output = Vec<Mounted>;

    fn control(
        &mut self,
        _path: InternId,
        _spec: &ControlSpec,
        _read: Option<&Binding>,
        _owner: InputOwner,
        _size: Option<SizeSpec>,
        _transform: Transform,
    ) -> Self::Output {
        Vec::new()
    }

    fn group(
        &mut self,
        _group: Group<'_>,
        children: Vec<GroupMount<Self::Output>>,
    ) -> Self::Output {
        Self::flatten(children.into_iter().map(|cell| cell.output))
    }

    fn hosted(
        &mut self,
        _node: &kithara_ui::expand::ExpandedNode,
        child: Self::Output,
    ) -> Self::Output {
        child
    }

    fn measured(&mut self, _plan: Measured, branches: Vec<Self::Output>) -> Self::Output {
        Self::flatten(branches)
    }

    fn module(&mut self, _module: Module<'_>, content: Option<Self::Output>) -> Self::Output {
        content.unwrap_or_default()
    }

    fn placed(&mut self, placement: PlacedMount<'_>, mut child: Self::Output) -> Self::Output {
        child.push(Mounted {
            path: self.ui.resolve(placement.path).to_owned(),
            at: placement.at,
            carried: placement.write.is_some(),
            snap: placement
                .snap
                .as_ref()
                .map(|snap| (snap.to.clone(), snap.within)),
        });
        child
    }

    fn popover(
        &mut self,
        popover: Popover<'_>,
        mut anchor: Self::Output,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) -> Self::Output {
        if popover.is_open() {
            anchor.extend(content(self));
        }
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

    fn slot(
        &mut self,
        children: Vec<GroupMount<Self::Output>>,
        _size: Option<SizeSpec>,
    ) -> Self::Output {
        Self::flatten(children.into_iter().map(|cell| cell.output))
    }

    fn split(
        &mut self,
        _axis: Axis,
        _measure: Option<MeasureAxis>,
        children: Vec<SplitMount<Self::Output>>,
    ) -> Self::Output {
        Self::flatten(children.into_iter().map(|cell| cell.output))
    }

    fn stage(&mut self, children: Vec<Self::Output>, _size: Option<SizeSpec>) -> Self::Output {
        Self::flatten(children)
    }

    fn window(
        &mut self,
        content: Self::Output,
        _carried: Option<&Binding>,
        _resize_edges: bool,
    ) -> Self::Output {
        content
    }
}

/// Where the application says each carried placement now stands. A point the
/// application does not answer leaves the document's own written point to
/// stand, which is what an unanswered endpoint means everywhere else.
struct Points {
    one: Option<Pt>,
    two: Option<Pt>,
}

impl Reads for Points {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "scene.one" => self.one.map(ReadValue::Point),
            "scene.two" => self.two.map(ReadValue::Point),
            _ => None,
        }
    }
}

const UNANSWERED: Points = Points {
    one: None,
    two: None,
};

fn registry() -> common::TestRegistry {
    let mut registry = common::player_registry();
    for id in ["scene.one", "scene.two"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Point),
        );
        registry.insert(
            EndpointCategory::Parameter,
            id,
            EndpointDesc::new(ValueKind::Point),
        );
    }
    registry
}

fn document(children: &str) -> CompiledUi {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "scene.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "scene-document",
            root: Module(instance: "page", source: "modules/scene.kmodule.ron"))"#,
    );
    resolver.insert(
        "modules/scene.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "scene",
            root: Stage(id: "stage", children: [{children}]))"#
        ),
    );
    compile(
        "scene.klayout.ron",
        &resolver,
        &registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the fixture must compile: {error}"))
}

fn mounted(children: &str, reads: &Points) -> Vec<Mounted> {
    let ui = document(children);

    render(
        &ui.root,
        Ctx::new(
            &ui,
            reads,
            &view::EMPTY,
            builtin::skin_doc(),
            Clock::default(),
        ),
        Spy { ui: &ui },
    )
}

fn one(children: &str, reads: &Points, path: &str) -> Mounted {
    let mut mounted = mounted(children, reads);
    let found = mounted.iter().position(|placement| placement.path == path);
    let Some(found) = found else {
        panic!("the fixture must mount `{path}`, not {mounted:?}");
    };
    mounted.swap_remove(found)
}

/// A dock and a sprite that snaps onto it, which is the smallest scene that
/// has something to pull and something to pull to.
const SCENE: &str = r#"
    Placed(id: "dock", at: (200.0, 100.0), child: Knob(id: "mark")),
    Placed(id: "carry", at: (16.0, 32.0),
        read: Model(id: "scene.one"),
        write: Parameter(id: "scene.one"),
        magnet: (to: ["dock"], within: 64.0),
        child: Knob(id: "sprite"))"#;

#[kithara::test]
fn a_placement_nothing_answers_for_stands_where_the_document_wrote_it() {
    assert_eq!(
        one(SCENE, &UNANSWERED, "page/carry").at,
        Pt { x: 16.0, y: 32.0 }
    );
}

/// The point is the application's, so the compiled document stands wherever
/// the endpoint now says rather than where it was written.
#[kithara::test]
fn a_placement_stands_at_the_point_its_endpoint_answers() {
    let reads = Points {
        one: Some(Pt { x: 120.0, y: 44.0 }),
        two: None,
    };

    assert_eq!(
        one(SCENE, &reads, "page/carry").at,
        Pt { x: 120.0, y: 44.0 }
    );
}

/// A placement with somewhere to write is one the pointer may carry.
#[kithara::test]
fn a_placement_with_somewhere_to_write_is_carried() {
    assert!(one(SCENE, &UNANSWERED, "page/carry").carried);
}

/// A placement the document gave nowhere to write stands where it was put.
#[kithara::test]
fn a_placement_with_nowhere_to_write_is_not_carried() {
    assert!(!one(SCENE, &UNANSWERED, "page/dock").carried);
}

#[kithara::test]
fn a_placement_without_a_magnet_takes_no_snap() {
    assert_eq!(one(SCENE, &UNANSWERED, "page/dock").snap, None);
}

/// A magnet names placements, and what reaches the host is where those
/// placements stand.
#[kithara::test]
fn a_magnet_reaches_the_host_as_the_points_it_names() {
    assert_eq!(
        one(SCENE, &UNANSWERED, "page/carry").snap,
        Some((vec![Pt { x: 200.0, y: 100.0 }], 64.0))
    );
}

/// A target that moves takes its magnet with it: the snap is worked out from
/// where the named placement stands this frame, not where the document wrote
/// it.
#[kithara::test]
fn a_magnet_follows_the_target_it_names() {
    const MOVING_TARGET: &str = r#"
        Placed(id: "dock", at: (200.0, 100.0),
            read: Model(id: "scene.two"),
            write: Parameter(id: "scene.two"),
            child: Knob(id: "mark")),
        Placed(id: "carry", at: (16.0, 32.0),
            read: Model(id: "scene.one"),
            write: Parameter(id: "scene.one"),
            magnet: (to: ["dock"], within: 64.0),
            child: Knob(id: "sprite"))"#;
    let reads = Points {
        one: None,
        two: Some(Pt { x: 300.0, y: 8.0 }),
    };

    assert_eq!(
        one(MOVING_TARGET, &reads, "page/carry").snap,
        Some((vec![Pt { x: 300.0, y: 8.0 }], 64.0))
    );
}

/// A magnet names an array, so a scene with several targets hands the host
/// every one of them, in the order the document named them.
#[kithara::test]
fn a_magnet_naming_several_targets_hands_over_all_of_them() {
    const THREE: &str = r#"
        Placed(id: "dock-left", at: (0.0, 0.0), child: Knob(id: "left")),
        Placed(id: "dock-right", at: (400.0, 0.0), child: Knob(id: "right")),
        Placed(id: "carry", at: (16.0, 32.0),
            read: Model(id: "scene.one"),
            write: Parameter(id: "scene.one"),
            magnet: (to: ["dock-right", "dock-left"], within: 64.0),
            child: Knob(id: "sprite"))"#;

    assert_eq!(
        one(THREE, &UNANSWERED, "page/carry").snap,
        Some((vec![Pt { x: 400.0, y: 0.0 }, Pt { x: 0.0, y: 0.0 }], 64.0))
    );
}
