use std::collections::{BTreeMap, BTreeSet};

use kithara_ui::{
    ids::EndpointId,
    module::TrackColumn,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ControlAction, ReadValue, Reads, StereoLevels, TreeRow, WaveBucket, WaveformView},
};
use num_traits::cast::AsPrimitive;

use crate::{
    mock_data::CATALOG,
    mock_mixer::{self, MixerState},
    mock_stress::{self, StressState},
    mock_transport::DeckTransport,
    sections::{ModuleDemo, Tab},
};

struct Consts;

impl Consts {
    const BPM: &str = "70.00";
    const BPM_VALUE: f32 = 70.0;
    const CUES: &[f32] = &[0.27, 0.31];
    const DURATION_SECS: f64 = 360.0;
    const KEY: &str = "4m";
    const LOOP_REGION: [f32; 2] = [0.30, 0.34];
    const POSITION_SECS: f64 = 103.0;
    const REMAIN: &str = "−04:17";
    const TEMPO: &str = "+0.0%";
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
    const TRACKLIST_MICRO: [bool; 9] =
        [false, false, true, false, false, false, true, false, false];
    const TRACKLIST_QUEUE: [bool; 9] = [true, true, true, false, true, true, false, true, true];
    const TRACKLIST_QUEUE_PRESET: usize = 1;
    const VIS_TICK_SECS: f64 = 0.016;
    const WAVE_BUCKETS: u32 = 4_096;
    const ZOOM: f64 = 0.12;
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
    knobs: [f64; 4],
    levels_volume: f64,
    library_query: String,
    library_scope: usize,
    mixer: MixerState,
    segmented_index: f64,
    stress: StressState,
    toggle_off: bool,
    toggle_on: bool,
    volume: f64,
    wave_beats: Vec<f32>,
    wave_downbeats: Vec<f32>,
    waveform: Vec<WaveBucket>,
    transport: DeckTransport,
    tracklist_columns: [bool; 9],
    tracklist_preset: usize,
    tracklist_widths: BTreeMap<TrackColumn, f64>,
    tree_expanded: Vec<bool>,
    tree_rows: Vec<TreeRow<'static>>,
    tree_selected: usize,
    tree_visible_indices: Vec<usize>,
    vis_levels: [f32; 2],
    vis_phase: f32,
    vis_preset: usize,
    vis_time_secs: f64,
    vis_rng: u32,
}

impl Default for MockReads {
    fn default() -> Self {
        let (wave_beats, wave_downbeats) = beat_grid();
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
            knobs: [0.35, 0.5, 0.65, 0.8],
            levels_volume: 0.7,
            library_query: String::new(),
            library_scope: 0,
            mixer: MixerState::default(),
            segmented_index: 2.0,
            stress: StressState::default(),
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
            wave_beats,
            wave_downbeats,
            waveform: waveform(),
            transport: DeckTransport::new(
                Consts::BPM_VALUE,
                Consts::CUES,
                Consts::DURATION_SECS,
                Consts::LOOP_REGION,
                Consts::POSITION_SECS,
                Consts::ZOOM,
            ),
            tracklist_columns: Consts::TRACKLIST_QUEUE,
            tracklist_preset: Consts::TRACKLIST_QUEUE_PRESET,
            tracklist_widths: BTreeMap::new(),
            tree_expanded,
            tree_rows: Vec::with_capacity(CATALOG.tree.len()),
            tree_selected,
            tree_visible_indices: Vec::with_capacity(CATALOG.tree.len()),
            vis_levels: [0.66, 0.52],
            vis_phase: 0.0,
            vis_preset: 0,
            vis_time_secs: 0.0,
            vis_rng: 0x8a17_4c3d,
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
            self.stress.reset_clock();
        }
        self.active_tab = tab;
    }

    pub(super) fn tick(&mut self) {
        match self.active_tab {
            Tab::Stress => self.stress.tick(),
            Tab::Vis => self.tick_vis(),
            _ => {}
        }
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
        } else if path == "vis/shader" && index < CATALOG.vis_presets.len() {
            self.vis_preset = index;
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

    fn set_tracklist_width(&mut self, name: &str, value: f64) {
        let Some(column) = Consts::TRACK_COLUMNS
            .iter()
            .find(|column| column.endpoint_name() == name)
            .copied()
        else {
            return;
        };
        if value.is_finite() {
            let minimum = f64::from(kithara_ui::builtin::skin().track_list.min_column_width);
            self.tracklist_widths.insert(column, value.max(minimum));
        }
    }

    fn set_scalar(&mut self, path: &str, value: f64) {
        if self.mixer.set_scalar(path, value) {
            return;
        }
        if self.stress.set_scalar(path, value) {
            return;
        }
        if let Some((_, name)) = path.rsplit_once("/width/") {
            self.set_tracklist_width(name, value);
            return;
        }
        let value = value.clamp(0.0, 1.0);
        if path.ends_with("/loop_start") {
            self.transport.set_loop_start(value);
        } else if path.ends_with("/loop_end") {
            self.transport.set_loop_end(value);
        } else if path.ends_with("/zoom") {
            self.transport.set_zoom(value);
        } else if let Some(index) = match path {
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
        } else if path.ends_with("/wave") {
            self.transport.seek_normalized(value);
        }
    }

    fn activate(&mut self, path: &str) {
        if self.mixer.activate(path) {
            return;
        }
        if self.transport.activate(path) {
            return;
        }
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
            "vis/next" => self.vis_preset = (self.vis_preset + 1) % CATALOG.vis_presets.len(),
            "vis/previous" => {
                self.vis_preset =
                    (self.vis_preset + CATALOG.vis_presets.len() - 1) % CATALOG.vis_presets.len();
            }
            path if path.starts_with("tracklist/column-") => {
                self.toggle_tracklist_column(&path["tracklist/column-".len()..]);
            }
            path if path.ends_with("/transport/sync") => {
                self.button_sync = !self.button_sync;
            }
            path if path.ends_with("/play") => self.transport.toggle_play(),
            _ => {}
        }
    }

    fn tick_vis(&mut self) {
        self.vis_time_secs += Consts::VIS_TICK_SECS;
        self.vis_phase += 0.17;
        self.vis_rng = self
            .vis_rng
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let left_noise: f32 = (self.vis_rng >> 16).as_();
        let right_noise: f32 = (self.vis_rng & 0xffff).as_();
        let scale = f32::from(u16::MAX);
        self.vis_levels = [
            (0.42 + self.vis_phase.sin().abs() * 0.32 + left_noise / scale * 0.14).clamp(0.0, 1.0),
            (0.38 + (self.vis_phase * 1.31).sin().abs() * 0.29 + right_noise / scale * 0.12)
                .clamp(0.0, 1.0),
        ];
    }
}

