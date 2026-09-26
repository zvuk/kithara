use std::{cell::Cell, collections::BTreeMap, sync::LazyLock};

use kithara_ui::{
    app::Config,
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Rect, Rgba},
    ids::EndpointId,
    module::IconName,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        CustomSkin, PortalMapView, PortalTarget, ReadValue, Reads, ScalarRange, StereoLevels,
        TableCell, TableRow, TreeRow, UiEvent, WaveBucket, WaveformView,
        custom::{CustomKinds, CustomWidget, Size2, SizeLimits, TextMeasurer},
    },
    source::{MemResolver, UiConfig},
    view,
};

use crate::scenarios::consts;

/// The readings every census control binds to, all moved by one call so a
/// data-change frame is a frame the reading really did move under.
pub(crate) struct CensusReads {
    flag: Cell<bool>,
    level: Cell<f32>,
    scalar: Cell<f64>,
    text: Cell<&'static str>,
}

impl Default for CensusReads {
    fn default() -> Self {
        Self {
            flag: Cell::new(true),
            level: Cell::new(0.4),
            scalar: Cell::new(0.25),
            text: Cell::new("All Tracks"),
        }
    }
}

impl CensusReads {
    pub(crate) fn bump(&self) {
        let scalar = self.scalar.get();
        self.scalar
            .set(if scalar > 0.9 { 0.1 } else { scalar + 0.05 });
        let level = self.level.get();
        self.level.set(if level > 0.9 { 0.1 } else { level + 0.05 });
        self.flag.set(!self.flag.get());
        self.text.set(if self.flag.get() {
            "All Tracks"
        } else {
            "Folder"
        });
    }
}

impl Reads for CensusReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        static PORTALS: [PortalTarget; 2] = [
            PortalTarget {
                bpm: 93.0,
                is_selected: true,
            },
            PortalTarget {
                bpm: 165.33,
                is_selected: false,
            },
        ];

        static WAVE: [WaveBucket; 4] = [WaveBucket {
            high: 0.8,
            low: 0.2,
            mid: 0.5,
        }; 4];

        static TREE_ROWS: [TreeRow<'static>; 8] = [TreeRow {
            label: "Folder",
            count: Some(8),
            expanded: Some(true),
            icon: IconName::Folder,
            muted: false,
            selected: false,
            depth: 0,
        }; 8];

        static TABLE_ROWS: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
            vec![TableRow::new(vec![TableCell::text("title", "Midnight Circuit")], false); 8]
        });

        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "deck.playback.tempo" | "deck.track.title" | "library.breadcrumb" | "library.query" => {
                Some(ReadValue::Text(self.text.get()))
            }
            "deck.playback.playing"
            | "deck.playback.looping"
            | "deck.playback.synced"
            | "ui.menu.open" => Some(ReadValue::Bool(self.flag.get())),
            "deck.playback.reverse" => Some(ReadValue::Bool(!self.flag.get())),
            "deck.playback.position_normalized"
            | "deck.view.zoom"
            | "library.scope"
            | "player.output.volume"
            | "vis.preset" => Some(ReadValue::Scalar(self.scalar.get())),
            "library.tree" => Some(ReadValue::Tree(&TREE_ROWS)),
            "library.visible_tracks" => Some(ReadValue::Table(&TABLE_ROWS[..])),
            "deck.playback.waveform" | "demo.wave" => Some(ReadValue::Waveform(WaveformView {
                beats: &[],
                buckets: &WAVE,
                revision: 0,
                cues: &[],
                downbeats: &[],
                unready: &[],
                bpm: None,
                r#loop: None,
            })),
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: self.level.get(),
                r: 1.0 - self.level.get(),
                volume: self.level.get(),
            })),
            "pivot.map" => Some(ReadValue::PortalMap(PortalMapView {
                master: 88.0 + self.level.get() * 88.0,
                min: 88.0,
                max: 176.0,
                targets: &PORTALS,
            })),
            "pivot.range" => Some(ReadValue::Range(ScalarRange {
                min: self.level.get() * 0.4,
                max: 0.6 + self.level.get() * 0.4,
            })),
            _ => None,
        }
    }
}

#[derive(Default)]
struct CensusRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl CensusRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for CensusRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

