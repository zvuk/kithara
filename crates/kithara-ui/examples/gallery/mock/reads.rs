use std::collections::{BTreeMap, BTreeSet};

use kithara_ui::{
    builtin,
    module::TrackColumn,
    render::{ControlAction, ReadValue, Reads, StereoLevels, TreeRow, WaveBucket, WaveformView},
};
use num_traits::cast::AsPrimitive;

use super::{
    clock::ClockState,
    consts::Consts,
    menu::{ContextState, MenuState},
    pivot::PivotState,
    quality::QualityState,
};
use crate::{
    mock_data::CATALOG,
    mock_mixer::MixerState,
    mock_stress::StressState,
    mock_transport::DeckTransport,
    sections::{ModuleDemo, Tab},
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct MockReads {
    tracklist_widths: BTreeMap<TrackColumn, f64>,
    collapsed: BTreeSet<String>,
    context: ContextState,
    clock: ClockState,
    transport: DeckTransport,
    menu: MenuState,
    pivot: PivotState,
    mixer: MixerState,
    #[field(get, vis = "pub(crate)", copy)]
    active_module: ModuleDemo,
    quality: QualityState,
    stress: StressState,
    #[field(set, vis = "pub(crate)")]
    library_query: String,
    #[field(get, vis = "pub(crate)", copy)]
    active_tab: Tab,
    tree_expanded: Vec<bool>,
    tree_rows: Vec<TreeRow<'static>>,
    tree_visible_indices: Vec<usize>,
    wave_beats: Vec<f32>,
    wave_downbeats: Vec<f32>,
    waveform: Vec<WaveBucket>,
    tracklist_columns: [bool; 9],
    vis_levels: [f32; 2],
    knobs: [f64; 4],
    button_cue: bool,
    button_play: bool,
    button_sync: bool,
    checkbox_off: bool,
    checkbox_on: bool,
    chip_active: bool,
    chip_inactive: bool,
    toggle_off: bool,
    toggle_on: bool,
    vis_phase: f32,
    levels_volume: f64,
    segmented_index: f64,
    vis_time_secs: f64,
    volume: f64,
    vis_rng: u32,
    library_scope: usize,
    tracklist_preset: usize,
    tree_selected: usize,
    vis_preset: usize,
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
            wave_beats,
            wave_downbeats,
            tree_expanded,
            tree_selected,
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
            clock: ClockState::default(),
            context: ContextState::default(),
            knobs: [0.35, 0.5, 0.65, 0.8],
            levels_volume: 0.7,
            library_query: String::new(),
            library_scope: 0,
            menu: MenuState::default(),
            pivot: PivotState::default(),
            mixer: MixerState::default(),
            quality: QualityState::default(),
            segmented_index: 2.0,
            stress: StressState::default(),
            toggle_off: false,
            toggle_on: true,
            volume: 0.7,
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
            tree_rows: Vec::with_capacity(CATALOG.tree.len()),
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
    fn activate(&mut self, path: &str) {
        if self.clock.activate(path) {
            return;
        }
        if self.pivot.activate(path) {
            return;
        }
        if self.menu.activate(path) {
            return;
        }
        if self.context.activate(path) {
            return;
        }
        if self.quality.activate(path) {
            return;
        }
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

    pub(crate) fn apply(&mut self, path: &str, action: &ControlAction) {
        match action {
            ControlAction::SetScalar(value) => self.set_scalar(path, *value),
            ControlAction::Activate => self.activate(path),
            ControlAction::SecondaryActivate => self.context.secondary(path),
            ControlAction::SelectIndex(index) => self.select_index(path, *index),
            ControlAction::StepScalar(steps) if path.contains("clock") => {
                self.clock.step(f64::from(*steps) * 0.01);
            }
            ControlAction::StepScalar(_) => {}
            _ => {}
        }
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

    fn reset_tracklist_columns(&mut self) {
        self.set_tracklist_preset(self.tracklist_preset);
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

    pub(crate) fn select_tab(&mut self, tab: Tab) {
        if self.active_tab != tab {
            self.stress.reset_clock();
        }
        self.active_tab = tab;
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

    fn set_scalar(&mut self, path: &str, value: f64) {
        if self.pivot.set_scalar(path, value) {
            return;
        }
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

    fn set_tracklist_width(&mut self, name: &str, value: f64) {
        let Some(column) = Consts::TRACK_COLUMNS
            .iter()
            .find(|column| column.endpoint_name() == name)
            .copied()
        else {
            return;
        };
        if value.is_finite() {
            let minimum = f64::from(builtin::skin().track_list.min_column_width);
            self.tracklist_widths.insert(column, value.max(minimum));
        }
    }

    fn shell(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            "gallery.tab.atoms" => self.active_tab == Tab::Atoms,
            "gallery.tab.buttons" => self.active_tab == Tab::Buttons,
            "gallery.tab.faders" => self.active_tab == Tab::Faders,
            "gallery.tab.modules" => self.active_tab == Tab::Modules,
            "gallery.tab.typography" => self.active_tab == Tab::Typography,
            "gallery.tab.cells" => self.active_tab == Tab::Cells,
            "gallery.tab.sizes" => self.active_tab == Tab::Sizes,
            "gallery.tab.tokens" => self.active_tab == Tab::Tokens,
            "gallery.tab.micro" => self.active_tab == Tab::Micro,
            "gallery.tab.mixer" => self.active_tab == Tab::Mixer,
            "gallery.tab.vis" => self.active_tab == Tab::Vis,
            "gallery.tab.chrome" => self.active_tab == Tab::Chrome,
            "gallery.tab.titlebars" => self.active_tab == Tab::Titlebars,
            "gallery.tab.tracklist" => self.active_tab == Tab::Tracklist,
            "gallery.tab.tree" => self.active_tab == Tab::Tree,
            "gallery.tab.library2" => self.active_tab == Tab::Library2,
            "gallery.tab.stress" => self.active_tab == Tab::Stress,
            "gallery.tab.menu" => self.active_tab == Tab::Menu,
            "gallery.tab.clock" => self.active_tab == Tab::Clock,
            "gallery.tab.pivot" => self.active_tab == Tab::Pivot,
            "gallery.module.deck" => self.active_module == ModuleDemo::Deck,
            "gallery.module.deck_micro" => self.active_module == ModuleDemo::DeckMicro,
            "gallery.module.global_bar" => self.active_module == ModuleDemo::GlobalBar,
            "gallery.module.telemetry" => self.active_module == ModuleDemo::Telemetry,
            "gallery.module.layout" => self.active_module == ModuleDemo::Layout,
            _ => return None,
        };
        Some(ReadValue::Bool(value))
    }

    pub(crate) fn tick(&mut self) {
        match self.active_tab {
            Tab::Stress => self.stress.tick(),
            Tab::Vis => self.tick_vis(),
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
            (left_noise / scale)
                .mul_add(0.14, self.vis_phase.sin().abs().mul_add(0.32, 0.42))
                .clamp(0.0, 1.0),
            (right_noise / scale)
                .mul_add(
                    0.12,
                    (self.vis_phase * 1.31).sin().abs().mul_add(0.29, 0.38),
                )
                .clamp(0.0, 1.0),
        ];
    }

    pub(crate) fn toggle_module(&mut self, module: String) {
        if !self.collapsed.remove(&module) {
            self.collapsed.insert(module);
        }
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
}

impl Reads for MockReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        // The menu axes are genuinely per-window, per-module and per-row, so
        // they answer the scoped key before it is dropped below.
        if let Some(value) = self.menu.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.context.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.quality.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.clock.get(
            endpoint,
            &self.transport,
            &self.waveform,
            &self.wave_beats,
            &self.wave_downbeats,
        ) {
            return Some(value);
        }
        if let Some(value) = self.pivot.get(endpoint) {
            return Some(value);
        }
        // The gallery hosts one virtual deck: every scope suffix resolves to
        // the same state, so the canonical `@scope` qualifier is dropped here.
        let endpoint = endpoint.split_once('@').map_or(endpoint, |(base, _)| base);
        if let Some(value) = self.mixer.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.stress.get(endpoint) {
            return Some(value);
        }
        if let Some(value) = self.shell(endpoint) {
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
            "mock.remain" | "deck.playback.remain" => ReadValue::Text(Consts::REMAIN),
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

#[cfg(test)]
mod tests;
