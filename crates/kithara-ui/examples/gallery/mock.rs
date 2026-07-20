use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::LazyLock,
};

use kithara_platform::time::Instant;
use kithara_ui::{
    ids::EndpointId,
    module::TrackColumn,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        ControlAction, ReadValue, Reads, StereoLevels, TrackRow, TreeIcon, TreeRow, WaveBucket,
        WaveformView,
    },
};
use num_traits::cast::AsPrimitive;
use serde::Deserialize;

struct Consts;

impl Consts {
    const BPM: &str = "70.00";
    const BPM_VALUE: f32 = 70.0;
    const DOWNBEATS: &[f32] = &[0.0, 0.5];
    const DURATION_SECS: f64 = 360.0;
    const FRAME_WINDOW: usize = 300;
    const KEY: &str = "4m";
    const POSITION_SECS: f64 = 103.0;
    const REMAIN: &str = "−04:17";
    const STRESS_WAVE_BUCKETS: u16 = 8_192;
    const TRACK_COLUMNS: [TrackColumn; 9] = [
        TrackColumn::Index,
        TrackColumn::Deck,
        TrackColumn::Title,
        TrackColumn::Artist,
        TrackColumn::Bpm,
        TrackColumn::Key,
        TrackColumn::Time,
        TrackColumn::Energy,
        TrackColumn::Transition,
    ];
    const TRACKLIST_LIBRARY: [bool; 9] = [true, true, true, true, true, true, true, false, false];
    const TRACKLIST_QUEUE: [bool; 9] = [true, true, true, false, true, true, false, true, true];
    const TRACKLIST_MICRO: [bool; 9] =
        [false, false, true, false, false, false, true, false, false];
    const TRACKLIST_QUEUE_PRESET: usize = 1;
    const WAVE_BEATS: &[f32] = &[0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];
}

#[derive(Deserialize)]
struct MockData {
    title: String,
    artist: String,
    breadcrumb: String,
    tree: Vec<MockTreeRow>,
    tracks: Vec<MockTrack>,
}

#[derive(Deserialize)]
struct MockTreeRow {
    #[serde(default)]
    depth: u8,
    label: String,
    icon: MockTreeIcon,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    expanded: Option<bool>,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    muted: bool,
}

#[derive(Clone, Copy, Deserialize)]
enum MockTreeIcon {
    Collection,
    Playlist,
    Folder,
    Plus,
    Zvuk,
    Search,
    Charts,
    Monitor,
    Home,
    Usb,
    Instrument,
    Waveform,
    Clock,
}

impl From<MockTreeIcon> for TreeIcon {
    fn from(value: MockTreeIcon) -> Self {
        match value {
            MockTreeIcon::Collection => Self::Collection,
            MockTreeIcon::Playlist => Self::Playlist,
            MockTreeIcon::Folder => Self::Folder,
            MockTreeIcon::Plus => Self::Plus,
            MockTreeIcon::Zvuk => Self::Zvuk,
            MockTreeIcon::Search => Self::Search,
            MockTreeIcon::Charts => Self::Charts,
            MockTreeIcon::Monitor => Self::Monitor,
            MockTreeIcon::Home => Self::Home,
            MockTreeIcon::Usb => Self::Usb,
            MockTreeIcon::Instrument => Self::Instrument,
            MockTreeIcon::Waveform => Self::Waveform,
            MockTreeIcon::Clock => Self::Clock,
        }
    }
}

#[derive(Deserialize)]
struct MockTrack {
    title: String,
    artist: String,
    time: String,
    search: String,
    deck: String,
    bpm: String,
    key: String,
    energy: u8,
    transition: String,
}

struct Catalog {
    title: &'static str,
    artist: &'static str,
    breadcrumb: &'static str,
    rows: &'static [TrackRow<'static>],
    tree: &'static [TreeRow<'static>],
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(load_catalog);