impl Reads for MockReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if let Some(value) = self.mixer.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.stress.get(endpoint) {
            return Some(value);
        }
        if let Some(module) = endpoint
            .strip_prefix("ui.module.")
            .and_then(|value| value.strip_suffix(".collapsed"))
        {
            return Some(ReadValue::Bool(self.collapsed.contains(module)));
        }
        if let Some(name) = endpoint.strip_prefix("gallery.tracklist.columns.width.") {
            let column = Consts::TRACK_COLUMNS
                .iter()
                .find(|column| column.endpoint_name() == name)?;
            return self
                .tracklist_widths
                .get(column)
                .copied()
                .map(ReadValue::Scalar);
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
            "vis.badge" => ReadValue::Bool(true),
            "gallery.tab.atoms" => ReadValue::Bool(self.active_tab == Tab::Atoms),
            "gallery.tab.buttons" => ReadValue::Bool(self.active_tab == Tab::Buttons),
            "gallery.tab.faders" => ReadValue::Bool(self.active_tab == Tab::Faders),
            "gallery.tab.modules" => ReadValue::Bool(self.active_tab == Tab::Modules),
            "gallery.tab.typography" => ReadValue::Bool(self.active_tab == Tab::Typography),
            "gallery.tab.cells" => ReadValue::Bool(self.active_tab == Tab::Cells),
            "gallery.tab.sizes" => ReadValue::Bool(self.active_tab == Tab::Sizes),
            "gallery.tab.tokens" => ReadValue::Bool(self.active_tab == Tab::Tokens),
            "gallery.tab.micro" => ReadValue::Bool(self.active_tab == Tab::Micro),
            "gallery.tab.mixer" => ReadValue::Bool(self.active_tab == Tab::Mixer),
            "gallery.tab.vis" => ReadValue::Bool(self.active_tab == Tab::Vis),
            "gallery.tab.chrome" => ReadValue::Bool(self.active_tab == Tab::Chrome),
            "gallery.tab.titlebars" => ReadValue::Bool(self.active_tab == Tab::Titlebars),
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
            "vis.preset" => ReadValue::Scalar(self.vis_preset.as_()),
            "vis.time" => ReadValue::Scalar(self.vis_time_secs),
            "vis.preset_index" => ReadValue::Text(CATALOG.vis_indices[self.vis_preset]),
            "vis.preset_name" => ReadValue::Text(CATALOG.vis_presets[self.vis_preset]),
            "gallery.footer.deck" => ReadValue::Text("48kHz / 24bit"),
            "gallery.footer.deck_micro" => ReadValue::Text("READY"),
            "gallery.footer.global_bar" => ReadValue::Text("MASTER READY"),
            "gallery.footer.telemetry" => ReadValue::Text("LIVE"),
            "gallery.footer.layout" => ReadValue::Text("5 MODULES"),
            "gallery.footer.tokens_anatomy" => ReadValue::Text(CATALOG.footer_tokens_anatomy),
            "deck.playback.playing" => ReadValue::Bool(self.transport.playing()),
            "deck.playback.position_normalized" => {
                ReadValue::Scalar(self.transport.position_normalized())
            }
            "deck.playback.remaining_secs" => {
                ReadValue::Scalar(Consts::DURATION_SECS - self.transport.position_secs())
            }
            "deck.playback.position_secs" => ReadValue::Scalar(self.transport.position_secs()),
            "deck.playback.duration_secs" => ReadValue::Scalar(Consts::DURATION_SECS),
            "deck.playback.looping" => ReadValue::Bool(self.transport.loop_region().is_some()),
            "deck.playback.reverse" => ReadValue::Bool(self.transport.reverse()),
            "deck.playback.synced" | "mock.button.sync" => ReadValue::Bool(self.button_sync),
            "deck.playback.tempo" => ReadValue::Text(Consts::TEMPO),
            "deck.playback.waveform" => ReadValue::Waveform(WaveformView {
                buckets: &self.waveform,
                beats: &self.wave_beats,
                downbeats: &self.wave_downbeats,
                bpm: Some(Consts::BPM_VALUE),
                r#loop: self.transport.loop_region(),
                cues: self.transport.cues(),
            }),
            "deck.track.title" | "mock.track.title" => ReadValue::Text(CATALOG.title),
            "deck.track.source_kind" | "mock.track.artist" => ReadValue::Text(CATALOG.artist),
            "deck.track.key" | "mock.key" => ReadValue::Text(Consts::KEY),
            "deck.view.zoom" => ReadValue::Scalar(self.transport.zoom()),
            "player.output.levels" => ReadValue::Stereo(StereoLevels {
                l: if self.active_tab == Tab::Vis {
                    self.vis_levels[0]
                } else {
                    0.66
                },
                r: if self.active_tab == Tab::Vis {
                    self.vis_levels[1]
                } else {
                    0.52
                },
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
            "mock.cells.segmented" => ReadValue::Scalar(self.segmented_index),
            "gallery.tracklist.preset" => ReadValue::Scalar(self.tracklist_preset.as_()),
            _ => return None,
        };
        Some(value)
    }
}

