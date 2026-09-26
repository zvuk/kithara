//! Which branch of a self-measured node stands, and which cell of a
//! measuring container, asked of what the host is handed.
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::compile,
    expand::{Binding, ControlSpec},
    geom::Transform,
    ids::InternId,
    layout::Axis,
    module::MeasureAxis,
    render::{
        Clock, InputOwner, ReadValue, Reads,
        document::{
            Band, Ctx, Group, GroupMount, Host, Measured, Module, PlacedMount, Popover, SplitMount,
            render,
        },
    },
    size::SizeSpec,
    source::UiConfig,
    view,
};

/// Every measured plan the host was handed, in mount order.
struct Spy;

impl Spy {
    fn flatten<T>(groups: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
        groups.into_iter().flatten().collect()
    }
}

impl Host for Spy {
    type Output = Vec<Measured>;

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

    fn measured(&mut self, plan: Measured, branches: Vec<Self::Output>) -> Self::Output {
        let mut mounted = Self::flatten(branches);
        mounted.push(plan);
        mounted
    }

    fn module(&mut self, _module: Module<'_>, content: Option<Self::Output>) -> Self::Output {
        content.unwrap_or_default()
    }

    fn placed(&mut self, _placement: PlacedMount<'_>, child: Self::Output) -> Self::Output {
        child
    }

    fn popover(
        &mut self,
        _popover: Popover<'_>,
        anchor: Self::Output,
        _content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) -> Self::Output {
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

/// An application with nothing to read: a bank measuring its own box reads
/// no endpoint.
struct Unanswered;

impl Reads for Unanswered {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

/// A bank measuring its own width, with a branch from 100 and one from 200.
fn measured() -> Measured {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "bank.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "bank-document",
            root: Module(instance: "page", source: "modules/bank.kmodule.ron"))"#,
    );
    resolver.insert(
        "modules/bank.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "bank",
            root: Adaptive(
                id: "bank",
                measure: Width,
                size: Some((w: Fill, h: Shrink)),
                base: Knob(id: "low"),
                steps: [
                    (from: 100.0, node: Knob(id: "mid")),
                    (from: 200.0, node: Knob(id: "high")),
                ],
            ))"#,
    );
    let ui = compile(
        "bank.klayout.ron",
        &resolver,
        &crate::common::registry::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the fixture must compile: {error}"));
    let mut plans = render(
        &ui.root,
        Ctx::new(
            &ui,
            &Unanswered,
            &view::EMPTY,
            builtin::skin_doc(),
            Clock::default(),
        ),
        Spy,
    );
    let (Some(plan), true) = (plans.pop(), plans.is_empty()) else {
        panic!("the bank must hand its host exactly one measured plan");
    };
    plan
}

/// A threshold is reached at its own value, not past it.
#[kithara::test]
fn a_branch_stands_from_the_room_it_names() {
    let measured = measured();

    assert_eq!(measured.branch(99.0), 0);
    assert_eq!(measured.branch(100.0), 1);
    assert_eq!(measured.branch(199.0), 1);
    assert_eq!(measured.branch(200.0), 2);
}

/// An axis nobody bounded takes the last branch, which is the widest one.
#[kithara::test]
fn an_unbounded_axis_takes_the_last_branch() {
    assert_eq!(measured().branch(f32::INFINITY), 2);
}

/// A cell that names no band never goes away.
#[kithara::test]
fn an_unnamed_band_stands_in_every_room() {
    assert!(Band::ALWAYS.stands(0.0));
    assert!(Band::ALWAYS.stands(f32::INFINITY));
}

/// A band starts at the room it names and stops below its ceiling.
#[kithara::test]
fn a_band_starts_at_its_floor() {
    let band = Band::new(100.0, Some(200.0));

    assert!(!band.stands(99.0));
    assert!(band.stands(100.0));
}

#[kithara::test]
fn a_band_stops_below_its_ceiling() {
    let band = Band::new(100.0, Some(200.0));

    assert!(band.stands(199.0));
    assert!(!band.stands(200.0));
}
