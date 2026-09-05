#![cfg(feature = "render")]

mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    expand::{Binding, ControlSpec, ExpandedNode},
    geom::Transform,
    ids::InternId,
    layout::Axis,
    module::{MeasureAxis, WaveStyle},
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        InputOwner, Reads,
        document::{
            Clock, Ctx, Group, GroupMount, Host, Measured, Module, PlacedMount, Popover,
            SplitMount, render,
        },
    },
    size::SizeSpec,
    source::UiConfig,
    view,
};

fn builtin_layout() -> &'static str {
    r#"(
        schema: "kithara.layout",
        version: 1,
        id: "wave-document",
        root: Module(
            instance: "deck-a",
            source: "modules/wave-document.kmodule.ron",
            with: { "deck": "a" },
        ),
    )"#
}

fn studio_layout() -> &'static str {
    r#"(
        schema: "kithara.layout",
        version: 1,
        id: "wave-document",
        root: Module(
            instance: "deck-a",
            source: "modules/wave-document.kmodule.ron",
            with: { "deck": "a", "letter": "A" },
        ),
    )"#
}

struct EmptyReads;

impl Reads for EmptyReads {
    fn get(&self, _endpoint: &str) -> Option<kithara_ui::render::ReadValue<'_>> {
        None
    }
}

#[derive(Debug, PartialEq)]
struct MountedWave {
    owner: InputOwner,
    path: String,
    style: WaveStyle,
}

struct WaveHost<'a> {
    ui: &'a CompiledUi,
}

impl<'a> WaveHost<'a> {
    const fn new(ui: &'a CompiledUi) -> Self {
        Self { ui }
    }

    fn flatten<T>(groups: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
        groups.into_iter().flatten().collect()
    }
}

impl Host for WaveHost<'_> {
    type Output = Vec<MountedWave>;

    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        _read: Option<&Binding>,
        owner: InputOwner,
        _size: Option<SizeSpec>,
        _transform: Transform,
    ) -> Self::Output {
        match spec {
            ControlSpec::Wave { style, .. } => vec![MountedWave {
                owner,
                path: self.ui.resolve(path).to_owned(),
                style: *style,
            }],
            _ => Vec::new(),
        }
    }

    fn group(
        &mut self,
        _group: Group<'_>,
        children: Vec<GroupMount<Self::Output>>,
    ) -> Self::Output {
        Self::flatten(children.into_iter().map(|cell| cell.output))
    }

    fn hosted(&mut self, _node: &ExpandedNode, child: Self::Output) -> Self::Output {
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

    /// Counts what the document shows, so a shut surface contributes nothing.
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

fn studio_registry() -> common::TestRegistry {
    let mut registry = common::player_registry();
    for (category, id, value) in [
        (
            EndpointCategory::Command,
            "deck.transport.prev",
            ValueKind::Trigger,
        ),
        (
            EndpointCategory::Command,
            "deck.transport.next",
            ValueKind::Trigger,
        ),
        (
            EndpointCategory::Command,
            "deck.queue.load",
            ValueKind::Trigger,
        ),
        (
            EndpointCategory::Telemetry,
            "deck.playback.bpm",
            ValueKind::Text,
        ),
        (
            EndpointCategory::Telemetry,
            "deck.playback.remain",
            ValueKind::Text,
        ),
        (EndpointCategory::Telemetry, "deck.focused", ValueKind::Bool),
        (
            EndpointCategory::Telemetry,
            "deck.playback.position_secs",
            ValueKind::Scalar,
        ),
        (
            EndpointCategory::Telemetry,
            "deck.track.title",
            ValueKind::Text,
        ),
        (
            EndpointCategory::Parameter,
            "deck.tempo.rate",
            ValueKind::Scalar,
        ),
        (EndpointCategory::Model, "deck.view.zoom", ValueKind::Scalar),
        (EndpointCategory::Model, "ui.drag.over", ValueKind::Bool),
    ] {
        registry.insert(category, id, EndpointDesc::new(value).with_scope("deck"));
    }
    registry
}

fn mounted_wave(module: &str, source: &str, layout: &str, studio: bool) -> Vec<MountedWave> {
    let mut resolver = builtin::resolver();
    resolver.insert("wave-document.klayout.ron", layout);
    resolver.insert("modules/wave-document.kmodule.ron", source);
    let registry = if studio {
        studio_registry()
    } else {
        common::player_registry()
    };
    let ui = compile(
        "wave-document.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("{module} must compile: {error}"));

    render(
        &ui.root,
        Ctx::new(
            &ui,
            &EmptyReads,
            &view::EMPTY,
            builtin::skin_doc(),
            Clock::default(),
        ),
        WaveHost::new(&ui),
    )
}

#[kithara::test]
fn all_four_shipped_wave_documents_mount_through_the_neutral_facade() {
    let cases = [
        (
            "deck",
            include_str!("../assets/modules/deck.kmodule.ron"),
            builtin_layout(),
            &[("deck-a/wave", WaveStyle::Hero)][..],
            InputOwner::Leaf,
            false,
        ),
        // The micro player stacks the bar over the deck, so it carries the two
        // waves those two documents own rather than one of its own.
        (
            "deck-micro",
            include_str!("../assets/modules/deck-micro.kmodule.ron"),
            builtin_layout(),
            &[
                ("deck-a/bar/wave", WaveStyle::Micro),
                ("deck-a/deck/wave", WaveStyle::Hero),
            ][..],
            InputOwner::Leaf,
            false,
        ),
        (
            "app-deck",
            include_str!("../../kithara-app/assets/ui/modules/app-deck.kmodule.ron"),
            studio_layout(),
            &[("deck-a/wave", WaveStyle::Hero)][..],
            InputOwner::Engine,
            true,
        ),
        (
            "deck-overview-row",
            include_str!("../assets/modules/deck/overview-row.kmodule.ron"),
            studio_layout(),
            &[("deck-a/wave", WaveStyle::Default)][..],
            InputOwner::Engine,
            true,
        ),
    ];

    for (module, source, layout, waves, owner, studio) in cases {
        let expected: Vec<MountedWave> = waves
            .iter()
            .map(|(path, style)| MountedWave {
                owner,
                path: (*path).to_owned(),
                style: *style,
            })
            .collect();

        assert_eq!(
            mounted_wave(module, source, layout, studio),
            expected,
            "{module} must mount its Waves through render::document",
        );
    }
}