fn waveform() -> Vec<WaveBucket> {
    let total: f32 = Consts::WAVE_BUCKETS.as_();
    (0..Consts::WAVE_BUCKETS)
        .map(|index| {
            let high: f32 = ((index * 41 + 23) % 55).as_();
            let low: f32 = ((index * 17) % 70).as_();
            let mid: f32 = ((index * 29 + 11) % 65).as_();
            let phase: f32 = index.as_();
            let phase = phase / total;
            let envelope =
                (phase * 44.0).sin().mul_add(0.3, 0.62) * (phase * 5.0).cos().mul_add(0.18, 0.82);
            WaveBucket {
                low: ((0.25 + low / 100.0) * envelope).clamp(0.0, 1.0),
                mid: ((0.18 + mid / 100.0) * envelope).clamp(0.0, 1.0),
                high: ((0.12 + high / 100.0) * envelope).clamp(0.0, 1.0),
            }
        })
        .collect()
}

fn beat_grid() -> (Vec<f32>, Vec<f32>) {
    let beat_count: usize = (Consts::DURATION_SECS * f64::from(Consts::BPM_VALUE) / 60.0)
        .floor()
        .as_();
    let beat_count_f: f32 = beat_count.as_();
    let beats: Vec<_> = (0..=beat_count)
        .map(|index| {
            let index: f32 = index.as_();
            index / beat_count_f
        })
        .collect();
    let downbeats = beats.iter().step_by(4).copied().collect();
    (beats, downbeats)
}

