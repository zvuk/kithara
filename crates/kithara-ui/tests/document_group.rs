//! What a group hands its host, measured where a host would see it.
//!
//! A group's face is resolved in the neutral facade, so the one place worth
//! testing is the argument every host receives. Neither host is involved here,
//! which is the point - a role dropped in the facade is dropped for both, and
//! a comparison between them cannot see it.
mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    expand::{Binding, ControlSpec},
    geom::Transform,
    ids::InternId,
    layout::Axis,
    module::MeasureAxis,
    render::{
        Clock, InputOwner, ReadValue, Reads,
        document::{
            Ctx, Group, GroupMount, Host, Measured, Module, PlacedMount, Popover, SplitMount,
            render,
        },
    },
    size::SizeSpec,
    skin::ColorRole,
    source::UiConfig,
    view,
};

/// The frame role one group was handed, in mount order.
struct Spy;

impl Spy {
    fn flatten<T>(groups: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
        groups.into_iter().flatten().collect()
    }
}

impl Host for Spy {
    type Output = Vec<ColorRole>;

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

    fn group(&mut self, group: Group<'_>, children: Vec<GroupMount<Self::Output>>) -> Self::Output {
        let mut mounted = Self::flatten(children.into_iter().map(|cell| cell.output));
        mounted.push(group.frame_color());
        mounted
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

    fn placed(&mut self, _placement: PlacedMount<'_>, child: Self::Output) -> Self::Output {
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

/// An application the fixture reads nothing from: a group's face is the
/// document's and the skin's, and neither is an endpoint.
struct Nothing;

impl Reads for Nothing {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

fn document(root: &str) -> CompiledUi {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "frame.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "frame-document",
            root: Module(instance: "page", source: "modules/frame.kmodule.ron"))"#,
    );
    resolver.insert(
        "modules/frame.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "frame", chrome: Plain, root: {root})"#
        ),
    );
    compile(
        "frame.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the fixture must compile: {error}"))
}

/// The frame role the one group in this document was handed.
fn frame_color(root: &str) -> ColorRole {
    let ui = document(root);
    let mounted = render(
        &ui.root,
        Ctx::new(
            &ui,
            &Nothing,
            &view::EMPTY,
            builtin::skin_doc(),
            Clock::default(),
        ),
        Spy,
    );
    match mounted.as_slice() {
        [only] => *only,
        other => panic!("the fixture must mount exactly one group, got {other:?}"),
    }
}

/// The frame side a group has to carry for its colour to mean anything.
const FRAMED: &str = "frame: (top: false, right: true, bottom: false, left: false)";

/// `Accent` rather than a line role: the skin's own divider is `LineInner`
/// (`kithara-dark.kskin.ron`), so a test naming a line role passes against a
/// facade that ignores the document entirely and pins nothing.
#[kithara::test]
fn a_column_frame_takes_the_colour_it_names() {
    let colour = frame_color(&format!(
        r#"Column(id: "flow", {FRAMED}, frame_color: Accent,
            children: [Spacer(id: "room", size: Some((w: Fill, h: Fill)))])"#
    ));

    assert_eq!(colour, ColorRole::Accent);
}

/// The sibling that already did, kept beside it so the two shapes are read
/// together rather than one being fixed to the other's shape by accident.
#[kithara::test]
fn a_row_frame_takes_the_colour_it_names() {
    let colour = frame_color(&format!(
        r#"Row(id: "flow", {FRAMED}, frame_color: Accent,
            children: [Spacer(id: "room", size: Some((w: Fill, h: Fill)))])"#
    ));

    assert_eq!(colour, ColorRole::Accent);
}

/// A group naming no colour takes the skin's divider, which is what makes the
/// two tests above about the document rather than about the default.
#[kithara::test]
fn a_column_naming_no_frame_colour_takes_the_skin_divider() {
    let colour = frame_color(&format!(
        r#"Column(id: "flow", {FRAMED},
            children: [Spacer(id: "room", size: Some((w: Fill, h: Fill)))])"#
    ));

    assert_eq!(colour, builtin::skin().divider.color);
}
