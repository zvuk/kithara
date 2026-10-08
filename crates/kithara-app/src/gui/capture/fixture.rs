//! The reads every studio page is drawn against.
//!
//! One fixture answers every endpoint both hosts ask for, so the two sets
//! differ in how they draw and never in what they were given to draw.
use std::rc::Rc;

use kithara::ui::{
    app::App,
    module::IconName,
    registry::ValueKind,
    render::{
        PortalMapView, PortalTarget, ReadValue, Reads, ScalarRange, Scope, Skin, StereoLevels,
        TableCell, TableRow, TreeRow, UiEvent, WaveBucket, WaveformView,
    },
};
use kithara_app_library::Registration;

use crate::gui::{
    library::{Library, StartupSource},
    test_fixture,
    ui::{cache::DeckLayout, endpoints::readable_kind, package::Package},
};

fn startup() -> Vec<Registration> {
    vec![StartupSource::registered(vec![
        "/music/Midnight Signal.flac".to_owned(),
        "https://example.test/Parallel Lines.m3u8".to_owned(),
    ])]
}

pub(super) fn package() -> Result<Rc<Package>, String> {
    test_fixture::mount(None, startup())
        .map(|(package, _)| package)
        .map_err(|error| format!("package: {error}"))
}

pub(super) struct Fixture {
    layout: DeckLayout,
    package: Rc<Package>,
    library: Library,
    rows: Vec<TableRow<'static>>,
}

impl Fixture {
    const BPM: f32 = 124.0;
    const LEVELS: StereoLevels = StereoLevels {
        l: 0.58,
        r: 0.46,
        volume: 0.72,
    };
    const SCALAR: f64 = 0.5;

    pub(super) fn new(layout: DeckLayout, package: Rc<Package>) -> Result<Self, String> {
        let library =
            Library::new(startup(), package.text()).map_err(|error| format!("library: {error}"))?;
        Ok(Self {
            layout,
            package,
            library,
            rows: vec![
                TableRow::new(vec![TableCell::text("title", "Midnight Signal")], false),
                TableRow::new(vec![TableCell::text("title", "Parallel Lines")], false),
            ],
        })
    }

    /// The settings sheet is open in both captures, because a control only the
    /// sheet carries is compared across the hosts on no page otherwise. Its two
    /// sections cannot show at once, so each layout photographs one of them.
    fn on(&self, endpoint: &str) -> bool {
        match endpoint {
            "deck.eq.three_band" | "ui.settings.open" => true,
            "ui.settings.on_view" => self.layout == DeckLayout::Dual,
            "ui.settings.on_audio" => self.layout == DeckLayout::Single,
            _ => false,
        }
    }
}

impl Reads for Fixture {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        /// The startup branch, selected, so both hosts draw a row.
        const TREE: [TreeRow<'static>; 1] = [TreeRow {
            label: "Startup",
            count: Some(2),
            expanded: None,
            page: false,
            icon: IconName::Playlist,
            muted: false,
            selected: true,
            depth: 0,
        }];
        /// Two decks on the tempo axis. An empty target list would draw an
        /// axis with nothing on it and still pass the page's budget.
        const TEMPOS: [PortalTarget; 2] = [
            PortalTarget {
                bpm: Fixture::BPM,
                is_selected: true,
            },
            PortalTarget {
                bpm: 128.0,
                is_selected: false,
            },
        ];
        const WAVE: [WaveBucket; 8] = [
            WaveBucket {
                high: 0.3,
                low: -0.2,
                mid: 0.05,
            },
            WaveBucket {
                high: 0.6,
                low: -0.4,
                mid: 0.1,
            },
            WaveBucket {
                high: 0.4,
                low: -0.3,
                mid: 0.0,
            },
            WaveBucket {
                high: 0.8,
                low: -0.7,
                mid: 0.08,
            },
            WaveBucket {
                high: 0.5,
                low: -0.4,
                mid: -0.04,
            },
            WaveBucket {
                high: 0.7,
                low: -0.5,
                mid: 0.03,
            },
            WaveBucket {
                high: 0.35,
                low: -0.25,
                mid: 0.0,
            },
            WaveBucket {
                high: 0.55,
                low: -0.45,
                mid: 0.05,
            },
        ];

        let (base, _) = Scope::split(endpoint);
        if base == "library.page" {
            return self.library.page().map(ReadValue::Text);
        }
        let value = match readable_kind(base)? {
            ValueKind::Bool => ReadValue::Bool(self.on(base)),
            ValueKind::Scalar => ReadValue::Scalar(Self::SCALAR),
            ValueKind::Stereo => ReadValue::Stereo(Self::LEVELS),
            ValueKind::Text => ReadValue::Text(text(base)),
            ValueKind::Waveform => ReadValue::Waveform(WaveformView {
                buckets: &WAVE,
                revision: 0,
                beats: &[],
                cues: &[],
                downbeats: &[],
                unready: &[],
                bpm: Some(Self::BPM),
                r#loop: None,
            }),
            ValueKind::PortalMap => ReadValue::PortalMap(PortalMapView {
                master: Self::BPM,
                min: 60.0,
                max: 200.0,
                targets: &TEMPOS,
            }),
            ValueKind::Range => ReadValue::Range(ScalarRange {
                min: 0.25,
                max: 0.8,
            }),
            ValueKind::Table => ReadValue::Table(&self.rows),
            ValueKind::Tree => ReadValue::Tree(&TREE),
            _ => return None,
        };
        Some(value)
    }
}

fn text(endpoint: &str) -> &'static str {
    match endpoint {
        "deck.playback.bpm" => "124.0",
        "source.status" => "",
        "deck.playback.remain" => "-03:42",
        "deck.playback.tempo" => "+0.0%",
        "deck.stream.quality" => "320 kbps",
        "broadcast.url" => "OFF AIR",
        _ => "Fixture",
    }
}

impl App for Fixture {
    fn document(&self) -> &str {
        self.package.document(self.layout)
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        self.package.skin()
    }

    fn update(&mut self, _event: UiEvent) {}
}
