use std::{collections::BTreeMap, sync::LazyLock};

use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::{CompiledUi, compile},
    ids::{EndpointId, SourceUri},
    module::IconName,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        PortalMapView, PortalTarget, ReadValue, Reads, ScalarRange, Skin, StereoLevels, TableCell,
        TableRow, TreeRow, UiEvent, WaveBucket, WaveformView, custom::CustomKinds,
    },
    shaping::FontPolicy,
    source::{MemResolver, UiConfig},
    view,
};

use super::table::{CENSUS_KIND, CENSUS_SOURCES, census_kinds};

/// The window every census row is mounted in.
pub(crate) const WIDTH: u32 = 240;
pub(crate) const HEIGHT: u32 = 120;

/// The path the document gives the one control a row mounts.
pub(crate) const CONTROL: &str = "demo/control";

/// The layout every row is mounted through.
const LAYOUT: &str = "fixture.klayout.ron";

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

static WAVE: [WaveBucket; 1] = [WaveBucket {
    high: 0.8,
    low: 0.2,
    mid: 0.5,
}];

/// More rows than the window holds, so a table has something under the hand
/// and something left to scroll to.
static TABLE_ROWS: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![
        TableRow::new(
            vec![
                TableCell::text("title", "Late Arrival"),
                TableCell::text("artist", "New Artist"),
                TableCell::text("bpm", "128"),
                TableCell::number("energy", 7),
                TableCell::text("key", "Am"),
                TableCell::text("time", "03:24"),
            ],
            false,
        );
        8
    ]
});

static TREE_ROWS: [TreeRow<'static>; 8] = [TreeRow {
    label: "Late Folder",
    count: Some(8),
    expanded: Some(true),
    icon: IconName::Folder,
    muted: false,
    selected: false,
    depth: 0,
}; 8];

/// The readings a census row needs to have anything to draw and anything
/// under the hand.
pub(crate) struct CensusReads;

impl Reads for CensusReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "ui.menu.open" => Some(ReadValue::Bool(true)),
            "deck.view.zoom" => Some(ReadValue::Scalar(0.25)),
            "library.breadcrumb" => Some(ReadValue::Text("All Tracks")),
            "library.query" => Some(ReadValue::Text("Folder")),
            "library.scope" => Some(ReadValue::Scalar(0.0)),
            "library.tree" => Some(ReadValue::Tree(&TREE_ROWS)),
            "library.visible_tracks" => Some(ReadValue::Table(&TABLE_ROWS)),
            "vis.preset" => Some(ReadValue::Scalar(1.0)),
            "demo.wave" => Some(ReadValue::Waveform(WaveformView {
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
                l: 0.6,
                r: 0.4,
                volume: 0.8,
            })),
            "player.output.volume" => Some(ReadValue::Scalar(0.8)),
            "pivot.map" => Some(ReadValue::PortalMap(PortalMapView {
                master: 124.0,
                min: 88.0,
                max: 176.0,
                targets: &PORTALS,
            })),
            "pivot.range" => Some(ReadValue::Range(ScalarRange { min: 0.2, max: 0.8 })),
            _ => None,
        }
    }
}