#[derive(Default)]
pub(super) struct MockRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MockRegistry {
    pub(super) fn insert(
        &mut self,
        category: EndpointCategory,
        id: &str,
        description: EndpointDesc,
    ) {
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

fn insert_deck_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("deck.transport.jump_back", ValueKind::Trigger),
        ("deck.transport.jump_forward", ValueKind::Trigger),
        ("deck.transport.set_cue", ValueKind::Trigger),
        ("deck.transport.toggle_loop", ValueKind::Trigger),
        ("deck.transport.toggle_play", ValueKind::Trigger),
        ("deck.transport.toggle_reverse", ValueKind::Trigger),
        ("deck.transport.toggle_sync", ValueKind::Trigger),
        ("deck.transport.seek_normalized", ValueKind::Scalar),
        ("deck.view.zoom_in", ValueKind::Trigger),
        ("deck.view.zoom_out", ValueKind::Trigger),
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
        ("deck.playback.looping", ValueKind::Bool),
        ("deck.playback.reverse", ValueKind::Bool),
        ("deck.playback.synced", ValueKind::Bool),
        ("deck.playback.tempo", ValueKind::Text),
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
}

pub(super) fn registry() -> impl EndpointRegistry {
    let mut registry = MockRegistry::default();
    insert_deck_endpoints(&mut registry);
    mock_mixer::insert_endpoints(&mut registry);
    mock_stress::insert_endpoints(&mut registry);
    insert_output_levels(&mut registry);
    for id in ["player.output.volume", "mock.cells.segmented", "vis.preset"] {
        registry.insert(
            EndpointCategory::Parameter,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    for id in ["vis.next", "vis.previous"] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
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
        "gallery.footer.tokens_anatomy",
        "vis.preset_index",
        "vis.preset_name",
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
        "gallery.tab.tokens",
        "gallery.tab.micro",
        "gallery.tab.mixer",
        "gallery.tab.vis",
        "gallery.tab.chrome",
        "gallery.tab.titlebars",
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
        "vis.badge",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }
    for id in [
        "deck.view.zoom",
        "mock.knob.26",
        "mock.knob.28",
        "mock.knob.34",
        "mock.knob.38",
        "mock.volume",
        "mock.cells.segmented",
        "vis.preset",
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
        registry.insert(
            EndpointCategory::Model,
            &format!("gallery.tracklist.columns.width.{}", column.endpoint_name()),
            EndpointDesc::new(ValueKind::Scalar),
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
    fn wave_zoom_is_host_owned_and_clamped() {
        let mut reads = MockReads::default();

        assert_eq!(
            reads.get("deck.view.zoom"),
            Some(ReadValue::Scalar(Consts::ZOOM))
        );
        reads.apply("modules/deck/wave/zoom", &ControlAction::SetScalar(0.001));
        assert_eq!(reads.get("deck.view.zoom"), Some(ReadValue::Scalar(0.015)));
        reads.apply("modules/deck/wave/zoom", &ControlAction::SetScalar(0.9));
        assert_eq!(reads.get("deck.view.zoom"), Some(ReadValue::Scalar(0.5)));
    }

    #[kithara::test]
    fn deck_sync_and_reverse_toggles_update_active_reads() {
        let mut reads = MockReads::default();

        assert_eq!(
            reads.get("deck.playback.synced"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            reads.get("deck.playback.reverse"),
            Some(ReadValue::Bool(false))
        );
        reads.apply("modules/deck/transport/sync", &ControlAction::Activate);
        reads.apply("modules/deck/transport/reverse", &ControlAction::Activate);
        assert_eq!(
            reads.get("deck.playback.synced"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            reads.get("deck.playback.reverse"),
            Some(ReadValue::Bool(true))
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
    fn vis_previous_and_next_cycle_the_preset() {
        let mut reads = MockReads::default();

        assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
        assert_eq!(
            reads.get("vis.preset_index"),
            Some(ReadValue::Text("1 / 3"))
        );

        reads.apply("vis/previous", &ControlAction::Activate);
        assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(2.0)));
        assert_eq!(
            reads.get("vis.preset_index"),
            Some(ReadValue::Text("3 / 3"))
        );

        reads.apply("vis/next", &ControlAction::Activate);
        assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
        reads.apply("vis/shader", &ControlAction::SelectIndex(1));
        assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(1.0)));
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
    fn tracklist_width_write_is_host_owned_and_clamped() {
        let mut reads = MockReads::default();
        let endpoint = "gallery.tracklist.columns.width.artist";

        assert_eq!(reads.get(endpoint), None);
        reads.apply(
            "tracklist/table/width/artist",
            &ControlAction::SetScalar(240.0),
        );
        assert_eq!(reads.get(endpoint), Some(ReadValue::Scalar(240.0)));

        reads.apply(
            "tracklist/table/width/artist",
            &ControlAction::SetScalar(1.0),
        );
        assert_eq!(
            reads.get(endpoint),
            Some(ReadValue::Scalar(f64::from(
                kithara_ui::builtin::skin().track_list.min_column_width
            )))
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