fn census_registry() -> CensusRegistry {
    let mut registry = CensusRegistry::default();
    for id in [
        "deck.transport.jump_back",
        "deck.transport.jump_forward",
        "deck.transport.set_cue",
        "deck.transport.toggle_loop",
        "deck.transport.toggle_play",
        "deck.transport.toggle_reverse",
        "deck.transport.toggle_sync",
        "deck.view.zoom_in",
        "deck.view.zoom_out",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.seek_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    for id in [
        "deck.playback.looping",
        "deck.playback.playing",
        "deck.playback.reverse",
        "deck.playback.synced",
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
        );
    }
    for id in ["deck.playback.tempo", "deck.track.title"] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Text).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.position_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.waveform",
        EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "player.output.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "player.output.volume",
        EndpointDesc::new(ValueKind::Scalar),
    );
    models(&mut registry);
    registry
}

fn models(registry: &mut CensusRegistry) {
    for (id, kind) in [
        ("deck.view.zoom", ValueKind::Scalar),
        ("library.breadcrumb", ValueKind::Text),
        ("library.query", ValueKind::Text),
        ("library.scope", ValueKind::Scalar),
        ("library.tree", ValueKind::Tree),
        ("library.visible_tracks", ValueKind::Table),
        ("demo.wave", ValueKind::Waveform),
        ("pivot.map", ValueKind::PortalMap),
        ("ui.menu.open", ValueKind::Bool),
        ("vis.preset", ValueKind::Scalar),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
    // A range reads the whole interval and writes one end of it, so the two
    // halves of its contract are two kinds under one name.
    registry.insert(
        EndpointCategory::Model,
        "pivot.range",
        EndpointDesc::new(ValueKind::Range),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "pivot.range",
        EndpointDesc::new(ValueKind::Scalar),
    );
}

/// Everything a host is handed that is not the host: the documents, the
/// endpoints they may bind to, and nothing that differs between the two.
pub(crate) struct Fixture {
    registry: CensusRegistry,
    kinds: CustomKinds,
    resolver: MemResolver,
}

struct CensusExtension;

impl CustomWidget for CensusExtension {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        bounds: Rect,
        _skin: &CustomSkin,
    ) {
        list.fill_rect(
            bounds,
            Rgba {
                a: 1.0,
                b: 1.0,
                g: 1.0,
                r: 1.0,
            },
        );
    }
}

pub(crate) fn census_kinds() -> CustomKinds {
    /// The kind the `Custom` scenario names, so what is measured is the mount path
    /// through a registered widget rather than the empty box a host falls to when
    /// the registry does not hold the name.
    const KIND: &str = "census-extension";

    CustomKinds::default().with(KIND, || CensusExtension, |()| UiEvent::OpenSettings)
}

impl Fixture {
    const MODULE: &str = "fixture.kmodule.ron";

    /// The one source the census table names beside a control. Only the shader
    /// row asks for it.
    const SOURCES: &[(&str, &str)] = &[(
        "census.wgsl",
        r"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(kithara.level.x, position.x / kithara.viewport.x, 0.0, 1.0);
}
",
    )];

    pub(crate) fn new(control: &str) -> Self {
        const LAYOUT_RON: &str = r#"(schema: "kithara.layout", version: 1, id: "fixture",
    root: Module(instance: "demo", source: "fixture.kmodule.ron", size: (w: Fill, h: Fill)))"#;

        let mut resolver = MemResolver::default();
        resolver.insert(consts::LAYOUT, LAYOUT_RON);
        resolver.insert(
            Self::MODULE,
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "census", chrome: Plain,
                    root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}]))"#
            ),
        );
        for (name, body) in Self::SOURCES {
            resolver.insert(name, body);
        }
        Self {
            resolver,
            kinds: census_kinds(),
            registry: census_registry(),
        }
    }

    pub(crate) fn compiled(&self) -> CompiledUi {
        compile(
            consts::LAYOUT,
            &self.resolver,
            &self.registry,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::builder().custom_kinds(self.kinds.names()).build(),
            &view::EMPTY,
        )
        .unwrap_or_else(|error| panic!("the frame-perf fixture must compile: {error}"))
    }

    pub(crate) fn config(&self) -> Config<'_> {
        Config::builder()
            .endpoints(&self.registry)
            .resolver(&self.resolver)
            .text(builtin::text_doc())
            .kinds(&self.kinds)
            .build()
    }
}