/// The endpoints the census rows bind, and nothing else.
struct CensusRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl Default for CensusRegistry {
    fn default() -> Self {
        let endpoints = [
            (EndpointCategory::Model, "deck.view.zoom", ValueKind::Scalar),
            (EndpointCategory::Model, "demo.wave", ValueKind::Waveform),
            (
                EndpointCategory::Model,
                "library.breadcrumb",
                ValueKind::Text,
            ),
            (EndpointCategory::Model, "library.query", ValueKind::Text),
            (EndpointCategory::Model, "library.scope", ValueKind::Scalar),
            (EndpointCategory::Model, "library.tree", ValueKind::Tree),
            (
                EndpointCategory::Model,
                "library.visible_tracks",
                ValueKind::Table,
            ),
            (EndpointCategory::Model, "pivot.map", ValueKind::PortalMap),
            // A range reads the whole interval and writes one end of it, so the
            // two halves of its contract are two kinds under one name.
            (EndpointCategory::Model, "pivot.range", ValueKind::Range),
            (
                EndpointCategory::Parameter,
                "pivot.range",
                ValueKind::Scalar,
            ),
            (EndpointCategory::Model, "ui.menu.open", ValueKind::Bool),
            (EndpointCategory::Model, "vis.preset", ValueKind::Scalar),
            (
                EndpointCategory::Parameter,
                "player.output.volume",
                ValueKind::Scalar,
            ),
            (
                EndpointCategory::Telemetry,
                "player.output.levels",
                ValueKind::Stereo,
            ),
        ]
        .into_iter()
        .map(|(category, id, kind)| {
            (
                (category, EndpointId(id.to_owned())),
                EndpointDesc::new(kind),
            )
        })
        .collect();
        Self { endpoints }
    }
}

impl EndpointRegistry for CensusRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

/// The skin every row is drawn in: the built-in one, on the embedded faces so
/// a machine's own fonts cannot move a box.
pub(crate) fn skin() -> Skin {
    Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &SourceUri("fixture:census".to_owned()),
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the census skin must resolve: {error}"))
}

/// One census row's control, alone in a window it fills.
pub(crate) struct Fixture {
    kinds: CustomKinds,
    registry: CensusRegistry,
    resolver: MemResolver,
}

impl Fixture {
    pub(crate) fn new(control: &str) -> Self {
        let mut resolver = MemResolver::default();
        resolver.insert(
            LAYOUT,
            r#"(schema: "kithara.layout", version: 1, id: "fixture",
                root: Module(instance: "demo", source: "fixture.kmodule.ron",
                    size: (w: Fill, h: Fill)))"#,
        );
        resolver.insert(
            "fixture.kmodule.ron",
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "census", chrome: Plain,
                    root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0,
                        children: [{control}]))"#
            ),
        );
        for (name, body) in CENSUS_SOURCES {
            resolver.insert(name, body);
        }
        Self {
            kinds: census_kinds(),
            registry: CensusRegistry::default(),
            resolver,
        }
    }

    /// The row's document, compiled the way the retained host compiles it.
    pub(crate) fn compiled(&self) -> CompiledUi {
        compile(
            LAYOUT,
            &self.resolver,
            &self.registry,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::builder()
                .custom_kinds([CENSUS_KIND.to_owned()].into_iter().collect())
                .build(),
            &view::EMPTY,
        )
        .unwrap_or_else(|error| panic!("the census fixture must compile: {error}"))
    }

    /// The row mounted on the retained host.
    pub(crate) fn mount<'a>(&'a self, skin: &'a Skin) -> Ui<'a, Census<'a>> {
        Ui::new(
            Census::new(skin),
            Config::builder()
                .endpoints(&self.registry)
                .kinds(&self.kinds)
                .resolver(&self.resolver)
                .text(builtin::text_doc())
                .build(),
            (WIDTH, HEIGHT),
            1.0,
        )
        .unwrap_or_else(|error| panic!("the census fixture must mount: {error}"))
    }
}

/// An application standing in for the one a census row has none of: it reads
/// the census values and keeps what the document published to it.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Census<'a> {
    skin: &'a Skin,
    #[field(get, vis = "pub(crate)")]
    published: Vec<UiEvent>,
}

impl<'a> Census<'a> {
    pub(crate) const fn new(skin: &'a Skin) -> Self {
        Self {
            skin,
            published: Vec::new(),
        }
    }
}

impl Reads for Census<'_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        CensusReads.get(endpoint)
    }
}

impl App for Census<'_> {
    fn document(&self) -> &str {
        LAYOUT
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        self.skin
    }

    fn update(&mut self, event: UiEvent) {
        self.published.push(event);
    }
}
