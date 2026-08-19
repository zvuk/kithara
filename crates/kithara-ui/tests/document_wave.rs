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
    module::WaveStyle,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        InputOwner, Reads,
        document::{Clock, Ctx, Group, Host, Module, Popover, render},
    },
    size::SizeSpec,
    source::UiConfig,
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

    fn hosted(&mut self, _node: &ExpandedNode, child: Self::Output) -> Self::Output {
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
    )
    .unwrap_or_else(|error| panic!("{module} must compile: {error}"));

    render(
        &ui.root,
        Ctx::new(&ui, &EmptyReads, builtin::skin_doc(), Clock::default()),
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
            WaveStyle::Hero,
            InputOwner::Leaf,
            false,
        ),
        (
            "deck-micro",
            include_str!("../assets/modules/deck-micro.kmodule.ron"),
            builtin_layout(),
            WaveStyle::Micro,
            InputOwner::Leaf,
            false,
        ),
        (
            "app-deck",
            include_str!("../../kithara-app/assets/ui/modules/app-deck.kmodule.ron"),
            studio_layout(),
            WaveStyle::Hero,
            InputOwner::Engine,
            true,
        ),
        (
            "app-overview-row",
            include_str!("../../kithara-app/assets/ui/modules/app-overview-row.kmodule.ron"),
            studio_layout(),
            WaveStyle::Default,
            InputOwner::Engine,
            true,
        ),
    ];

    for (module, source, layout, style, owner, studio) in cases {
        assert_eq!(
            mounted_wave(module, source, layout, studio),
            [MountedWave {
                owner,
                path: "deck-a/wave".to_owned(),
                style,
            }],
            "{} must mount its Wave through render::document",
            module,
        );
    }
}