fn load_catalog() -> Catalog {
    let data: MockData = ron::from_str(include_str!("assets/mock-data.ron"))
        .expect("embedded gallery mock data must parse");
    let data: &'static MockData = Box::leak(Box::new(data));
    let rows: Vec<TrackRow<'static>> = data
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| TrackRow {
            title: &track.title,
            artist: Some(&track.artist),
            time: Some(&track.time),
            search: Some(&track.search),
            deck: Some(&track.deck),
            bpm: Some(&track.bpm),
            key: Some(&track.key),
            energy: Some(track.energy),
            transition: Some(&track.transition),
            current: index == 0,
            selected: index == 0,
        })
        .collect();
    let tree: Vec<TreeRow<'static>> = data
        .tree
        .iter()
        .map(|row| TreeRow {
            depth: row.depth,
            label: &row.label,
            icon: row.icon.into(),
            count: row.count,
            expanded: row.expanded,
            selected: row.selected,
            muted: row.muted,
        })
        .collect();
    Catalog {
        title: &data.title,
        artist: &data.artist,
        breadcrumb: &data.breadcrumb,
        rows: Box::leak(rows.into_boxed_slice()),
        tree: Box::leak(tree.into_boxed_slice()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tab {
    Atoms,
    Buttons,
    Faders,
    Modules,
    Typography,
    Cells,
    Sizes,
    Tracklist,
    Tree,
    Library2,
    Stress,
}

impl Tab {
    pub(super) const ALL: [Self; 11] = [
        Self::Atoms,
        Self::Buttons,
        Self::Faders,
        Self::Modules,
        Self::Typography,
        Self::Cells,
        Self::Sizes,
        Self::Tracklist,
        Self::Tree,
        Self::Library2,
        Self::Stress,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Atoms => "gallery-atoms.klayout.ron",
            Self::Buttons => "gallery-buttons.klayout.ron",
            Self::Faders => "gallery-faders.klayout.ron",
            Self::Modules => "gallery-modules.klayout.ron",
            Self::Typography => "gallery-typography.klayout.ron",
            Self::Cells => "gallery-cells.klayout.ron",
            Self::Sizes => "gallery-sizes.klayout.ron",
            Self::Tracklist => "gallery-tracklist.klayout.ron",
            Self::Tree => "gallery-tree.klayout.ron",
            Self::Library2 => "gallery-library2.klayout.ron",
            Self::Stress => "gallery-stress.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::Buttons => 1,
            Self::Faders => 2,
            Self::Modules => 3,
            Self::Typography => 4,
            Self::Cells => 5,
            Self::Sizes => 6,
            Self::Tracklist => 7,
            Self::Tree => 8,
            Self::Library2 => 9,
            Self::Stress => 10,
        }
    }
}

impl TryFrom<&str> for Tab {
    type Error = ();

    fn try_from(path: &str) -> Result<Self, ()> {
        match path {
            "gallery/atoms" => Ok(Self::Atoms),
            "gallery/buttons" => Ok(Self::Buttons),
            "gallery/faders" => Ok(Self::Faders),
            "gallery/modules" => Ok(Self::Modules),
            "gallery/typography" => Ok(Self::Typography),
            "gallery/cells" => Ok(Self::Cells),
            "gallery/sizes" => Ok(Self::Sizes),
            "gallery/tracklist" => Ok(Self::Tracklist),
            "gallery/tree" => Ok(Self::Tree),
            "gallery/library2" => Ok(Self::Library2),
            "gallery/stress" => Ok(Self::Stress),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModuleDemo {
    Deck,
    DeckMicro,
    GlobalBar,
    Telemetry,
    Layout,
}

impl ModuleDemo {
    pub(super) const ALL: [Self; 5] = [
        Self::Deck,
        Self::DeckMicro,
        Self::GlobalBar,
        Self::Telemetry,
        Self::Layout,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Deck => "gallery-modules.klayout.ron",
            Self::DeckMicro => "gallery-modules-deck-micro.klayout.ron",
            Self::GlobalBar => "gallery-modules-global-bar.klayout.ron",
            Self::Telemetry => "gallery-modules-telemetry.klayout.ron",
            Self::Layout => "gallery-modules-layout.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Deck => 0,
            Self::DeckMicro => 1,
            Self::GlobalBar => 2,
            Self::Telemetry => 3,
            Self::Layout => 4,
        }
    }
}

pub(super) struct MockReads {
    active_module: ModuleDemo,
    active_tab: Tab,
    button_cue: bool,
    button_play: bool,
    button_sync: bool,
    checkbox_off: bool,
    checkbox_on: bool,
    chip_active: bool,
    chip_inactive: bool,
    collapsed: BTreeSet<String>,
    frame_ms: VecDeque<f64>,
    frame_ms_ordered: Vec<f64>,
    frame_ms_avg: String,
    frame_ms_p99: String,
    fps: String,
    knobs: [f64; 4],
    last_tick: Option<Instant>,
    levels_volume: f64,
    library_query: String,
    library_scope: usize,
    playing: bool,
    position_secs: f64,
    segmented_index: f64,
    toggle_off: bool,
    toggle_on: bool,
    volume: f64,
    waveform: Vec<WaveBucket>,
    stress_fader: f64,
    stress_levels: [StereoLevels; 8],
    stress_phase: f32,
    stress_waveforms: [Vec<WaveBucket>; 4],
    tracklist_columns: [bool; 9],
    tracklist_preset: usize,
    tree_expanded: Vec<bool>,
    tree_rows: Vec<TreeRow<'static>>,
    tree_selected: usize,
    tree_visible_indices: Vec<usize>,
}

impl Default for MockReads {
    fn default() -> Self {
        let tree_expanded = CATALOG
            .tree
            .iter()
            .map(|row| row.expanded.unwrap_or(false))
            .collect();
        let tree_selected = CATALOG
            .tree
            .iter()
            .position(|row| row.selected)
            .unwrap_or_default();
        let mut reads = Self {
            active_module: ModuleDemo::Deck,
            active_tab: Tab::Atoms,
            button_cue: false,
            button_play: false,
            button_sync: true,
            checkbox_off: false,
            checkbox_on: true,
            chip_active: true,
            chip_inactive: false,
            collapsed: BTreeSet::new(),
            frame_ms: VecDeque::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_ordered: Vec::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_avg: "--".to_owned(),
            frame_ms_p99: "--".to_owned(),
            fps: "--".to_owned(),
            knobs: [0.35, 0.5, 0.65, 0.8],
            last_tick: None,
            levels_volume: 0.7,
            library_query: String::new(),
            library_scope: 0,
            playing: true,
            position_secs: Consts::POSITION_SECS,
            segmented_index: 2.0,
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
            waveform: waveform(),
            stress_fader: 0.7,
            stress_levels: [StereoLevels::default(); 8],
            stress_phase: 0.0,
            stress_waveforms: std::array::from_fn(stress_waveform),
            tracklist_columns: Consts::TRACKLIST_QUEUE,
            tracklist_preset: Consts::TRACKLIST_QUEUE_PRESET,
            tree_expanded,
            tree_rows: Vec::with_capacity(CATALOG.tree.len()),
            tree_selected,
            tree_visible_indices: Vec::with_capacity(CATALOG.tree.len()),
        };
        reads.rebuild_tree();
        reads
    }
}

impl MockReads {
    pub(super) const fn active_module(&self) -> ModuleDemo {
        self.active_module
    }

    pub(super) const fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub(super) fn select_tab(&mut self, tab: Tab) {
        if self.active_tab != tab {
            self.last_tick = None;
        }
        self.active_tab = tab;
    }

    pub(super) fn tick(&mut self) {
        if self.active_tab != Tab::Stress {
            return;
        }
        let now = Instant::now();
        if let Some(previous) = self.last_tick.replace(now) {
            self.record_frame(now.duration_since(previous).as_secs_f64() * 1_000.0);
        }
        self.push_stress_data();
    }

    pub(super) fn set_library_query(&mut self, query: String) {
        self.library_query = query;
    }

    pub(super) fn toggle_module(&mut self, module: String) {
        if !self.collapsed.remove(&module) {
            self.collapsed.insert(module);
        }
    }

    pub(super) fn apply(&mut self, path: &str, action: &ControlAction) {
        match action {
            ControlAction::SetScalar(value) => self.set_scalar(path, *value),
            ControlAction::Activate => self.activate(path),
            ControlAction::SelectIndex(index) => self.select_index(path, *index),
            _ => {}
        }
    }

    fn select_index(&mut self, path: &str, index: usize) {
        if path == "cells/beat" {
            self.segmented_index = index.as_();
        } else if path == "tracklist/column-preset" {
            self.set_tracklist_preset(index);
        } else if path == "library2/context" {
            self.library_scope = index;
        } else if matches!(path, "tree/browser" | "library2/browser") {
            self.select_tree_row(index);
        }
    }

    fn select_tree_row(&mut self, index: usize) {
        let Some(base_index) = self.tree_visible_indices.get(index).copied() else {
            return;
        };
        let row = CATALOG.tree[base_index];
        if row.muted {
            return;
        }
        if row.expanded.is_some() {
            self.tree_expanded[base_index] = !self.tree_expanded[base_index];
        } else {
            self.tree_selected = base_index;
        }
        self.rebuild_tree();
    }

    fn rebuild_tree(&mut self) {
        self.tree_rows.clear();
        self.tree_visible_indices.clear();
        let mut ancestors = Vec::new();
        for (index, base) in CATALOG.tree.iter().copied().enumerate() {
            let depth = usize::from(base.depth);
            ancestors.truncate(depth);
            let visible = ancestors.iter().all(|expanded| *expanded);
            if visible {
                self.tree_rows.push(TreeRow {
                    expanded: base.expanded.map(|_| self.tree_expanded[index]),
                    selected: index == self.tree_selected,
                    ..base
                });
                self.tree_visible_indices.push(index);
            }
            if base.expanded.is_some() {
                ancestors.push(self.tree_expanded[index]);
            }
        }
    }

    fn set_tracklist_preset(&mut self, index: usize) {
        let Some(columns) = [
            Consts::TRACKLIST_LIBRARY,
            Consts::TRACKLIST_QUEUE,
            Consts::TRACKLIST_MICRO,
        ]
        .get(index)
        .copied() else {
            return;
        };
        self.tracklist_preset = index;
        self.tracklist_columns = columns;
    }

    fn reset_tracklist_columns(&mut self) {
        self.set_tracklist_preset(self.tracklist_preset);
    }

    fn toggle_tracklist_column(&mut self, name: &str) {
        let Some(index) = Consts::TRACK_COLUMNS
            .iter()
            .position(|column| column.endpoint_name() == name)
        else {
            return;
        };
        self.tracklist_columns[index] = !self.tracklist_columns[index];
    }

    fn set_scalar(&mut self, path: &str, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if let Some(index) = match path {
            "atoms/knobs/size-26" => Some(0),
            "atoms/knobs/size-28" => Some(1),
            "atoms/knobs/size-34" => Some(2),
            "atoms/knobs/size-38" => Some(3),
            _ => None,
        } {
            self.knobs[index] = value;
        } else if path.starts_with("atoms/meters/") {
            self.levels_volume = value;
        } else if path.starts_with("faders/") || path.ends_with("/volume") {
            self.volume = value;
        } else if path == "stress/master" {
            self.stress_fader = value;
        } else if path.ends_with("/wave") {
            self.position_secs = value * Consts::DURATION_SECS;
        }
    }

    fn activate(&mut self, path: &str) {
        match path {
            "modules-tabs/deck" => self.active_module = ModuleDemo::Deck,
            "modules-tabs/deck-micro" => self.active_module = ModuleDemo::DeckMicro,
            "modules-tabs/global-bar" => self.active_module = ModuleDemo::GlobalBar,
            "modules-tabs/telemetry" => self.active_module = ModuleDemo::Telemetry,
            "modules-tabs/layout" => self.active_module = ModuleDemo::Layout,
            "atoms/toggles/toggle-on" | "cells/toggle-on" => self.toggle_on = !self.toggle_on,
            "atoms/toggles/toggle-off" | "cells/toggle-off" => self.toggle_off = !self.toggle_off,
            "atoms/toggles/checkbox-on" | "cells/checkbox-on" => {
                self.checkbox_on = !self.checkbox_on;
            }
            "atoms/toggles/checkbox-off" | "cells/checkbox-off" => {
                self.checkbox_off = !self.checkbox_off;
            }
            "atoms/chips/active" => self.chip_active = !self.chip_active,
            "atoms/chips/inactive" => self.chip_inactive = !self.chip_inactive,
            "buttons/play" => self.button_play = !self.button_play,
            "buttons/cue" => self.button_cue = !self.button_cue,
            "buttons/sync" => self.button_sync = !self.button_sync,
            "tracklist/reset-columns" => self.reset_tracklist_columns(),
            path if path.starts_with("tracklist/column-") => {
                self.toggle_tracklist_column(&path["tracklist/column-".len()..]);
            }
            path if path.ends_with("/play") => self.playing = !self.playing,
            _ => {}
        }
    }
}

impl Reads for MockReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if let Some(module) = endpoint
            .strip_prefix("ui.module.")
            .and_then(|value| value.strip_suffix(".collapsed"))
        {
            return Some(ReadValue::Bool(self.collapsed.contains(module)));
        }
        if let Some(name) = endpoint.strip_prefix("gallery.tracklist.columns.") {
            let index = Consts::TRACK_COLUMNS
                .iter()
                .position(|column| column.endpoint_name() == name)?;
            return Some(ReadValue::Bool(self.tracklist_columns[index]));
        }
        let value = match endpoint {
            "gallery.label.knobs" => ReadValue::Text("KNOB · 26 / 28 / 34 / 38"),
            "gallery.label.meters" => ReadValue::Text("VU · STEREO / VERTICAL"),
            "gallery.label.toggles" => ReadValue::Text("TOGGLE / CHECKBOX"),
            "gallery.label.readouts" => ReadValue::Text("READOUT"),
            "gallery.label.chips" => ReadValue::Text("CHIP"),
            "gallery.label.transport" => ReadValue::Text("TRANSPORT BUTTONS"),
            "gallery.label.regular" => ReadValue::Text("BUTTON STYLES"),
            "gallery.label.text" => ReadValue::Text("TEXT STYLES"),
            "gallery.label.faders" => ReadValue::Text("HORIZONTAL FADERS"),
            "gallery.label.scalar" => ReadValue::Text("SCALAR TELEMETRY"),
            "gallery.tab.atoms" => ReadValue::Bool(self.active_tab == Tab::Atoms),
            "gallery.tab.buttons" => ReadValue::Bool(self.active_tab == Tab::Buttons),
            "gallery.tab.faders" => ReadValue::Bool(self.active_tab == Tab::Faders),
            "gallery.tab.modules" => ReadValue::Bool(self.active_tab == Tab::Modules),
            "gallery.tab.typography" => ReadValue::Bool(self.active_tab == Tab::Typography),
            "gallery.tab.cells" => ReadValue::Bool(self.active_tab == Tab::Cells),
            "gallery.tab.sizes" => ReadValue::Bool(self.active_tab == Tab::Sizes),
            "gallery.tab.tracklist" => ReadValue::Bool(self.active_tab == Tab::Tracklist),
            "gallery.tab.tree" => ReadValue::Bool(self.active_tab == Tab::Tree),
            "gallery.tab.library2" => ReadValue::Bool(self.active_tab == Tab::Library2),
            "gallery.tab.stress" => ReadValue::Bool(self.active_tab == Tab::Stress),
            "gallery.module.deck" => ReadValue::Bool(self.active_module == ModuleDemo::Deck),
            "gallery.module.deck_micro" => {
                ReadValue::Bool(self.active_module == ModuleDemo::DeckMicro)
            }
            "gallery.module.global_bar" => {
                ReadValue::Bool(self.active_module == ModuleDemo::GlobalBar)
            }
            "gallery.module.telemetry" => {
                ReadValue::Bool(self.active_module == ModuleDemo::Telemetry)
            }
            "gallery.module.layout" => ReadValue::Bool(self.active_module == ModuleDemo::Layout),
            "gallery.footer.deck" => ReadValue::Text("48kHz / 24bit"),
            "gallery.footer.deck_micro" => ReadValue::Text("READY"),
            "gallery.footer.global_bar" => ReadValue::Text("MASTER READY"),
            "gallery.footer.telemetry" => ReadValue::Text("LIVE"),
            "gallery.footer.layout" => ReadValue::Text("5 MODULES"),
            "bench.fps" => ReadValue::Text(&self.fps),
            "bench.frame_ms_avg" => ReadValue::Text(&self.frame_ms_avg),
            "bench.frame_ms_p99" => ReadValue::Text(&self.frame_ms_p99),
            "bench.fader" => ReadValue::Scalar(self.stress_fader),
            "bench.wave.0" => stress_waveform_value(&self.stress_waveforms[0]),
            "bench.wave.1" => stress_waveform_value(&self.stress_waveforms[1]),
            "bench.wave.2" => stress_waveform_value(&self.stress_waveforms[2]),
            "bench.wave.3" => stress_waveform_value(&self.stress_waveforms[3]),
            "bench.level.0" => ReadValue::Stereo(self.stress_levels[0]),
            "bench.level.1" => ReadValue::Stereo(self.stress_levels[1]),
            "bench.level.2" => ReadValue::Stereo(self.stress_levels[2]),
            "bench.level.3" => ReadValue::Stereo(self.stress_levels[3]),
            "bench.level.4" => ReadValue::Stereo(self.stress_levels[4]),
            "bench.level.5" => ReadValue::Stereo(self.stress_levels[5]),
            "bench.level.6" => ReadValue::Stereo(self.stress_levels[6]),
            "bench.level.7" => ReadValue::Stereo(self.stress_levels[7]),
            "deck.playback.playing" => ReadValue::Bool(self.playing),
            "deck.playback.position_normalized" => {
                ReadValue::Scalar(self.position_secs / Consts::DURATION_SECS)
            }
            "deck.playback.remaining_secs" => {
                ReadValue::Scalar(Consts::DURATION_SECS - self.position_secs)
            }
            "deck.playback.position_secs" => ReadValue::Scalar(self.position_secs),
            "deck.playback.duration_secs" => ReadValue::Scalar(Consts::DURATION_SECS),
            "deck.playback.waveform" => ReadValue::Waveform(WaveformView {
                buckets: &self.waveform,
                beats: Consts::WAVE_BEATS,
                downbeats: Consts::DOWNBEATS,
                bpm: Some(Consts::BPM_VALUE),
            }),
            "deck.track.title" | "mock.track.title" => ReadValue::Text(CATALOG.title),
            "deck.track.source_kind" | "mock.track.artist" => ReadValue::Text(CATALOG.artist),
            "deck.track.key" | "mock.key" => ReadValue::Text(Consts::KEY),
            "player.output.levels" => ReadValue::Stereo(StereoLevels {
                l: 0.66,
                r: 0.52,
                volume: self.volume.as_(),
            }),
            "player.output.volume" | "mock.volume" => ReadValue::Scalar(self.volume),
            "library.visible_tracks" => ReadValue::TrackList(CATALOG.rows),
            "library.tree" => ReadValue::Tree(&self.tree_rows),
            "library.breadcrumb" => ReadValue::Text(CATALOG.breadcrumb),
            "library.query" => ReadValue::Text(&self.library_query),
            "library.scope" => ReadValue::Scalar(self.library_scope.as_()),
            "ui.preset" => ReadValue::Text("player"),
            "mock.bpm" => ReadValue::Text(Consts::BPM),
            "mock.remain" => ReadValue::Text(Consts::REMAIN),
            "mock.knob.26" => ReadValue::Scalar(self.knobs[0]),
            "mock.knob.28" => ReadValue::Scalar(self.knobs[1]),
            "mock.knob.34" => ReadValue::Scalar(self.knobs[2]),
            "mock.knob.38" => ReadValue::Scalar(self.knobs[3]),
            "mock.levels" => ReadValue::Stereo(StereoLevels {
                l: 0.66,
                r: 0.52,
                volume: self.levels_volume.as_(),
            }),
            "mock.toggle.on" => ReadValue::Bool(self.toggle_on),
            "mock.toggle.off" => ReadValue::Bool(self.toggle_off),
            "mock.checkbox.on" => ReadValue::Bool(self.checkbox_on),
            "mock.checkbox.off" => ReadValue::Bool(self.checkbox_off),
            "mock.chip.active" => ReadValue::Bool(self.chip_active),
            "mock.chip.inactive" => ReadValue::Bool(self.chip_inactive),
            "mock.button.play" => ReadValue::Bool(self.button_play),
            "mock.button.cue" => ReadValue::Bool(self.button_cue),
            "mock.button.sync" => ReadValue::Bool(self.button_sync),
            "mock.cells.segmented" => ReadValue::Scalar(self.segmented_index),
            "gallery.tracklist.preset" => ReadValue::Scalar(self.tracklist_preset.as_()),
            _ => return None,
        };
        Some(value)
    }
}

impl MockReads {
    fn record_frame(&mut self, frame_ms: f64) {
        if self.frame_ms.len() == Consts::FRAME_WINDOW {
            self.frame_ms.pop_front();
        }
        self.frame_ms.push_back(frame_ms);
        let count = u32::try_from(self.frame_ms.len()).map_or(1.0, f64::from);
        let average = self.frame_ms.iter().sum::<f64>() / count;
        self.frame_ms_ordered.clear();
        self.frame_ms_ordered.extend(self.frame_ms.iter().copied());
        self.frame_ms_ordered.sort_by(f64::total_cmp);
        let percentile = self
            .frame_ms_ordered
            .len()
            .saturating_mul(99)
            .div_ceil(100)
            .saturating_sub(1);
        let Some(p99) = self.frame_ms_ordered.get(percentile).copied() else {
            return;
        };
        self.fps = format!("{:.1}", 1_000.0 / average);
        self.frame_ms_avg = format!("{average:.2}");
        self.frame_ms_p99 = format!("{p99:.2}");
    }

    fn push_stress_data(&mut self) {
        self.stress_phase += 0.037;
        for (index, waveform) in self.stress_waveforms.iter_mut().enumerate() {
            waveform.rotate_left(1);
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            if let Some(bucket) = waveform.last_mut() {
                *bucket = stress_bucket(self.stress_phase + offset * 0.71);
            }
        }
        for (index, levels) in self.stress_levels.iter_mut().enumerate() {
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            let carrier = (self.stress_phase * 2.3 + offset * 0.47).sin();
            let noise = (self.stress_phase * 31.7 + offset * 7.13).sin();
            levels.l = (carrier.mul_add(0.32, noise * 0.08 + 0.54)).clamp(0.0, 1.0);
            levels.r = ((carrier + 0.63).sin().mul_add(0.3, noise * 0.09 + 0.5)).clamp(0.0, 1.0);
            levels.volume = self.stress_fader.as_();
        }
    }
}

fn waveform() -> Vec<WaveBucket> {
    (0_u16..160)
        .map(|index| WaveBucket {
            low: 0.25 + f32::from((index * 17) % 70) / 100.0,
            mid: 0.18 + f32::from((index * 29 + 11) % 65) / 100.0,
            high: 0.12 + f32::from((index * 41 + 23) % 55) / 100.0,
        })
        .collect()
}

fn stress_waveform(index: usize) -> Vec<WaveBucket> {
    let offset = u16::try_from(index).map_or(0.0, f32::from);
    (0..Consts::STRESS_WAVE_BUCKETS)
        .map(|bucket| stress_bucket(f32::from(bucket).mul_add(0.013, offset)))
        .collect()
}

fn stress_bucket(phase: f32) -> WaveBucket {
    WaveBucket {
        low: phase.sin().mul_add(0.34, 0.52).clamp(0.0, 1.0),
        mid: (phase * 1.73).sin().mul_add(0.29, 0.45).clamp(0.0, 1.0),
        high: (phase * 3.11).sin().mul_add(0.2, 0.34).clamp(0.0, 1.0),
    }
}

fn stress_waveform_value(waveform: &[WaveBucket]) -> ReadValue<'_> {
    ReadValue::Waveform(WaveformView {
        buckets: waveform,
        beats: &[],
        downbeats: &[],
        bpm: None,
    })
}

#[derive(Default)]
struct MockRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MockRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for MockRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

fn insert_output_levels(registry: &mut MockRegistry) {
    registry.insert(
        EndpointCategory::Telemetry,
        "player.output.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
}

pub(super) fn registry() -> impl EndpointRegistry {
    let mut registry = MockRegistry::default();
    for (id, kind) in [
        ("deck.transport.toggle_play", ValueKind::Trigger),
        ("deck.transport.prev", ValueKind::Trigger),
        ("deck.transport.next", ValueKind::Trigger),
        ("deck.transport.seek_normalized", ValueKind::Scalar),
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    for (id, kind) in [
        ("deck.playback.playing", ValueKind::Bool),
        ("deck.playback.position_normalized", ValueKind::Scalar),
        ("deck.playback.remaining_secs", ValueKind::Scalar),
        ("deck.playback.position_secs", ValueKind::Scalar),
        ("deck.playback.duration_secs", ValueKind::Scalar),
        ("deck.playback.waveform", ValueKind::Waveform),
        ("deck.track.title", ValueKind::Text),
        ("deck.track.source_kind", ValueKind::Text),
        ("deck.track.key", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    insert_output_levels(&mut registry);
    for id in ["player.output.volume", "mock.cells.segmented"] {
        registry.insert(
            EndpointCategory::Parameter,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    insert_library_endpoints(&mut registry);
    for id in [
        "gallery.label.knobs",
        "gallery.label.meters",
        "gallery.label.toggles",
        "gallery.label.readouts",
        "gallery.label.chips",
        "gallery.label.transport",
        "gallery.label.regular",
        "gallery.label.text",
        "gallery.label.faders",
        "gallery.label.scalar",
        "mock.track.title",
        "mock.track.artist",
        "mock.bpm",
        "mock.key",
        "mock.remain",
        "gallery.footer.deck",
        "gallery.footer.deck_micro",
        "gallery.footer.global_bar",
        "gallery.footer.telemetry",
        "gallery.footer.layout",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    for id in [
        "gallery.tab.atoms",
        "gallery.tab.buttons",
        "gallery.tab.faders",
        "gallery.tab.modules",
        "gallery.tab.typography",
        "gallery.tab.cells",
        "gallery.tab.sizes",
        "gallery.tab.tracklist",
        "gallery.tab.tree",
        "gallery.tab.library2",
        "gallery.tab.stress",
        "gallery.module.deck",
        "gallery.module.deck_micro",
        "gallery.module.global_bar",
        "gallery.module.telemetry",
        "gallery.module.layout",
        "mock.toggle.on",
        "mock.toggle.off",
        "mock.checkbox.on",
        "mock.checkbox.off",
        "mock.chip.active",
        "mock.chip.inactive",
        "mock.button.play",
        "mock.button.cue",
        "mock.button.sync",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }
    for id in [
        "mock.knob.26",
        "mock.knob.28",
        "mock.knob.34",
        "mock.knob.38",
        "mock.volume",
        "mock.cells.segmented",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    insert_tracklist_endpoints(&mut registry);
    registry.insert(
        EndpointCategory::Model,
        "mock.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    for id in ["bench.fps", "bench.frame_ms_avg", "bench.frame_ms_p99"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    registry.insert(
        EndpointCategory::Model,
        "bench.fader",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for index in 0..4 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.wave.{index}"),
            EndpointDesc::new(ValueKind::Waveform),
        );
    }
    for index in 0..8 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.level.{index}"),
            EndpointDesc::new(ValueKind::Stereo),
        );
    }
    registry
}

fn insert_tracklist_endpoints(registry: &mut MockRegistry) {
    registry.insert(
        EndpointCategory::Model,
        "gallery.tracklist.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for column in Consts::TRACK_COLUMNS {
        registry.insert(
            EndpointCategory::Model,
            &format!("gallery.tracklist.columns.{}", column.endpoint_name()),
            EndpointDesc::new(ValueKind::Bool),
        );
    }
}

fn insert_library_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("library.visible_tracks", ValueKind::TrackList),
        ("library.tree", ValueKind::Tree),
        ("library.breadcrumb", ValueKind::Text),
        ("library.query", ValueKind::Text),
        ("library.scope", ValueKind::Scalar),
        ("ui.preset", ValueKind::Text),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn visible_tree_row_selected(reads: &MockReads, label: &str) -> bool {
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };
        rows.iter()
            .find(|row| row.label == label)
            .map(|row| row.selected)
            .unwrap_or_else(|| panic!("missing visible tree row {label}"))
    }

    fn visible_tree_row_index(reads: &MockReads, label: &str) -> usize {
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };
        rows.iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("missing visible tree row {label}"))
    }

    fn selected_visible_index(reads: &MockReads) -> usize {
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };
        rows.iter()
            .position(|row| row.selected)
            .expect("a selected tree row")
    }

    fn muted_visible_index(reads: &MockReads) -> usize {
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };
        rows.iter()
            .position(|row| row.muted)
            .expect("a muted tree row")
    }

    fn visible_tree_len(reads: &MockReads) -> usize {
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };
        rows.len()
    }

    #[kithara::test]
    fn wave_scalar_write_updates_normalized_playback_position() {
        let mut reads = MockReads::default();

        reads.apply("modules/deck/wave", &ControlAction::SetScalar(0.25));

        assert_eq!(
            reads.get("deck.playback.position_normalized"),
            Some(ReadValue::Scalar(0.25))
        );
        assert_eq!(
            reads.get("deck.playback.position_secs"),
            Some(ReadValue::Scalar(Consts::DURATION_SECS * 0.25))
        );
    }

    #[kithara::test]
    fn toggle_module_owns_the_collapsed_read_endpoint() {
        let mut reads = MockReads::default();
        let endpoint = "ui.module.gallery-module-deck.collapsed";

        assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(false)));
        reads.toggle_module("gallery-module-deck".to_owned());
        assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(true)));
        reads.toggle_module("gallery-module-deck".to_owned());
        assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(false)));
    }

    #[kithara::test]
    fn knob_gallery_values_cover_both_sides_of_center() {
        let mut reads = MockReads::default();

        assert_eq!(reads.get("mock.knob.26"), Some(ReadValue::Scalar(0.35)));
        assert_eq!(reads.get("mock.knob.28"), Some(ReadValue::Scalar(0.5)));
        assert_eq!(reads.get("mock.knob.34"), Some(ReadValue::Scalar(0.65)));
        assert_eq!(reads.get("mock.knob.38"), Some(ReadValue::Scalar(0.8)));

        reads.apply("atoms/knobs/size-26", &ControlAction::SetScalar(0.45));
        assert_eq!(reads.get("mock.knob.26"), Some(ReadValue::Scalar(0.45)));
        assert_eq!(reads.get("mock.knob.38"), Some(ReadValue::Scalar(0.8)));
    }

    #[kithara::test]
    fn segmented_gallery_selects_an_index() {
        let mut reads = MockReads::default();

        assert_eq!(
            reads.get("mock.cells.segmented"),
            Some(ReadValue::Scalar(2.0))
        );
        reads.apply("cells/beat", &ControlAction::SelectIndex(3));
        assert_eq!(
            reads.get("mock.cells.segmented"),
            Some(ReadValue::Scalar(3.0))
        );
    }

    #[kithara::test]
    fn tracklist_presets_replace_host_owned_column_visibility() {
        let mut reads = MockReads::default();

        assert_eq!(
            reads.get("gallery.tracklist.columns.energy"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            reads.get("gallery.tracklist.columns.artist"),
            Some(ReadValue::Bool(false))
        );

        reads.apply("tracklist/column-preset", &ControlAction::SelectIndex(0));

        assert_eq!(
            reads.get("gallery.tracklist.columns.energy"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            reads.get("gallery.tracklist.columns.artist"),
            Some(ReadValue::Bool(true))
        );
    }

    #[kithara::test]
    fn tracklist_reset_restores_current_preset_defaults() {
        let mut reads = MockReads::default();

        reads.apply("tracklist/column-energy", &ControlAction::Activate);
        assert_eq!(
            reads.get("gallery.tracklist.columns.energy"),
            Some(ReadValue::Bool(false))
        );

        reads.apply("tracklist/reset-columns", &ControlAction::Activate);

        assert_eq!(
            reads.get("gallery.tracklist.columns.energy"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            reads.get("gallery.tracklist.preset"),
            Some(ReadValue::Scalar(1.0))
        );
    }

    #[kithara::test]
    fn tree_branch_selection_toggles_visible_descendants() {
        let mut reads = MockReads::default();
        let before = visible_tree_len(&reads);
        let explorer = visible_tree_row_index(&reads, "Explorer");

        reads.apply("tree/browser", &ControlAction::SelectIndex(explorer));
        let collapsed = visible_tree_len(&reads);
        assert!(collapsed < before);

        reads.apply("tree/browser", &ControlAction::SelectIndex(explorer));
        assert_eq!(visible_tree_len(&reads), before);
    }

    #[kithara::test]
    fn tree_leaf_selection_is_host_owned() {
        let mut reads = MockReads::default();
        let previous = selected_visible_index(&reads);
        let all_tracks = visible_tree_row_index(&reads, "All tracks");
        assert_ne!(previous, all_tracks);

        reads.apply("library2/browser", &ControlAction::SelectIndex(all_tracks));

        assert!(visible_tree_row_selected(&reads, "All tracks"));
        assert_eq!(selected_visible_index(&reads), all_tracks);
    }

    #[kithara::test]
    fn muted_tree_row_does_not_change_selection() {
        let mut reads = MockReads::default();
        let muted = muted_visible_index(&reads);
        let selected = selected_visible_index(&reads);

        reads.apply("tree/browser", &ControlAction::SelectIndex(muted));

        assert_eq!(selected_visible_index(&reads), selected);
    }

    #[kithara::test]
    fn expanded_mock_tree_overflows_the_gallery_viewport() {
        let reads = MockReads::default();
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("expected tree rows");
        };

        assert!(rows.len() >= 30, "visible rows: {}", rows.len());
    }

    #[kithara::test]
    fn library_query_read_reflects_host_updates() {
        let mut reads = MockReads::default();

        reads.set_library_query("acid bass".to_owned());

        assert_eq!(
            reads.get("library.query"),
            Some(ReadValue::Text("acid bass"))
        );
    }

    #[kithara::test]
    fn context_scope_selection_is_host_owned() {
        let mut reads = MockReads::default();

        assert_eq!(reads.get("library.scope"), Some(ReadValue::Scalar(0.0)));
        reads.apply("library2/context", &ControlAction::SelectIndex(1));
        assert_eq!(reads.get("library.scope"), Some(ReadValue::Scalar(1.0)));
    }

    #[kithara::test]
    fn breadcrumb_data_excludes_the_scope_prefix() {
        assert!(!CATALOG.breadcrumb.is_empty());
        assert!(!CATALOG.breadcrumb.contains('\u{203a}'));
    }
}
